//! R60 — the developer/admin-only HULL DESIGN editor.
//!
//! A full-screen egui screen (entered from the dev panel) to AUTHOR a [`Hull`]: paint cells with their
//! sub-cell [`CellShape`] on a grid, place/edit slots (hardpoints), set name/class/role/budgets, and
//! create NEW hulls or edit existing ones. A live render-to-texture 3-D PREVIEW pane (see `preview.rs`)
//! shows the hull as you edit. "Apply to live" pushes the design into the running game's `HullCatalog`;
//! "Save → ships.ron" persists it (via [`tuning_io::save_catalogs`](crate::tuning_io::save_catalogs)).
//!
//! Dev-only (`#[cfg(feature = "dev_panel")]`) + windowed-client only → determinism-neutral; the only
//! durable effect is the explicit Save (the same accepted tradeoff as the dev-panel "Save designs").

mod preview;

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};
use sim::fitting::{
    CellShape, GridCell, HardpointType, Hull, HullCatalog, HullId, SectionId, ShipClass, ShipRole,
    Slot, SlotId, SlotSize, HULL_FIGHTER,
};

use crate::net::LoopbackHost;

pub use preview::PreviewPlugin;

/// The dev hull-editor app-state. Entered from the dev panel ("Open Hull Editor"); the editor systems
/// gate on `Designing`. Combat input is unaffected (the editor is a dev overlay).
#[derive(States, Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HullDesignState {
    #[default]
    Flying,
    Designing,
}

/// The hull-editor plugin (dev-only). On entering `Designing`, [`load_design_session`] clones the live
/// catalog into the [`HullDesignSession`]; [`hull_editor_ui`] draws the editor every frame while open.
pub struct HullEditorPlugin;

impl Plugin for HullEditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<HullDesignState>()
            .init_resource::<HullDesignSession>()
            .add_systems(OnEnter(HullDesignState::Designing), load_design_session)
            .add_systems(
                EguiPrimaryContextPass,
                hull_editor_ui.run_if(in_state(HullDesignState::Designing)),
            )
            // R77 — install the egui glyph fallback font ONCE (un-gated → also covers the dev panel,
            // which shares the primary egui context). No-op if `assets/fonts/symbols.ttf` is absent.
            .add_systems(EguiPrimaryContextPass, install_egui_fonts)
            .add_plugins(PreviewPlugin);
    }
}

/// R77 — register a glyph-rich FALLBACK font with egui so symbols (`▲ ▼ → ↳ …`) render instead of tofu
/// boxes (egui's bundled font lacks them). Loads `assets/fonts/symbols.ttf` (any glyph-rich TTF — e.g.
/// DejaVu Sans) via `std::fs` ONCE and appends it to the Proportional + Monospace families (egui uses a
/// fallback only for glyphs the body font is missing). No-op + a one-time log if the file is absent, so
/// the build/tests don't depend on it. Un-gated → also fixes the dev panel (shared egui context).
fn install_egui_fonts(mut contexts: EguiContexts, mut done: Local<bool>) {
    if *done {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return; // primary egui context not ready yet — retry next frame
    };
    *done = true; // attempt exactly once, whether or not the file exists
    let root = std::env::var("BEVY_ASSET_ROOT").unwrap_or_else(|_| ".".to_string());
    let path = std::path::Path::new(&root).join("assets/fonts/symbols.ttf");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "symbols".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("symbols".to_owned());
            }
            ctx.set_fonts(fonts);
            info!("hull editor: loaded glyph fallback font {}", path.display());
        }
        Err(e) => {
            info!(
                "hull editor: no glyph font at {} ({e}); editor symbols may show as boxes — drop a \
                 glyph-rich TTF (e.g. DejaVu Sans) there as symbols.ttf",
                path.display()
            );
        }
    }
}

/// R72 — the editing ACTION (the "tool"): WHAT a grid interaction does. Orthogonal to the active
/// [`Layer`]. R73 — every tool now operates on the active layer: `Paint` SETS it, `Erase` CLEARS it,
/// `Select` inspects; `Stamp` lays a multi-cell shape preset (shape-centric).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    /// Set the active [`Layer`] on the cell(s) under the cursor (Shape → the brush shape, Hull/Armor →
    /// the material brush, Module → the slot brush).
    Paint,
    /// R63 — stamp a multi-cell SHAPE preset (round cap / cone / blade / needle) at the anchor.
    Stamp,
    /// Clear the active [`Layer`] on the cell(s): Shape → delete the cell, Hull → Standard, Armor →
    /// None, Module → remove the slot.
    Erase,
    /// Just select the clicked cell (inspect / edit in the right panel).
    Select,
}

/// R72/R73 — the per-cell LAYER. It is the SINGLE active-layer selection shared by BOTH the brush
/// (what a tool acts on) AND the grid "Show:" view (what the grid is coloured by) — selecting it from
/// either the Brush "Layer:" row or the "Show:" toolbar syncs the other. Shape + Hull are mandatory;
/// Armor + Module optional (their palettes carry a "None" to clear them). Orthogonal to the [`Tool`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// The cell's sub-cell shape / slot footprint (the default authoring view).
    Shape,
    /// The HULL (structural) material.
    Hull,
    /// The ARMOR material.
    Armor,
    /// The MODULE (hardpoint slot) type.
    Module,
}

/// R80 — the top tabbed-panel tab: `Design` (hull meta / grid / move / mirror / budgets) or `Brush`
/// (Tool / Layer / the active tool's palette).
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    Design,
    Brush,
}

/// R84 — ALL the hull-editor UI sizes/positions, loaded from `assets/content/editor_layout.ron` so they
/// can be tuned by editing the file (no rebuild): reloaded on every editor open (F8) and via the title
/// bar's "Load UI" button. Missing fields fall back to the defaults (`#[serde(default)]`); a missing FILE
/// is written out as an editable template.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct EditorLayout {
    /// The top Design|Brush tabbed panel's height (px). Sized so the Brush tab shows ~one full row of
    /// shape cards plus a peek of the next.
    tabs_panel_height: f32,
    /// The Brush tab's LEFT column width (Tool/Layer + the wrapped legend).
    brush_left_width: f32,
    /// The right inspector panel's default width.
    inspect_panel_width: f32,
    /// The 3-D preview image size (square, px).
    preview_px: f32,
    /// Palette icon size (shape / tool / layer / material swatches), px.
    icon_px: f32,
    /// Grid canvas cell size, px.
    grid_cell_px: f32,
    /// The hull Name text field width (Design tab).
    name_field_width: f32,
    /// The budget sliders' width (Design tab).
    slider_width: f32,
    /// The shape-palette search box width.
    search_width: f32,
    /// R86 — the grid panel's background colour `(r, g, b)` (behind/around the grid canvas).
    grid_panel_bg: [u8; 3],
    /// R86 — the EMPTY grid cell fill `(r, g, b)` (present cells draw their layer colour on top).
    grid_empty_cell: [u8; 3],
}

impl Default for EditorLayout {
    fn default() -> Self {
        Self {
            tabs_panel_height: 230.0,
            brush_left_width: 210.0,
            inspect_panel_width: 300.0,
            preview_px: 280.0,
            icon_px: 26.0,
            grid_cell_px: 26.0,
            name_field_width: 140.0,
            slider_width: 150.0,
            search_width: 120.0,
            grid_panel_bg: [10, 11, 14],
            grid_empty_cell: [34, 39, 48],
        }
    }
}

/// R86 — an `EditorLayout` `(r, g, b)` triple as an egui colour.
fn col3(c: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(c[0], c[1], c[2])
}

/// R84 — load `editor_layout.ron` from the content dir; absent → write the DEFAULT out as an editable
/// template + use it; unparseable → log + defaults. Windowed/dev-only `std::fs` (never on a test path).
fn load_editor_layout() -> EditorLayout {
    let path = crate::tuning_io::content_dir().join("editor_layout.ron");
    match std::fs::read_to_string(&path) {
        Ok(text) => match ron::from_str(&text) {
            Ok(layout) => layout,
            Err(e) => {
                warn!("editor_layout.ron parse error ({e}); using defaults");
                EditorLayout::default()
            }
        },
        Err(_) => {
            let def = EditorLayout::default();
            if let Ok(text) = ron::ser::to_string_pretty(&def, ron::ser::PrettyConfig::default()) {
                if std::fs::write(&path, text).is_ok() {
                    info!(
                        "wrote default {} (edit + \"Load UI\" to tune)",
                        path.display()
                    );
                }
            }
            def
        }
    }
}

/// R86 — write the CURRENT editor-UI sizes back to `editor_layout.ron` (the "Save UI" button), so live
/// tweaks — e.g. a Ctrl+wheel-zoomed grid — persist. Returns the status line to show.
fn save_editor_layout(layout: &EditorLayout) -> String {
    let path = crate::tuning_io::content_dir().join("editor_layout.ron");
    match ron::ser::to_string_pretty(layout, ron::ser::PrettyConfig::default()) {
        Ok(body) => {
            let text = format!(
                "// Hull-editor UI sizes/colours. Edit + \"Load UI\" (or close + F8) to apply; \"Save UI\" rewrites it.\n{body}"
            );
            match std::fs::write(&path, text) {
                Ok(()) => format!("saved {}", path.display()),
                Err(e) => format!("save FAILED: {e}"),
            }
        }
        Err(e) => format!("save FAILED (serialize): {e}"),
    }
}

/// R63/R64 — a family of related [`CellShape`]s, shown as one chip in the compact palette.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShapeFamily {
    Full,
    Half,
    Quarter,
    Chamfer,
    Slope,
    Slope3,
    Slope4,
    Wedge2,
    Wedge3,
    Wedge4,
    Strip34,
    Strip12,
    Strip14,
    Strip18,
    Point,
    Round,
    Octagon,
}

impl ShapeFamily {
    const ALL: [ShapeFamily; 17] = [
        ShapeFamily::Full,
        ShapeFamily::Half,
        ShapeFamily::Quarter,
        ShapeFamily::Chamfer,
        ShapeFamily::Slope,
        ShapeFamily::Slope3,
        ShapeFamily::Slope4,
        ShapeFamily::Wedge2,
        ShapeFamily::Wedge3,
        ShapeFamily::Wedge4,
        ShapeFamily::Strip34,
        ShapeFamily::Strip12,
        ShapeFamily::Strip14,
        ShapeFamily::Strip18,
        ShapeFamily::Point,
        ShapeFamily::Round,
        ShapeFamily::Octagon,
    ];
    fn label(self) -> &'static str {
        match self {
            ShapeFamily::Full => "Full",
            ShapeFamily::Half => "Half",
            ShapeFamily::Quarter => "Quarter",
            ShapeFamily::Chamfer => "Chamfer",
            // FAT trapezoids (a near-full cell with a corner shaved).
            ShapeFamily::Slope => "Slope 1:2",
            ShapeFamily::Slope3 => "Slope 1:3",
            ShapeFamily::Slope4 => "Slope 1:4",
            // THIN triangles (the skinny complement of the slope).
            ShapeFamily::Wedge2 => "Wedge 1:2",
            ShapeFamily::Wedge3 => "Wedge 1:3",
            ShapeFamily::Wedge4 => "Wedge 1:4",
            ShapeFamily::Strip34 => "Strip 3/4",
            ShapeFamily::Strip12 => "Strip 1/2",
            ShapeFamily::Strip14 => "Strip 1/4",
            ShapeFamily::Strip18 => "Strip 1/8",
            ShapeFamily::Point => "Point",
            ShapeFamily::Round => "Round",
            ShapeFamily::Octagon => "Octagon",
        }
    }
    /// The shapes (orientations) in this family.
    fn shapes(self) -> Vec<CellShape> {
        use CellShape::*;
        match self {
            ShapeFamily::Full => vec![Full],
            ShapeFamily::Half => vec![HalfNW, HalfNE, HalfSW, HalfSE],
            ShapeFamily::Quarter => vec![QuarterNW, QuarterNE, QuarterSW, QuarterSE],
            ShapeFamily::Chamfer => vec![ChamferNW, ChamferNE, ChamferSW, ChamferSE],
            ShapeFamily::Slope => vec![
                SlopeNWH, SlopeNWV, SlopeNEH, SlopeNEV, SlopeSWH, SlopeSWV, SlopeSEH, SlopeSEV,
            ],
            ShapeFamily::Slope3 => vec![
                Slope3NWH, Slope3NWV, Slope3NEH, Slope3NEV, Slope3SWH, Slope3SWV, Slope3SEH,
                Slope3SEV,
            ],
            ShapeFamily::Slope4 => vec![
                Slope4NWH, Slope4NWV, Slope4NEH, Slope4NEV, Slope4SWH, Slope4SWV, Slope4SEH,
                Slope4SEV,
            ],
            ShapeFamily::Wedge2 => vec![
                Wedge2NWH, Wedge2NWV, Wedge2NEH, Wedge2NEV, Wedge2SWH, Wedge2SWV, Wedge2SEH,
                Wedge2SEV,
            ],
            ShapeFamily::Wedge3 => vec![
                Wedge3NWH, Wedge3NWV, Wedge3NEH, Wedge3NEV, Wedge3SWH, Wedge3SWV, Wedge3SEH,
                Wedge3SEV,
            ],
            ShapeFamily::Wedge4 => vec![
                Wedge4NWH, Wedge4NWV, Wedge4NEH, Wedge4NEV, Wedge4SWH, Wedge4SWV, Wedge4SEH,
                Wedge4SEV,
            ],
            ShapeFamily::Strip34 => vec![StripN34, StripS34, StripE34, StripW34],
            ShapeFamily::Strip12 => vec![StripN12, StripS12, StripE12, StripW12],
            ShapeFamily::Strip14 => vec![StripN14, StripS14, StripE14, StripW14],
            ShapeFamily::Strip18 => vec![StripN18, StripS18, StripE18, StripW18],
            ShapeFamily::Point => vec![PointN, PointS, PointE, PointW],
            ShapeFamily::Round => vec![RoundN, RoundS, RoundE, RoundW],
            ShapeFamily::Octagon => vec![Octagon],
        }
    }
}

/// R63 — a multi-cell stamp preset (canonical orientation points +row / North).
#[derive(Clone, Copy, PartialEq, Eq)]
enum StampKind {
    Blade5,
    Blade7,
    Needle,
    Cone3,
    Cone5,
    RoundCap3,
    RoundCap5,
}

impl StampKind {
    const ALL: [StampKind; 7] = [
        StampKind::Blade5,
        StampKind::Blade7,
        StampKind::Needle,
        StampKind::Cone3,
        StampKind::Cone5,
        StampKind::RoundCap3,
        StampKind::RoundCap5,
    ];
    fn label(self) -> &'static str {
        match self {
            StampKind::Blade5 => "Blade (5)",
            StampKind::Blade7 => "Blade (7)",
            StampKind::Needle => "Needle",
            StampKind::Cone3 => "Cone (3)",
            StampKind::Cone5 => "Cone (5)",
            StampKind::RoundCap3 => "Round cap (3)",
            StampKind::RoundCap5 => "Round cap (5)",
        }
    }
}

/// R63 — the stamp direction (where the end points). `N` = canonical (+row).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    N,
    E,
    S,
    W,
}

/// The editor's working state — a COPY of the live hull catalog plus the hull being edited. Nothing
/// touches the running game until "Apply to live" / "Save".
#[derive(Resource)]
pub struct HullDesignSession {
    /// The live hull catalog (cloned on enter); the working hull is committed back into it on apply/save.
    catalog: HullCatalog,
    /// The hull currently being edited.
    working: Hull,
    /// Which catalog id `working` is (so apply/save write the right row).
    selected_hull: HullId,
    /// The shape painted by left-click in `Paint`+`Shape` mode.
    brush: CellShape,
    /// R72 — the editing ACTION (the tool). The LAYER it targets is the shared `layer` below.
    tool: Tool,
    /// R80 — the active top-panel tab (Design / Brush).
    tab: EditorTab,
    /// R84 — the RON-tunable UI sizes (`editor_layout.ron`), reloaded on editor open + "Load UI".
    layout: EditorLayout,
    selected_cell: Option<(u16, u16)>,
    selected_slot: Option<SlotId>,
    /// R61 — staged grid size (the cols/rows fields edit this; "Apply grid" commits it so editing the
    /// numbers never wipes the design until applied).
    pending_grid: (u16, u16),
    /// R61 — the last cell painted during a click-drag (so a held drag fills continuously without
    /// re-painting the same cell, and a fast drag can line-fill between successive cells).
    last_painted: Option<(u16, u16)>,
    /// R74 — the shape-palette family FILTER (`None` = show all 86 shapes) + a name search box. The
    /// palette shows every matching shape in a scrollable grid (R63's single-family view is now the
    /// "filter to one family" case).
    palette_filter: Option<ShapeFamily>,
    palette_search: String,
    /// R63 — the selected multi-cell stamp + its direction (used in `Stamp` mode).
    stamp_kind: StampKind,
    stamp_dir: Dir,
    /// R66 — the hull/armor material ids painted in `Hull`/`Armor` mode.
    hull_material_brush: u8,
    armor_material_brush: u8,
    /// R68 — the module type painted in `Module` mode (`None` removes the slot).
    module_brush: Option<HardpointType>,
    /// R72/R73 — the SINGLE active layer: both the brush paint TARGET and the grid "Show:" VIEW. The
    /// Brush "Layer:" row and the "Show:" toolbar both edit this one field → they stay in sync.
    layer: Layer,
    status: String,
    /// R79 — UNDO / REDO snapshot stacks of the working `Hull` (the design is small + cloneable). A
    /// `checkpoint()` is taken once per edit gesture; `undo`/`redo` swap `working` with a stack top.
    undo: Vec<Hull>,
    redo: Vec<Hull>,
    /// Set on any edit → the 3-D preview rebuilds its mesh next frame.
    pub dirty: bool,
    /// Preview camera orbit (yaw, pitch) in radians, dragged on the preview image.
    pub orbit: (f32, f32),
}

/// R79 — cap the undo history so a long session can't grow unbounded.
const UNDO_CAP: usize = 64;

impl HullDesignSession {
    /// R79 — snapshot the CURRENT working hull onto the undo stack (and clear the redo stack) BEFORE an
    /// edit, so one call = one undoable step. Call at the start of each edit gesture.
    fn checkpoint(&mut self) {
        self.redo.clear();
        self.undo.push(self.working.clone());
        if self.undo.len() > UNDO_CAP {
            self.undo.remove(0);
        }
    }

    /// R79 — restore the previous snapshot (pushing the current state onto the redo stack).
    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            let cur = std::mem::replace(&mut self.working, prev);
            self.redo.push(cur);
            self.after_history();
        }
    }

    /// R79 — re-apply an undone snapshot (pushing the current state back onto the undo stack).
    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            let cur = std::mem::replace(&mut self.working, next);
            self.undo.push(cur);
            self.after_history();
        }
    }

    /// Shared cleanup after an undo/redo swaps `working` (rebuild preview, drop a possibly-stale
    /// selection, resync the staged grid size).
    fn after_history(&mut self) {
        self.dirty = true;
        self.selected_cell = None;
        self.selected_slot = None;
        self.pending_grid = self.working.grid_dims;
    }
}

impl Default for HullDesignSession {
    fn default() -> Self {
        let (_, catalog) = sim::fitting::seed_catalogs();
        let working = catalog
            .get(HULL_FIGHTER)
            .cloned()
            .unwrap_or_else(|| blank_hull(HULL_FIGHTER));
        let pending_grid = working.grid_dims;
        Self {
            catalog,
            working,
            selected_hull: HULL_FIGHTER,
            brush: CellShape::Full,
            tool: Tool::Paint,
            tab: EditorTab::Design,
            layout: EditorLayout::default(),
            selected_cell: None,
            selected_slot: None,
            pending_grid,
            last_painted: None,
            palette_filter: None, // show all shapes by default
            palette_search: String::new(),
            stamp_kind: StampKind::Blade5,
            stamp_dir: Dir::N,
            hull_material_brush: 2,  // Heavy (so Hull painting is visible)
            armor_material_brush: 1, // Light
            module_brush: Some(HardpointType::Weapon),
            layer: Layer::Shape,
            status: String::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: true,
            orbit: (0.6, 0.5),
        }
    }
}

/// `OnEnter(Designing)` — clone the LIVE catalogs from the embedded server + load the currently-selected
/// hull (default the fighter) into `working`. Windowed-only (the embedded server lives only here).
fn load_design_session(
    mut session: ResMut<HullDesignSession>,
    host: Option<NonSend<LoopbackHost>>,
) {
    if let Some(host) = host {
        let w = host.server.world();
        if let Some(h) = w.get_resource::<HullCatalog>() {
            session.catalog = h.clone();
        }
    }
    let id = session.selected_hull;
    if let Some(h) = session.catalog.get(id).cloned() {
        session.working = h;
    }
    session.pending_grid = session.working.grid_dims;
    session.selected_cell = None;
    session.selected_slot = None;
    session.dirty = true;
    session.status.clear();
    // R84 — re-read the UI sizes on every editor open, so editing editor_layout.ron + F8 applies it.
    session.layout = load_editor_layout();
}

/// The editor screen (egui), drawn every frame while `Designing`.
fn hull_editor_ui(
    mut contexts: EguiContexts,
    mut session: ResMut<HullDesignSession>,
    host: Option<NonSendMut<LoopbackHost>>,
    mut next_state: ResMut<NextState<HullDesignState>>,
    preview: Res<preview::PreviewTarget>,
    materials: Option<Res<sim::fitting::CellMaterials>>,
) {
    // Register the preview render-target as an egui texture (must borrow `contexts` BEFORE `ctx_mut`).
    let preview_tex = contexts.image_id(&preview.image).unwrap_or_else(|| {
        contexts.add_image(bevy_egui::EguiTextureHandle::Strong(preview.image.clone()))
    });
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    // R66 — the live materials catalog (windowed override or default), for the material brush names.
    let cell_materials = materials.map(|m| m.clone()).unwrap_or_default();
    // R88 — the LIVE struct-cell fallbacks (material id 0 = these) for the Design-tab stats readout.
    let (hp_fb, mass_fb) = host
        .as_ref()
        .and_then(|h| h.server.world().get_resource::<sim::SimTuning>())
        .map(|t| (t.struct_cell_hp, t.struct_cell_mass))
        .unwrap_or_else(|| {
            let d = sim::SimTuning::default();
            (d.struct_cell_hp, d.struct_cell_mass)
        });
    let s = &mut *session;

    // R79 — Ctrl+Z / Ctrl+Y (or Ctrl+Shift+Z) undo / redo. Gated on `!wants_keyboard_input` so Ctrl+Z
    // still works INSIDE the search box (egui handles text-field undo itself).
    if !ctx.wants_keyboard_input() {
        let (undo, redo) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                cmd && !i.modifiers.shift && i.key_pressed(egui::Key::Z),
                cmd && (i.key_pressed(egui::Key::Y)
                    || (i.modifiers.shift && i.key_pressed(egui::Key::Z))),
            )
        });
        if undo {
            s.undo();
        }
        if redo {
            s.redo();
        }
        // R88 — tool/layer hotkeys (gated like undo/redo so typing in Name/search is unaffected):
        // Q/W/E/R = Paint/Stamp/Erase/Select, 1–4 = Shape/Hull/Armor/Module.
        ctx.input(|i| {
            for (key, tool) in [
                (egui::Key::Q, Tool::Paint),
                (egui::Key::W, Tool::Stamp),
                (egui::Key::E, Tool::Erase),
                (egui::Key::R, Tool::Select),
            ] {
                if i.key_pressed(key) {
                    s.tool = tool;
                }
            }
            for (key, layer) in [
                (egui::Key::Num1, Layer::Shape),
                (egui::Key::Num2, Layer::Hull),
                (egui::Key::Num3, Layer::Armor),
                (egui::Key::Num4, Layer::Module),
            ] {
                if i.key_pressed(key) {
                    s.layer = layer;
                }
            }
        });
    }

    // Intents collected in the panel closures, executed after (so the closures don't hold `host`).
    let mut do_apply = false;
    let mut do_save = false;
    let mut do_save_new = false;
    let mut do_close = false;

    // ---- Top bar: hull selector + actions + status ----
    egui::TopBottomPanel::top("hull_editor_top").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Hull Editor");
            ui.separator();
            // Hull selector.
            let mut pick = s.selected_hull;
            egui::ComboBox::from_id_salt("hull_pick")
                .selected_text(format!("{} (#{})", s.working.name, s.selected_hull.0))
                .show_ui(ui, |ui| {
                    for (id, h) in s.catalog.hulls.iter() {
                        ui.selectable_value(&mut pick, *id, format!("{} (#{})", h.name, id.0));
                    }
                });
            if pick != s.selected_hull {
                if let Some(h) = s.catalog.get(pick).cloned() {
                    // R65 — commit the current edits into the in-memory catalog BEFORE switching, so
                    // unsaved work survives a switch-and-switch-back (disk save still needs the button).
                    let mut cur = s.working.clone();
                    normalize_hull(&mut cur);
                    s.catalog.hulls.insert(s.selected_hull, cur);
                    s.working = h;
                    s.selected_hull = pick;
                    s.pending_grid = s.working.grid_dims;
                    s.selected_cell = None;
                    s.selected_slot = None;
                    s.dirty = true;
                }
            }
            if ui.button("New blank hull").clicked() {
                let id = next_hull_id(&s.catalog);
                s.working = blank_hull(id);
                s.selected_hull = id;
                s.pending_grid = s.working.grid_dims;
                s.catalog.hulls.insert(id, s.working.clone());
                s.selected_cell = None;
                s.selected_slot = None;
                s.dirty = true;
                s.status = format!("New hull #{}", id.0);
            }
            ui.separator();
            if ui.button("Apply to live").clicked() {
                do_apply = true;
            }
            if ui
                .button("Save → ship file")
                .on_hover_text("Write THIS ship to its own assets/content/ships/<id>_<name>.ron")
                .clicked()
            {
                do_save = true;
            }
            if ui
                .button("Save as NEW ship")
                .on_hover_text("Save the current design as a brand-new ship (fresh id) — keeps the original file")
                .clicked()
            {
                do_save_new = true;
            }
            ui.separator();
            // R79/R80/R87 — undo / redo glyph icons (⭯ / ⭮ — covered by the Noto Sans Symbols 2
            // fallback installed at assets/fonts/symbols.ttf). Also Ctrl+Z / Ctrl+Y, wired below.
            if ui
                .add_enabled(!s.undo.is_empty(), egui::Button::new("⭯"))
                .on_hover_text("Undo (Ctrl+Z)")
                .clicked()
            {
                s.undo();
            }
            if ui
                .add_enabled(!s.redo.is_empty(), egui::Button::new("⭮"))
                .on_hover_text("Redo (Ctrl+Y / Ctrl+Shift+Z)")
                .clicked()
            {
                s.redo();
            }
            // R84/R87 — re-read editor_layout.ron live (edit the file → click → the sizes apply).
            if ui
                .button("Load UI")
                .on_hover_text("Reload assets/content/editor_layout.ron (UI sizes)")
                .clicked()
            {
                s.layout = load_editor_layout();
                s.status = "UI layout reloaded".into();
            }
            // R86/R87 — persist the CURRENT UI sizes (incl. a Ctrl+wheel-zoomed grid) back to the RON.
            if ui
                .button("Save UI")
                .on_hover_text("Save the current UI sizes to assets/content/editor_layout.ron")
                .clicked()
            {
                s.status = save_editor_layout(&s.layout);
            }
            // R80 — Close ✕ pinned to the FAR RIGHT of the title bar. R85 — the status renders INLINE
            // on this same row (left of ✕) so a message appearing never adds a line / shifts the panels.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("✕").on_hover_text("Close").clicked() {
                    do_close = true;
                }
                ui.weak(&s.status);
            });
        });
    });

    // ---- Top: a TABBED panel — Design (hull/grid/move/mirror/budgets) | Brush (tool/layer/palette) ----
    // R80 — replaces the left side panel + the R79 brush top panel. `auto_shrink(false)` on each tab's
    // body holds the panel at a CONSISTENT height (no shrink/jump when switching Tool/Layer or tabs).
    // R84 — EXACT height from editor_layout.ron (the RON is the single source of truth → "Load UI"
    // applies instantly; egui's drag-resize memory would otherwise shadow a reloaded value).
    egui::TopBottomPanel::top("hull_editor_tabs")
        .resizable(false)
        .exact_height(s.layout.tabs_panel_height)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.tab, EditorTab::Design, "Design");
                ui.selectable_value(&mut s.tab, EditorTab::Brush, "Brush");
            });
            ui.separator();
            match s.tab {
                EditorTab::Design => {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .id_salt("design_scroll")
                        .show(ui, |ui| {
                            design_tab(ui, s, &cell_materials, hp_fb, mass_fb);
                        });
                }
                EditorTab::Brush => {
                    // R82/R85 — Tool/Layer on the LEFT, the shape palette to their RIGHT. R85 — done
                    // with NESTED `show_inside` panels: `ui.horizontal_top` gives children a region only
                    // ~one row TALL, so the R82–R84 ScrollArea inside it was a sliver — its content
                    // (chips/search) overflowed + got clipped at the panel bottom, and the icon cards
                    // were drawn below the clip line, unreachable by scroll. Nested panels give REAL
                    // bounded rects → the palette fills + scrolls correctly.
                    egui::SidePanel::left("brush_left_inside")
                        .resizable(false)
                        .exact_width(s.layout.brush_left_width)
                        .show_inside(ui, |ui| {
                            brush_header(ui, s);
                        });
                    egui::CentralPanel::default().show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink(false)
                            .id_salt("brush_scroll")
                            .show(ui, |ui| {
                                brush_options(ui, s, &cell_materials);
                            });
                    });
                }
            }
        });

    // ---- Right: selected-cell + slots + 3-D preview ----
    egui::SidePanel::right("hull_editor_inspect")
        .default_width(s.layout.inspect_panel_width)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("inspect_scroll")
                .show(ui, |ui| {
                    // 3-D preview pane (render-to-texture); drag to orbit.
                    ui.heading("Preview");
                    let size = egui::vec2(s.layout.preview_px, s.layout.preview_px);
                    let resp = ui.add(
                        egui::Image::new(egui::load::SizedTexture::new(preview_tex, size))
                            .sense(egui::Sense::drag()),
                    );
                    if resp.dragged() {
                        let d = resp.drag_delta();
                        s.orbit.0 += d.x * 0.01;
                        s.orbit.1 = (s.orbit.1 + d.y * 0.01).clamp(-1.4, 1.4);
                    }
                    ui.separator();

                    ui.heading("Selected cell");
                    if let Some(coord) = s.selected_cell {
                        ui.label(format!("({}, {})", coord.0, coord.1));
                        if let Some(idx) = s.working.cells.iter().position(|c| c.coord == coord) {
                            let mut shape = s.working.cells[idx].shape;
                            let lay = s.layout;
                            if shape_palette(
                                ui,
                                "inspect",
                                &mut shape,
                                &mut s.palette_filter,
                                &mut s.palette_search,
                                lay,
                            ) {
                                s.checkpoint();
                                s.working.cells[idx].shape = shape;
                                s.dirty = true;
                            }
                            // R68 — this cell's full layer STACK as icon palettes (matching the brush palettes),
                            // each editing the selected cell directly + showing the chosen entry's stats.
                            ui.separator();
                            ui.label("Hull material");
                            let hull_items: Vec<(egui::Color32, &str)> = cell_materials
                                .hull
                                .iter()
                                .enumerate()
                                .map(|(i, h)| (hull_mat_color(i as u8), h.name.as_str()))
                                .collect();
                            let mut hm = s.working.cells[idx].hull_material;
                            if swatch_palette(ui, &mut hm, &hull_items, lay.icon_px) {
                                s.checkpoint();
                                s.working.cells[idx].hull_material = hm;
                                s.dirty = true;
                            }
                            if let Some(h) = cell_materials.hull.get(hm as usize) {
                                ui.small(format!("hp {:.1} · mass {:.2}", h.cell_hp, h.mass));
                            }
                            ui.label("Armor material");
                            let armor_items: Vec<(egui::Color32, &str)> = cell_materials
                                .armor
                                .iter()
                                .enumerate()
                                .map(|(i, a)| (armor_mat_color(i as u8), a.name.as_str()))
                                .collect();
                            let mut am = s.working.cells[idx].armor_material;
                            if swatch_palette(ui, &mut am, &armor_items, lay.icon_px) {
                                s.checkpoint();
                                s.working.cells[idx].armor_material = am;
                                s.dirty = true;
                            }
                            if let Some(a) = cell_materials.armor.get(am as usize) {
                                ui.small(format!(
                                    "th {:.1}×{:.1} · carve {:.0} · mass {:.2}",
                                    a.thickness, a.multiplier, a.carve_hp, a.mass
                                ));
                            }
                            // R68 — Module: pick a hardpoint type as icons (— = none); size/facing edited below.
                            ui.label("Module");
                            let mut module = s
                                .working
                                .slots
                                .iter()
                                .find(|sl| sl.coord == coord)
                                .map(|sl| sl.slot_type);
                            if module_palette(ui, &mut module, lay.icon_px) {
                                s.checkpoint();
                                set_cell_module(s, coord, module);
                            }
                            if let Some(sidx) =
                                s.working.slots.iter().position(|sl| sl.coord == coord)
                            {
                                ui.label(format!("Slot #{}", s.working.slots[sidx].id.0));
                                slot_editor(ui, &mut s.working.slots[sidx]);
                            }
                        } else {
                            ui.label("(empty — paint a cell here)");
                        }
                    } else {
                        ui.label("(click a cell)");
                    }

                    ui.separator();
                    ui.heading("Slots");
                    let coords: Vec<(SlotId, (u16, u16), HardpointType)> = s
                        .working
                        .slots
                        .iter()
                        .map(|sl| (sl.id, sl.coord, sl.slot_type))
                        .collect();
                    for (id, coord, ty) in coords {
                        if ui
                            .button(format!("#{} {:?} ({},{})", id.0, ty, coord.0, coord.1))
                            .clicked()
                        {
                            s.selected_cell = Some(coord);
                            s.selected_slot = Some(id);
                        }
                    }
                });
        });

    // ---- Center: the cell-grid painter ----
    // R86 — the panel behind the grid uses a CONFIGURABLE near-black fill (editor_layout.ron
    // `grid_panel_bg`) so the lighter grid cells stand out.
    let grid_frame = egui::Frame::central_panel(&ctx.style()).fill(col3(s.layout.grid_panel_bg));
    egui::CentralPanel::default()
        .frame(grid_frame)
        .show(ctx, |ui| {
            // R88 — the active layer's colour LEGEND sits at the top of the grid panel, above the ship
            // (it keys the grid's colours). A single wrapped row → near-constant height.
            brush_legend(ui, s.layer, &cell_materials);
            ui.separator();
            // R85 — the grid canvas is CENTERED width-wise in the viewport (left-padded by half the slack);
            // when it's wider than the viewport the pad is 0 and it scrolls exactly as before.
            let avail_w = ui.available_width();
            egui::ScrollArea::both().show(ui, |ui| {
                let grid_w = s.working.grid_dims.0 as f32 * s.layout.grid_cell_px;
                let pad = ((avail_w - grid_w) * 0.5).max(0.0);
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    draw_grid(ui, s);
                });
            });
        });

    // ---- Execute deferred intents (need `host`) ----
    if do_close {
        next_state.set(HullDesignState::Flying);
    }
    if do_apply {
        s.status = apply_to_live(host, s);
    } else if do_save {
        s.status = save_design(s);
    } else if do_save_new {
        s.status = save_as_new(s);
    }
}

/// R61 — the editable cell grid as ONE painted CANVAS (nose-up / port-left, matching the fitting view +
/// in-game): each present cell is drawn as its REAL `CellShape` polygon (so the design reads at a glance),
/// and a single `click_and_drag` interaction lets Paint/Erase modes drag a swath across the grid.
fn draw_grid(ui: &mut egui::Ui, s: &mut HullDesignSession) {
    let (cols, rows) = s.working.grid_dims;
    let cell_px = s.layout.grid_cell_px; // R84 — RON-tunable (editor_layout.ron)
    let size = egui::vec2(cols as f32 * cell_px, rows as f32 * cell_px);
    let (canvas, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let painter = ui.painter_at(canvas);

    let cell_rect = |col: u16, row: u16| {
        // Port-left (high col on the LEFT) + nose-up (high row at the TOP).
        let x = canvas.min.x + (cols - 1 - col) as f32 * cell_px;
        let y = canvas.min.y + (rows - 1 - row) as f32 * cell_px;
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell_px, cell_px))
    };
    let hovered = resp
        .hover_pos()
        .and_then(|p| cell_at_pointer(canvas, p, (cols, rows), cell_px));

    // R86 — Ctrl+wheel (or pinch) over the grid ZOOMS the cell size (egui's `zoom_delta` channel, so a
    // plain wheel still scrolls the surrounding ScrollArea). "Save UI" persists the zoomed size.
    if resp.hovered() {
        let zoom = ui.input(|i| i.zoom_delta());
        if zoom != 1.0 {
            s.layout.grid_cell_px = (s.layout.grid_cell_px * zoom).clamp(8.0, 80.0);
        }
    }

    for row in 0..rows {
        for col in 0..cols {
            let coord = (col, row);
            let rect = cell_rect(col, row);
            painter.rect_filled(rect.shrink(0.5), 2.0, col3(s.layout.grid_empty_cell));
            if let Some(c) = s.working.cells.iter().find(|c| c.coord == coord).copied() {
                let slot = s.working.slots.iter().find(|sl| sl.coord == coord).copied();
                // R67 — the cell FILL follows the active grid view (shape / hull mat / armor mat).
                let fill = match s.layer {
                    Layer::Shape => {
                        slot.map_or_else(|| shape_color(c.shape), |sl| slot_color(sl.slot_type))
                    }
                    Layer::Hull => hull_mat_color(c.hull_material),
                    Layer::Armor => armor_mat_color(c.armor_material),
                    // R68 — Module view: colour by the slot type (structural cells = neutral grey).
                    Layer::Module => slot.map_or(egui::Color32::from_rgb(60, 66, 78), |sl| {
                        slot_color(sl.slot_type)
                    }),
                };
                painter.add(egui::Shape::convex_polygon(
                    shape_poly_in_rect(c.shape, rect.shrink(1.5)),
                    fill,
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(15, 18, 22)),
                ));
                if let Some(sl) = slot {
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        slot_initial(sl.slot_type),
                        egui::FontId::proportional(11.0),
                        egui::Color32::BLACK,
                    );
                }
                // R66/R67/R68 — material overlay: a hull-material tint dot (top-left) + an armor-material
                // border. Drawn as a HINT in every view EXCEPT the one where that layer is the fill
                // (so HullMat view drops the dot, ArmorMat view drops the border; Shape/Module show both).
                let show_hull_dot = !matches!(s.layer, Layer::Hull) && c.hull_material > 0;
                let show_armor_border = !matches!(s.layer, Layer::Armor) && c.armor_material > 0;
                if show_hull_dot {
                    painter.circle_filled(
                        rect.left_top() + egui::vec2(5.0, 5.0),
                        3.0,
                        hull_mat_color(c.hull_material),
                    );
                }
                if show_armor_border {
                    painter.rect_stroke(
                        rect.shrink(2.0),
                        2.0,
                        egui::Stroke::new(2.0, armor_mat_color(c.armor_material)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            if s.selected_cell == Some(coord) {
                painter.rect_stroke(
                    rect.shrink(0.5),
                    2.0,
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(240, 220, 80)),
                    egui::StrokeKind::Inside,
                );
            } else if hovered == Some(coord) {
                painter.rect_stroke(
                    rect.shrink(0.5),
                    2.0,
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 130, 150)),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    // R63 — faint vertical CENTER LINE (the bilateral-symmetry / mirror axis), drawn over the cells.
    let cx = canvas.center().x;
    painter.line_segment(
        [egui::pos2(cx, canvas.min.y), egui::pos2(cx, canvas.max.y)],
        egui::Stroke::new(
            1.5,
            egui::Color32::from_rgba_unmultiplied(230, 230, 255, 70),
        ),
    );

    // R88 — GHOST preview: show what a click would do at the hovered cell (~45% alpha) — the brush
    // shape / material tint / the WHOLE oriented stamp footprint / an erase mark.
    if let Some(h) = hovered {
        let rect = cell_rect(h.0, h.1);
        match s.tool {
            Tool::Paint => match s.layer {
                Layer::Shape => {
                    painter.add(egui::Shape::convex_polygon(
                        shape_poly_in_rect(s.brush, rect.shrink(1.5)),
                        shape_color(s.brush).gamma_multiply(0.45),
                        egui::Stroke::NONE,
                    ));
                }
                Layer::Hull => {
                    painter.rect_filled(
                        rect.shrink(1.5),
                        2.0,
                        hull_mat_color(s.hull_material_brush).gamma_multiply(0.45),
                    );
                }
                Layer::Armor => {
                    painter.rect_stroke(
                        rect.shrink(2.0),
                        2.0,
                        egui::Stroke::new(
                            2.0,
                            armor_mat_color(s.armor_material_brush).gamma_multiply(0.6),
                        ),
                        egui::StrokeKind::Inside,
                    );
                }
                Layer::Module => match s.module_brush {
                    Some(ty) => {
                        painter.rect_filled(
                            rect.shrink(1.5),
                            2.0,
                            slot_color(ty).gamma_multiply(0.45),
                        );
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            slot_initial(ty),
                            egui::FontId::proportional(11.0),
                            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140),
                        );
                    }
                    None => {
                        painter.rect_filled(
                            rect.shrink(1.5),
                            2.0,
                            egui::Color32::from_rgba_unmultiplied(20, 22, 28, 110),
                        );
                    }
                },
            },
            Tool::Stamp => {
                for (dc, dr, shape) in oriented_stamp_cells(s.stamp_kind, s.stamp_dir) {
                    let (nc, nr) = (h.0 as i32 + dc, h.1 as i32 + dr);
                    if nc < 0 || nr < 0 || nc >= cols as i32 || nr >= rows as i32 {
                        continue;
                    }
                    let r = cell_rect(nc as u16, nr as u16);
                    painter.add(egui::Shape::convex_polygon(
                        shape_poly_in_rect(shape, r.shrink(1.5)),
                        shape_color(shape).gamma_multiply(0.45),
                        egui::Stroke::NONE,
                    ));
                }
            }
            Tool::Erase => {
                painter.rect_filled(
                    rect.shrink(0.5),
                    2.0,
                    egui::Color32::from_rgba_unmultiplied(230, 90, 90, 70),
                );
            }
            Tool::Select => {} // the hover outline above already shows the target
        }
    }

    // R79 — snapshot ONCE at the start of an editing gesture (drag/click), so one undo = one stroke.
    // Covers every grid edit: paint / erase / material / module / stamp. Select (primary) never edits.
    let primary_start = resp.drag_started_by(egui::PointerButton::Primary)
        || resp.clicked_by(egui::PointerButton::Primary);
    let secondary_start = resp.drag_started_by(egui::PointerButton::Secondary)
        || resp.clicked_by(egui::PointerButton::Secondary);
    if (primary_start && s.tool != Tool::Select) || secondary_start {
        s.checkpoint();
    }

    // R67/R73 — Right-click / DRAG → erase a swath (line-filled so a fast drag has no gaps), matching
    // left-drag paint. R73 — it clears the ACTIVE LAYER (same as the Erase tool), so right-drag while
    // viewing Hull resets hull, on Shape deletes cells, etc. Reuses the `last_painted` tracker (only one
    // button drags at a time; the `!resp.dragged()` reset below clears it between drags).
    if resp.dragged_by(egui::PointerButton::Secondary)
        || resp.clicked_by(egui::PointerButton::Secondary)
    {
        if let Some(coord) = resp
            .interact_pointer_pos()
            .and_then(|p| cell_at_pointer(canvas, p, (cols, rows), cell_px))
        {
            s.selected_cell = Some(coord);
            let layer = s.layer;
            let from = s.last_painted.unwrap_or(coord);
            for cc in line_cells(from, coord) {
                erase_layer(s, cc, layer);
            }
            s.last_painted = Some(coord);
        }
    }
    // PRIMARY click / DRAG → paint / material / select / stamp (line-filled so a fast drag has no
    // gaps). R67 — gated to the PRIMARY button so a right-drag (erase, above) does NOT also paint.
    if resp.dragged_by(egui::PointerButton::Primary)
        || resp.clicked_by(egui::PointerButton::Primary)
    {
        if let Some(coord) = resp
            .interact_pointer_pos()
            .and_then(|p| cell_at_pointer(canvas, p, (cols, rows), cell_px))
        {
            s.selected_cell = Some(coord);
            match s.tool {
                // R72 — Paint applies the active LAYER along the drag; Erase removes whole cells.
                Tool::Paint => {
                    let layer = s.layer;
                    let from = s.last_painted.unwrap_or(coord);
                    for cc in line_cells(from, coord) {
                        match layer {
                            Layer::Shape => paint_cell(s, cc),
                            Layer::Hull => paint_hull_material(s, cc),
                            Layer::Armor => paint_armor_material(s, cc),
                            Layer::Module => paint_module(s, cc),
                        }
                    }
                    s.last_painted = Some(coord);
                }
                // R73 — Erase CLEARS the active layer (Shape → delete cell; Hull → Standard; Armor →
                // None; Module → remove slot).
                Tool::Erase => {
                    let layer = s.layer;
                    let from = s.last_painted.unwrap_or(coord);
                    for cc in line_cells(from, coord) {
                        erase_layer(s, cc, layer);
                    }
                    s.last_painted = Some(coord);
                }
                // Stamp places on a fresh CLICK only (a drag mustn't re-stamp every frame).
                Tool::Stamp => {
                    if resp.clicked_by(egui::PointerButton::Primary) {
                        apply_stamp(s, coord);
                    }
                }
                Tool::Select => {}
            }
        }
    }
    if !resp.dragged() {
        s.last_painted = None;
    }
}

/// Map a [`CellShape`]'s unit-cell polygon into a screen `rect`, using the editor orientation
/// (`+col → LEFT`, `+row → UP`) so an icon / grid cell looks exactly like the painted hull cell.
fn shape_poly_in_rect(shape: CellShape, rect: egui::Rect) -> Vec<egui::Pos2> {
    shape
        .corners(0, 0)
        .iter()
        .map(|p| {
            egui::pos2(
                rect.min.x + (1.0 - p.x) * rect.width(),
                rect.min.y + (1.0 - p.y) * rect.height(),
            )
        })
        .collect()
}

/// The grid cell under a pointer position (inverse of `cell_rect`), or `None` if outside.
fn cell_at_pointer(
    canvas: egui::Rect,
    p: egui::Pos2,
    grid: (u16, u16),
    cell: f32,
) -> Option<(u16, u16)> {
    let (cols, rows) = grid;
    if !canvas.contains(p) {
        return None;
    }
    let sx = ((p.x - canvas.min.x) / cell).floor() as i32;
    let sy = ((p.y - canvas.min.y) / cell).floor() as i32;
    if sx < 0 || sy < 0 || sx >= cols as i32 || sy >= rows as i32 {
        return None;
    }
    Some(((cols as i32 - 1 - sx) as u16, (rows as i32 - 1 - sy) as u16))
}

/// Cells along the line from `a` to `b` (Bresenham) — fills gaps when a fast drag skips cells.
fn line_cells(a: (u16, u16), b: (u16, u16)) -> Vec<(u16, u16)> {
    let (mut x0, mut y0) = (a.0 as i32, a.1 as i32);
    let (x1, y1) = (b.0 as i32, b.1 as i32);
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let mut err = dx + dy;
    let mut out = Vec::new();
    loop {
        out.push((x0 as u16, y0 as u16));
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
    out
}

/// R80/R83 — the "Design" tab (hull metadata + grid resize + Move/Mirror + budgets), extracted from the
/// old left panel. R83 — the sections sit SIDE-BY-SIDE as top-aligned COLUMNS (Hull | Grid |
/// Move & mirror | Budgets), each with its own header at the top of its column and its controls stacked
/// beneath; on a very narrow window the row scrolls horizontally instead of mangling. Reads/writes `s`.
fn design_tab(
    ui: &mut egui::Ui,
    s: &mut HullDesignSession,
    cell_materials: &sim::fitting::CellMaterials,
    hp_fb: f32,
    mass_fb: f32,
) {
    egui::ScrollArea::horizontal()
        .id_salt("design_hscroll")
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                // --- Column 1: Hull metadata ---
                ui.vertical(|ui| {
                    ui.strong("Hull");
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut s.working.name)
                                    .desired_width(s.layout.name_field_width),
                            )
                            .changed()
                        {
                            s.dirty = true;
                        }
                    });
                    egui::ComboBox::from_label("Class")
                        .selected_text(format!("{:?}", s.working.class))
                        .show_ui(ui, |ui| {
                            for c in ShipClass::ALL {
                                ui.selectable_value(&mut s.working.class, c, format!("{c:?}"));
                            }
                        });
                    egui::ComboBox::from_label("Role")
                        .selected_text(format!("{:?}", s.working.role))
                        .show_ui(ui, |ui| {
                            for r in ShipRole::ALL {
                                ui.selectable_value(&mut s.working.role, r, format!("{r:?}"));
                            }
                        });
                });
                ui.separator();
                // --- Column 2: Grid resize (staged behind "Apply grid"; a shrink warns) ---
                ui.vertical(|ui| {
                    ui.strong("Grid");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut s.pending_grid.0).range(1..=40));
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut s.pending_grid.1).range(1..=40));
                    });
                    let (pc, pr) = s.pending_grid;
                    if s.pending_grid != s.working.grid_dims {
                        if ui.button("Apply grid").clicked() {
                            s.checkpoint();
                            s.working.grid_dims = s.pending_grid;
                            s.working.cells.retain(|c| c.coord.0 < pc && c.coord.1 < pr);
                            s.working
                                .slots
                                .retain(|sl| sl.coord.0 < pc && sl.coord.1 < pr);
                            if s.selected_cell.is_some_and(|c| c.0 >= pc || c.1 >= pr) {
                                s.selected_cell = None;
                            }
                            s.dirty = true;
                        }
                        let drop_c = s
                            .working
                            .cells
                            .iter()
                            .filter(|c| c.coord.0 >= pc || c.coord.1 >= pr)
                            .count();
                        let drop_s = s
                            .working
                            .slots
                            .iter()
                            .filter(|sl| sl.coord.0 >= pc || sl.coord.1 >= pr)
                            .count();
                        if drop_c > 0 || drop_s > 0 {
                            ui.colored_label(
                                egui::Color32::from_rgb(220, 90, 80),
                                format!("⚠ Apply drops {drop_c} cells · {drop_s} slots"),
                            );
                        }
                    } else {
                        ui.add_enabled(false, egui::Button::new("Apply grid"));
                    }
                });
                ui.separator();
                // --- Column 3: Move (▲ = +row up, ◀ = +col port-left) & mirror (E↔W) ---
                ui.vertical(|ui| {
                    ui.strong("Move & mirror");
                    ui.horizontal(|ui| {
                        if ui.button("◀").clicked() {
                            s.checkpoint();
                            if !shift_design(s, 1, 0) {
                                s.status = "shift blocked (edge)".into();
                            }
                        }
                        if ui.button("▶").clicked() {
                            s.checkpoint();
                            if !shift_design(s, -1, 0) {
                                s.status = "shift blocked (edge)".into();
                            }
                        }
                        if ui.button("▲").clicked() {
                            s.checkpoint();
                            if !shift_design(s, 0, 1) {
                                s.status = "shift blocked (edge)".into();
                            }
                        }
                        if ui.button("▼").clicked() {
                            s.checkpoint();
                            if !shift_design(s, 0, -1) {
                                s.status = "shift blocked (edge)".into();
                            }
                        }
                        if ui.button("Auto-center").clicked() {
                            s.checkpoint();
                            auto_center(s);
                        }
                    });
                    if ui.button("◀ copy left→right").clicked() {
                        s.checkpoint();
                        mirror_design(s, true);
                    }
                    if ui.button("copy right→left ▶").clicked() {
                        s.checkpoint();
                        mirror_design(s, false);
                    }
                });
                ui.separator();
                // --- Column 4: Budgets ---
                ui.vertical(|ui| {
                    ui.strong("Budgets");
                    ui.spacing_mut().slider_width = s.layout.slider_width;
                    for (label, val, range) in [
                        ("Power cap", &mut s.working.power_capacity, 0.0..=500.0),
                        ("CPU cap", &mut s.working.cpu_capacity, 0.0..=500.0),
                        ("Mass cap", &mut s.working.mass_capacity, 0.0..=2000.0),
                        ("Base mass", &mut s.working.hull_base_mass, 0.0..=500.0),
                    ] {
                        ui.add(egui::Slider::new(val, range).text(label));
                    }
                });
            });
        });
    // --- Footer ---
    // R88 — LIVE design stats from the painted materials, mirroring the sim's cell-mass formula
    // ((hull-or-module + armor) × shape area; `cell_mass_with`). Module mass/HP depends on the FITTED
    // modules (not the hull design) → excluded, hence the `~`. Material id 0 = the live struct-cell
    // fallbacks read from the embedded server's SimTuning.
    let mut mass = s.working.hull_base_mass;
    let mut hp = 0.0f32;
    for c in &s.working.cells {
        let area = c.shape.area_factor();
        let armor = cell_materials.armor_params(c.armor_material).mass;
        let has_slot = s.working.slots.iter().any(|sl| sl.coord == c.coord);
        if has_slot {
            mass += armor * area; // the module's own mass comes from the fit — not counted
        } else {
            mass += (cell_materials.hull_mass(c.hull_material, mass_fb) + armor) * area;
            hp += cell_materials.hull_hp(c.hull_material, hp_fb);
        }
    }
    ui.separator();
    ui.label(format!(
        "{} cells · {} slots · mass ~{mass:.1} (base+hull+armor, no modules) · hull HP ~{hp:.0}",
        s.working.cells.len(),
        s.working.slots.len()
    ));
}

/// R79 — the Brush panel's PINNED header (stays put while the palette scrolls): the Tool + Layer icon
/// rows in an aligned `egui::Grid`. (R88 — the colour legend moved out, above the grid.)
/// Reads/writes the [`HullDesignSession`].
fn brush_header(ui: &mut egui::Ui, s: &mut HullDesignSession) {
    // R72/R73 — two axes: a TOOL (the action) + the active LAYER (also the grid view, R73). R79 — the
    // Grid aligns the "Tool:" / "Layer:" labels + their icon columns. R88 — hotkeys in the hover names.
    egui::Grid::new("brush_tools_layer")
        .spacing(egui::vec2(4.0, 4.0))
        .show(ui, |ui| {
            ui.label("Tool:");
            // R78 — drawn icons (Paint dab / Stamp square / Erase ✕ / Select marquee), hover = name.
            for tool in [Tool::Paint, Tool::Stamp, Tool::Erase, Tool::Select] {
                if tool_icon(ui, tool, s.tool == tool, s.layout.icon_px) {
                    s.tool = tool;
                }
            }
            ui.end_row();
            ui.label("Layer:");
            for (layer, which, name) in [
                (Layer::Shape, 0u8, "Shape (1)"),
                (Layer::Hull, 1u8, "Hull material (2)"),
                (Layer::Armor, 2u8, "Armor material (3)"),
                (Layer::Module, 3u8, "Module / hardpoint (4)"),
            ] {
                if layer_icon(ui, which, s.layer == layer, true, name, s.layout.icon_px) {
                    s.layer = layer;
                }
            }
            ui.end_row();
        });
}

/// R71/R79/R88 — the colour LEGEND for the active layer view (a single wrapped row): Shapes = a hint,
/// Hull/Armor = the material swatches + names, Module = the hardpoint-type key. R88 — shown at the TOP
/// of the GRID panel, above the ship (it keys the grid's colours, so it lives with the grid).
fn brush_legend(ui: &mut egui::Ui, layer: Layer, cell_materials: &sim::fitting::CellMaterials) {
    ui.horizontal_wrapped(|ui| match layer {
        Layer::Shape => {
            ui.weak("colour = shape / slot");
        }
        Layer::Hull => {
            for (i, h) in cell_materials.hull.iter().enumerate() {
                material_legend_row(ui, hull_mat_color(i as u8), &format!("{i}: {}", h.name));
            }
        }
        Layer::Armor => {
            for (i, a) in cell_materials.armor.iter().enumerate() {
                material_legend_row(ui, armor_mat_color(i as u8), &format!("{i}: {}", a.name));
            }
        }
        Layer::Module => {
            for ty in MODULE_TYPES {
                material_legend_row(
                    ui,
                    slot_color(ty),
                    &format!("{}  {:?}", slot_initial(ty), ty),
                );
            }
        }
    });
}

/// R79 — the Brush panel's SCROLLING body: the active tool's options/palette + Fill/Clear (split from
/// [`brush_header`] so Tool/Layer/legend stay pinned while this scrolls). Reads/writes the session.
fn brush_options(
    ui: &mut egui::Ui,
    s: &mut HullDesignSession,
    cell_materials: &sim::fitting::CellMaterials,
) {
    // The active tool's options: Paint → the layer's icon palette (the brush); Stamp → presets;
    // Erase/Select → a one-line hint (they act on the active layer too).
    let lay = s.layout;
    match s.tool {
        Tool::Paint => match s.layer {
            Layer::Shape => {
                // (R85 — no header label: every px of panel height goes to the chips + icon cards.)
                shape_palette(
                    ui,
                    "brush",
                    &mut s.brush,
                    &mut s.palette_filter,
                    &mut s.palette_search,
                    lay,
                );
            }
            Layer::Hull => {
                ui.label("Hull material (click-drag to paint):");
                let items: Vec<(egui::Color32, &str)> = cell_materials
                    .hull
                    .iter()
                    .enumerate()
                    .map(|(i, h)| (hull_mat_color(i as u8), h.name.as_str()))
                    .collect();
                swatch_palette(ui, &mut s.hull_material_brush, &items, lay.icon_px);
            }
            Layer::Armor => {
                ui.label("Armor material (click-drag to plate):");
                let items: Vec<(egui::Color32, &str)> = cell_materials
                    .armor
                    .iter()
                    .enumerate()
                    .map(|(i, a)| (armor_mat_color(i as u8), a.name.as_str()))
                    .collect();
                swatch_palette(ui, &mut s.armor_material_brush, &items, lay.icon_px);
            }
            Layer::Module => {
                ui.label("Module type (click-drag to paint; — removes):");
                module_palette(ui, &mut s.module_brush, lay.icon_px);
            }
        },
        Tool::Stamp => {
            // R63 — multi-cell stamp (Shape layer): pick a preset + direction, click to place.
            egui::ComboBox::from_id_salt("stamp_kind")
                .selected_text(s.stamp_kind.label())
                .show_ui(ui, |ui| {
                    for k in StampKind::ALL {
                        ui.selectable_value(&mut s.stamp_kind, k, k.label());
                    }
                });
            ui.horizontal(|ui| {
                ui.label("Dir");
                ui.selectable_value(&mut s.stamp_dir, Dir::N, "N");
                ui.selectable_value(&mut s.stamp_dir, Dir::E, "E");
                ui.selectable_value(&mut s.stamp_dir, Dir::S, "S");
                ui.selectable_value(&mut s.stamp_dir, Dir::W, "W");
            });
            ui.small("click the grid to stamp");
        }
        Tool::Erase => {
            ui.weak(match s.layer {
                Layer::Shape => "click / drag to DELETE cells",
                Layer::Hull => "click / drag to reset hull → Standard",
                Layer::Armor => "click / drag to remove armor",
                Layer::Module => "click / drag to remove the slot",
            });
        }
        Tool::Select => {
            ui.weak("click a cell to inspect / edit it");
        }
    }
    ui.horizontal(|ui| {
        if ui.button("Fill bounding box").clicked() {
            s.checkpoint();
            fill_bounding_box(&mut s.working, s.brush);
            s.dirty = true;
        }
        if ui.button("Clear all").clicked() {
            s.checkpoint();
            s.working.cells.clear();
            s.working.slots.clear();
            s.dirty = true;
        }
    });
}

/// R74/R77 — the shape brush palette: a family-FILTER chip row (`All` plus the 17 families), a name
/// SEARCH box, and a wrapped grid of per-family CARDS of every MATCHING shape (the PARENT panel scrolls —
/// R77 dropped the inner scroll). `id` disambiguates the two instances (the bottom-panel brush + the
/// right-panel selected-cell inspector) so their chips / search field don't collide. Returns true if
/// `current` changed.
fn shape_palette(
    ui: &mut egui::Ui,
    id: &str,
    current: &mut CellShape,
    filter: &mut Option<ShapeFamily>,
    search: &mut String,
    lay: EditorLayout,
) -> bool {
    let before = *current;
    ui.push_id(id, |ui| {
        // Filter chips: All + each family.
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(filter, None, "All");
            for f in ShapeFamily::ALL {
                ui.selectable_value(filter, Some(f), f.label());
            }
        });
        // Name search.
        ui.horizontal(|ui| {
            ui.label("find");
            ui.add(egui::TextEdit::singleline(search).desired_width(lay.search_width));
            if !search.is_empty() && ui.small_button("✕").clicked() {
                search.clear();
            }
        });
        // R76 — render each family as a compact CARD: the family name ONCE, then a WYSIWYG compass where
        // an icon's slot = where its mass is DRAWN (= where it paints). Search nulls non-matching cells
        // (keeping the structural blanks); a family with nothing left is skipped.
        let q = search.trim().to_lowercase();
        let fams: Vec<ShapeFamily> = match *filter {
            Some(f) => vec![f],
            None => ShapeFamily::ALL.to_vec(),
        };
        // R78 — collect the families surviving the filter + search, then lay them out in MANUAL rows
        // sized to the available width. egui's `horizontal_wrapped` does NOT wrap these nested CARD
        // blocks (they ran off the right of the screen, R76/R77) — breaking the rows by hand guarantees
        // the cards never overflow horizontally (the parent panel still scrolls vertically, R77).
        let mut visible: Vec<FamilyCard> = Vec::new();
        for f in &fams {
            let (label, mut subs) = family_layout(*f);
            if !q.is_empty() {
                for (_, cells, _) in subs.iter_mut() {
                    for c in cells.iter_mut() {
                        if let Some(sh) = *c {
                            if !format!("{} {}", f.label(), sh.label())
                                .to_lowercase()
                                .contains(&q)
                            {
                                *c = None;
                            }
                        }
                    }
                }
                subs.retain(|(_, cells, _)| cells.iter().any(Option::is_some));
            }
            if !subs.is_empty() {
                visible.push((family_card_width(*f, lay.icon_px), label, subs));
            }
        }
        if visible.is_empty() {
            ui.weak("no shapes match");
        } else {
            let avail = ui.available_width();
            let gap = ui.spacing().item_spacing.x;
            let mut row: Vec<&FamilyCard> = Vec::new();
            let mut row_w = 0.0f32;
            for card in &visible {
                let would = if row.is_empty() {
                    card.0
                } else {
                    row_w + gap + card.0
                };
                if !row.is_empty() && would > avail {
                    ui.horizontal(|ui| {
                        for c in &row {
                            shape_card(ui, &c.1, &c.2, current, lay.icon_px);
                        }
                    });
                    row.clear();
                    row_w = 0.0;
                }
                row_w = if row.is_empty() {
                    card.0
                } else {
                    row_w + gap + card.0
                };
                row.push(card);
            }
            if !row.is_empty() {
                ui.horizontal(|ui| {
                    for c in &row {
                        shape_card(ui, &c.1, &c.2, current, lay.icon_px);
                    }
                });
            }
        }
    });
    *current != before
}

/// R74 — one shape icon (`px` square, R84 RON-tunable): the shape's real polygon on a dark swatch,
/// `selected`-bordered, hover-named. Returns true on click.
fn shape_icon(ui: &mut egui::Ui, shape: CellShape, selected: bool, px: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(px, px), egui::Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, egui::Color32::from_rgb(30, 34, 42));
    p.add(egui::Shape::convex_polygon(
        shape_poly_in_rect(shape, rect.shrink(3.0)),
        shape_color(shape),
        egui::Stroke::new(1.0, egui::Color32::from_rgb(15, 18, 22)),
    ));
    let stroke = if selected {
        egui::Stroke::new(2.0, egui::Color32::from_rgb(240, 220, 80))
    } else {
        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 66, 78))
    };
    p.rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Inside);
    resp.on_hover_text(shape.label()).clicked()
}

/// R78 — a DRAWN tool icon (`px` square; no font glyph → never tofu): a distinct primitive per [`Tool`]
/// on a dark swatch, `selected`-bordered, hover-named. Returns true on click. (Paint = a dab, Stamp = a
/// square, Erase = a red ✕, Select = a marquee outline.)
fn tool_icon(ui: &mut egui::Ui, tool: Tool, selected: bool, px: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(px, px), egui::Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, egui::Color32::from_rgb(30, 34, 42));
    let c = rect.center();
    let r = rect.width() * 0.28;
    match tool {
        Tool::Paint => {
            p.circle_filled(c, r, egui::Color32::from_rgb(90, 200, 240));
        }
        Tool::Stamp => {
            p.rect_filled(
                egui::Rect::from_center_size(c, egui::vec2(r * 1.9, r * 1.9)),
                1.0,
                egui::Color32::from_rgb(210, 180, 100),
            );
        }
        Tool::Erase => {
            let red = egui::Stroke::new(2.5, egui::Color32::from_rgb(230, 90, 90));
            p.line_segment([c + egui::vec2(-r, -r), c + egui::vec2(r, r)], red);
            p.line_segment([c + egui::vec2(-r, r), c + egui::vec2(r, -r)], red);
        }
        Tool::Select => {
            p.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(r * 2.0, r * 2.0)),
                1.0,
                egui::Stroke::new(1.5, egui::Color32::from_rgb(200, 210, 220)),
                egui::StrokeKind::Inside,
            );
        }
    }
    let stroke = if selected {
        egui::Stroke::new(2.0, egui::Color32::from_rgb(240, 220, 80))
    } else {
        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 66, 78))
    };
    p.rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Inside);
    let name = match tool {
        Tool::Paint => "Paint (Q)",
        Tool::Stamp => "Stamp (W)",
        Tool::Erase => "Erase (E)",
        Tool::Select => "Select (R)",
    };
    resp.on_hover_text(name).clicked()
}

/// R75/R76 — draw one compass: `cells.chunks(cols)` rows of slots — `Some(sh)` → a clickable
/// [`shape_icon`] (sets `*current` on click), `None` → a fixed blank 26-px slot so an icon's POSITION
/// encodes its direction (the slot = where the shape's mass is drawn = where it paints on the grid).
fn draw_compass(
    ui: &mut egui::Ui,
    cells: &[Option<CellShape>],
    cols: usize,
    current: &mut CellShape,
    px: f32,
) {
    ui.vertical(|ui| {
        for row in cells.chunks(cols) {
            ui.horizontal(|ui| {
                for cell in row {
                    match cell {
                        Some(sh) => {
                            if shape_icon(ui, *sh, *current == *sh, px) {
                                *current = *sh;
                            }
                        }
                        None => {
                            ui.allocate_exact_size(egui::vec2(px, px), egui::Sense::hover());
                        }
                    }
                }
            });
        }
    });
}

/// R76 — one family's CARD: a `ui.group` (a `Frame` that shrinks to content → compact, no full-width
/// sprawl) titled with the family name ONCE, then each compass in `subs` side by side. Slope/Wedge get a
/// "shallow" + a "steep" compass under that one name (the family title is what tells the steep groups
/// apart). Sets `*current` on click (tracked by the caller's `before` diff).
fn shape_card(
    ui: &mut egui::Ui,
    family_label: &str,
    subs: &[(Option<&'static str>, Vec<Option<CellShape>>, usize)],
    current: &mut CellShape,
    px: f32,
) {
    ui.group(|ui| {
        ui.vertical(|ui| {
            ui.strong(family_label);
            ui.horizontal(|ui| {
                for (caption, cells, cols) in subs {
                    ui.vertical(|ui| {
                        if let Some(c) = caption {
                            ui.weak(*c);
                        }
                        draw_compass(ui, cells, *cols, current, px);
                    });
                }
            });
        });
    });
}

/// R78 — a built family card ready to lay out: `(estimated px width, family name, compass(es))`.
type FamilyCard = (
    f32,
    String,
    Vec<(Option<&'static str>, Vec<Option<CellShape>>, usize)>,
);

/// R78 — a generous UPPER-BOUND px width for a family's card (for manual row wrapping). Erring HIGH means
/// rows wrap slightly early → cards never overflow the panel width. R84 — scaled by the RON `icon_px`
/// (the estimates were calibrated at 26-px icons).
fn family_card_width(f: ShapeFamily, icon_px: f32) -> f32 {
    let base = match f {
        ShapeFamily::Full | ShapeFamily::Octagon => 80.0,
        ShapeFamily::Half | ShapeFamily::Quarter | ShapeFamily::Chamfer => 95.0,
        ShapeFamily::Strip34
        | ShapeFamily::Strip12
        | ShapeFamily::Strip14
        | ShapeFamily::Strip18
        | ShapeFamily::Point
        | ShapeFamily::Round => 120.0,
        // Slope/Wedge families render a "shallow" + a "steep" 2×2 side by side → widest.
        _ => 160.0,
    };
    base * (icon_px / 26.0).max(0.5)
}

/// R76 — a family's WYSIWYG layout: `(family name, compass(es))`, each compass `(caption, cells, cols)`
/// row-major. An icon's slot = where its mass is DRAWN — the editor draws up = North, left = East (the
/// grid's port-left / nose-up view) — so corners are TL=NE TR=NW BL=SE BR=SW; edges N-top / S-bottom /
/// E-left / W-right; Slope/Wedge = a "shallow" + a "steep" compass; Full/Octagon = a single icon.
fn family_layout(
    f: ShapeFamily,
) -> (
    String,
    Vec<(Option<&'static str>, Vec<Option<CellShape>>, usize)>,
) {
    let s = f.shapes();
    let g = |i: usize| Some(s[i]);
    let subs = match f {
        // No direction → a single icon.
        ShapeFamily::Full | ShapeFamily::Octagon => vec![(None, vec![g(0)], 1)],
        // Corner (shapes() = NW NE SW SE) → one 2×2: TL=NE TR=NW / BL=SE BR=SW.
        ShapeFamily::Half | ShapeFamily::Quarter | ShapeFamily::Chamfer => {
            vec![(None, vec![g(1), g(0), g(3), g(2)], 2)]
        }
        // Corner + shallow/steep (shapes() = NW-H NW-V NE-H NE-V SW-H SW-V SE-H SE-V) → two 2×2 (NE NW / SE SW).
        ShapeFamily::Slope
        | ShapeFamily::Slope3
        | ShapeFamily::Slope4
        | ShapeFamily::Wedge2
        | ShapeFamily::Wedge3
        | ShapeFamily::Wedge4 => vec![
            (Some("shallow"), vec![g(2), g(0), g(6), g(4)], 2), // H
            (Some("steep"), vec![g(3), g(1), g(7), g(5)], 2),   // V
        ],
        // Edge (shapes() = N S E W) → an N/S/E/W cross (3-wide; E left, W right; centre + corners blank).
        ShapeFamily::Strip34
        | ShapeFamily::Strip12
        | ShapeFamily::Strip14
        | ShapeFamily::Strip18
        | ShapeFamily::Point
        | ShapeFamily::Round => vec![(
            None,
            vec![
                None,
                g(0),
                None, // . N .
                g(2),
                None,
                g(3), // E . W
                None,
                g(1),
                None, // . S .
            ],
            3,
        )],
    };
    (f.label().to_string(), subs)
}

/// R66 — the hardpoint types selectable as a module (Armor removed in R66 — it's a per-cell material).
const MODULE_TYPES: [HardpointType; 6] = [
    HardpointType::Reactor,
    HardpointType::Thruster,
    HardpointType::Weapon,
    HardpointType::Shield,
    HardpointType::Sensor,
    HardpointType::Utility,
];

/// R68 — a readable text colour (black/white) for a glyph drawn ON a coloured swatch.
fn text_on(c: egui::Color32) -> egui::Color32 {
    if c.r() as u32 + c.g() as u32 + c.b() as u32 > 380 {
        egui::Color32::BLACK
    } else {
        egui::Color32::WHITE
    }
}

/// R68 — draw one palette icon (`px` square): a coloured swatch with a centred `glyph`,
/// `selected`-bordered, `hover`-tooltipped. Returns true if it was clicked. The shared core of
/// [`swatch_palette`] + [`module_palette`] (mirrors the icon styling of [`shape_palette`]).
fn icon_swatch(
    ui: &mut egui::Ui,
    color: egui::Color32,
    glyph: &str,
    selected: bool,
    hover: &str,
    px: f32,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(px, px), egui::Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, color);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(11.0),
        text_on(color),
    );
    let stroke = if selected {
        egui::Stroke::new(2.0, egui::Color32::from_rgb(240, 220, 80))
    } else {
        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 66, 78))
    };
    p.rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Inside);
    resp.on_hover_text(hover).clicked()
}

/// R72 — draw the representative glyph for a LAYER `which` (`0`=Shape, `1`=Hull, `2`=Armor, `3`=Module)
/// into `inner`, echoing how that layer colours the grid (a chamfer polygon / a hull swatch / an armor
/// border / a module "M"). Shared by [`show_view_icon`] (the "Show:" toolbar) + [`layer_icon`].
fn draw_layer_glyph(p: &egui::Painter, inner: egui::Rect, which: u8) {
    match which {
        0 => {
            p.add(egui::Shape::convex_polygon(
                shape_poly_in_rect(CellShape::ChamferNE, inner),
                shape_color(CellShape::ChamferNE),
                egui::Stroke::NONE,
            ));
        }
        1 => {
            p.rect_filled(inner, 2.0, hull_mat_color(2));
        }
        2 => {
            p.rect_filled(inner, 2.0, egui::Color32::from_rgb(50, 56, 66));
            p.rect_stroke(
                inner,
                2.0,
                egui::Stroke::new(3.0, armor_mat_color(3)),
                egui::StrokeKind::Inside,
            );
        }
        _ => {
            let c = slot_color(HardpointType::Weapon);
            p.rect_filled(inner, 2.0, c);
            p.text(
                inner.center(),
                egui::Align2::CENTER_CENTER,
                "M",
                egui::FontId::proportional(11.0),
                text_on(c),
            );
        }
    }
}

/// R72 — a clickable 26-px LAYER icon (`which` `0..3`) echoing that layer's grid colouring. `selected`
/// gives the yellow border; `enabled == false` greys it + ignores clicks (used for the brush Layer row
/// when the tool isn't Paint). Hover shows `name`. Returns true if clicked. Shared by the "Show:" toolbar
/// ([`show_view_icon`]) + the brush Layer row.
fn layer_icon(
    ui: &mut egui::Ui,
    which: u8,
    selected: bool,
    enabled: bool,
    name: &str,
    px: f32,
) -> bool {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(px, px), sense);
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, egui::Color32::from_rgb(30, 34, 42));
    draw_layer_glyph(&p, rect.shrink(5.0), which);
    if !enabled {
        // Grey veil so a disabled (non-Paint) layer icon reads as inactive.
        p.rect_filled(
            rect,
            3.0,
            egui::Color32::from_rgba_unmultiplied(20, 22, 28, 160),
        );
    }
    let stroke = if selected {
        egui::Stroke::new(2.0, egui::Color32::from_rgb(240, 220, 80))
    } else {
        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 66, 78))
    };
    p.rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Inside);
    resp.on_hover_text(name).clicked()
}

/// R68 — a compact palette of colour-SWATCH icons (hull/armor materials), each labelled by its id;
/// sets `*current` (the material id) on click; returns true if it changed. `items[i] = (colour, name)`.
fn swatch_palette(
    ui: &mut egui::Ui,
    current: &mut u8,
    items: &[(egui::Color32, &str)],
    px: f32,
) -> bool {
    let before = *current;
    ui.horizontal_wrapped(|ui| {
        for (i, (color, name)) in items.iter().enumerate() {
            let label = format!("{i}: {name}");
            if icon_swatch(ui, *color, &format!("{i}"), *current == i as u8, &label, px) {
                *current = i as u8;
            }
        }
    });
    *current != before
}

/// R68 — the MODULE-type icon palette: a "—" (None = no slot) icon + one colour+letter swatch per
/// [`HardpointType`]. Sets `*current` on click; returns true if it changed.
fn module_palette(ui: &mut egui::Ui, current: &mut Option<HardpointType>, px: f32) -> bool {
    let before = *current;
    ui.horizontal_wrapped(|ui| {
        if icon_swatch(
            ui,
            egui::Color32::from_rgb(40, 44, 52),
            "—",
            current.is_none(),
            "None (no slot)",
            px,
        ) {
            *current = None;
        }
        for ty in MODULE_TYPES {
            if icon_swatch(
                ui,
                slot_color(ty),
                slot_initial(ty),
                *current == Some(ty),
                &format!("{ty:?}"),
                px,
            ) {
                *current = Some(ty);
            }
        }
    });
    *current != before
}

/// Set the cell at `coord` to the brush shape (adding it if absent). Keeps `structural` consistent with
/// the slot list (a cell becomes a module cell only when it carries a slot).
fn paint_cell(s: &mut HullDesignSession, coord: (u16, u16)) {
    let has_slot = s.working.slots.iter().any(|sl| sl.coord == coord);
    if let Some(c) = s.working.cells.iter_mut().find(|c| c.coord == coord) {
        c.shape = s.brush;
    } else {
        s.working.cells.push(GridCell {
            coord,
            section: if has_slot {
                SectionId(coord.0 as u32 * 1000 + coord.1 as u32)
            } else {
                SectionId(10000)
            },
            structural: !has_slot,
            shape: s.brush,
            hull_material: 0,
            armor_material: 0,
        });
    }
    s.dirty = true;
}

/// R66 — set the cell at `coord`'s HULL (structural) material to the brush (creating a Full
/// structural cell if absent). A module cell can still carry a hull material (it's ignored for
/// mass, but kept for round-tripping).
fn paint_hull_material(s: &mut HullDesignSession, coord: (u16, u16)) {
    let m = s.hull_material_brush;
    if let Some(c) = s.working.cells.iter_mut().find(|c| c.coord == coord) {
        c.hull_material = m;
    } else {
        let has_slot = s.working.slots.iter().any(|sl| sl.coord == coord);
        s.working.cells.push(GridCell {
            coord,
            section: SectionId(10000),
            structural: !has_slot,
            shape: CellShape::Full,
            hull_material: m,
            armor_material: 0,
        });
    }
    s.dirty = true;
}

/// R66 — set the cell at `coord`'s ARMOR material to the brush (creating a Full structural cell if
/// absent, so you can plate the hull directly).
fn paint_armor_material(s: &mut HullDesignSession, coord: (u16, u16)) {
    let m = s.armor_material_brush;
    if let Some(c) = s.working.cells.iter_mut().find(|c| c.coord == coord) {
        c.armor_material = m;
    } else {
        let has_slot = s.working.slots.iter().any(|sl| sl.coord == coord);
        s.working.cells.push(GridCell {
            coord,
            section: SectionId(10000),
            structural: !has_slot,
            shape: CellShape::Full,
            hull_material: 0,
            armor_material: m,
        });
    }
    s.dirty = true;
}

/// Remove the cell at `coord` and any slot on it.
fn erase_cell(s: &mut HullDesignSession, coord: (u16, u16)) {
    s.working.cells.retain(|c| c.coord != coord);
    s.working.slots.retain(|sl| sl.coord != coord);
    s.dirty = true;
}

/// R73 — clear the active LAYER at `coord`: `Shape` → delete the whole cell; `Hull` → reset to Standard
/// (id 0); `Armor` → None (id 0); `Module` → remove the slot. No-op when the cell/slot is absent (so it
/// never spuriously creates a cell).
fn erase_layer(s: &mut HullDesignSession, coord: (u16, u16), layer: Layer) {
    match layer {
        Layer::Shape => erase_cell(s, coord),
        Layer::Hull => {
            if let Some(c) = s.working.cells.iter_mut().find(|c| c.coord == coord) {
                c.hull_material = 0;
                s.dirty = true;
            }
        }
        Layer::Armor => {
            if let Some(c) = s.working.cells.iter_mut().find(|c| c.coord == coord) {
                c.armor_material = 0;
                s.dirty = true;
            }
        }
        Layer::Module => {
            if s.working.slots.iter().any(|sl| sl.coord == coord) {
                set_cell_module(s, coord, None);
            }
        }
    }
}

/// R68 — set / retype / remove the MODULE (hardpoint slot) at `coord`. `Some(type)` adds a slot (with
/// sensible defaults for a new one) or retypes an existing one and marks the cell a module cell;
/// `None` removes any slot and reverts the cell to structural. Ensures a cell exists first (creates a
/// Full / Standard structural cell). The shared core of [`paint_module`] + the inspector's module edit.
fn set_cell_module(s: &mut HullDesignSession, coord: (u16, u16), ty: Option<HardpointType>) {
    if !s.working.cells.iter().any(|c| c.coord == coord) {
        s.working.cells.push(GridCell {
            coord,
            section: SectionId(10000),
            structural: true,
            shape: CellShape::Full,
            hull_material: 0,
            armor_material: 0,
        });
    }
    match ty {
        Some(t) => {
            if let Some(sl) = s.working.slots.iter_mut().find(|sl| sl.coord == coord) {
                sl.slot_type = t; // retype; keep size/facing/weapon-mount
            } else {
                let id = next_slot_id(&s.working);
                s.working.slots.push(Slot {
                    id,
                    slot_type: t,
                    size: SlotSize::Small,
                    coord,
                    facing: 0.0,
                    is_weapon_mount: t == HardpointType::Weapon,
                });
            }
        }
        None => s.working.slots.retain(|sl| sl.coord != coord),
    }
    // Keep the cell's `structural` flag + section in sync with whether a slot now sits on it.
    let has_slot = s.working.slots.iter().any(|sl| sl.coord == coord);
    if let Some(c) = s.working.cells.iter_mut().find(|c| c.coord == coord) {
        c.structural = !has_slot;
        c.section = if has_slot {
            SectionId(coord.0 as u32 * 1000 + coord.1 as u32)
        } else {
            SectionId(10000)
        };
    }
    s.dirty = true;
}

/// R68 — paint the module brush onto `coord` (the `Module` paint layer).
fn paint_module(s: &mut HullDesignSession, coord: (u16, u16)) {
    set_cell_module(s, coord, s.module_brush);
}

/// R62 — translate EVERY cell + slot by `(dc, dr)`. Returns false (no-op) if any would leave the grid,
/// so nothing is silently lost.
fn shift_design(s: &mut HullDesignSession, dc: i32, dr: i32) -> bool {
    let (cols, rows) = s.working.grid_dims;
    let in_bounds = |coord: (u16, u16)| {
        let (nc, nr) = (coord.0 as i32 + dc, coord.1 as i32 + dr);
        nc >= 0 && nr >= 0 && nc < cols as i32 && nr < rows as i32
    };
    if !s.working.cells.iter().all(|c| in_bounds(c.coord))
        || !s.working.slots.iter().all(|sl| in_bounds(sl.coord))
    {
        return false;
    }
    let mv = |coord: (u16, u16)| ((coord.0 as i32 + dc) as u16, (coord.1 as i32 + dr) as u16);
    for c in &mut s.working.cells {
        c.coord = mv(c.coord);
    }
    for sl in &mut s.working.slots {
        sl.coord = mv(sl.coord);
    }
    s.selected_cell = s.selected_cell.map(mv);
    s.dirty = true;
    true
}

/// R62 — shift the design so its cell bounding box is centred in the grid.
fn auto_center(s: &mut HullDesignSession) {
    if s.working.cells.is_empty() {
        return;
    }
    let (cols, rows) = s.working.grid_dims;
    let min_c = s.working.cells.iter().map(|c| c.coord.0).min().unwrap();
    let max_c = s.working.cells.iter().map(|c| c.coord.0).max().unwrap();
    let min_r = s.working.cells.iter().map(|c| c.coord.1).min().unwrap();
    let max_r = s.working.cells.iter().map(|c| c.coord.1).max().unwrap();
    let dc = (cols as i32 - (max_c - min_c + 1) as i32) / 2 - min_c as i32;
    let dr = (rows as i32 - (max_r - min_r + 1) as i32) / 2 - min_r as i32;
    shift_design(s, dc, dr);
}

/// R63 — make the design bilaterally symmetric across the vertical centre line: keep the cells/slots on
/// one SCREEN half (`from_left` → the screen-left = high-col half) and reflect them onto the other half
/// (`col → cols-1-col`, shape `→ mirror_x()`, slots get fresh ids). The centre column maps to itself.
fn mirror_design(s: &mut HullDesignSession, from_left: bool) {
    let cols = s.working.grid_dims.0 as i32;
    let partner = |c: u16| (cols - 1 - c as i32) as u16;
    // A cell is on the MASTER half if its col is on the chosen screen side of the axis (or on it).
    // Screen-left = high col (the grid is port-left), so `from_left` keeps `col >= partner(col)`.
    let on_master = |c: u16| {
        let pc = partner(c);
        if from_left {
            c >= pc
        } else {
            c <= pc
        }
    };
    s.working.cells.retain(|c| on_master(c.coord.0));
    s.working.slots.retain(|sl| on_master(sl.coord.0));
    for c in s.working.cells.clone() {
        let pc = partner(c.coord.0);
        if pc != c.coord.0 {
            s.working.cells.push(GridCell {
                coord: (pc, c.coord.1),
                section: c.section,
                structural: c.structural,
                shape: c.shape.mirror_x(),
                hull_material: c.hull_material,
                armor_material: c.armor_material,
            });
        }
    }
    let mut next = next_slot_id(&s.working).0;
    for sl in s.working.slots.clone() {
        let pc = partner(sl.coord.0);
        if pc != sl.coord.0 {
            let mut nsl = sl;
            nsl.id = SlotId(next);
            next += 1;
            nsl.coord = (pc, sl.coord.1);
            s.working.slots.push(nsl);
        }
    }
    s.selected_cell = None;
    s.dirty = true;
}

/// R63 — a multi-cell STAMP's per-cell pattern in the CANONICAL orientation (pointing +row / North,
/// anchor `(0,0)` at the base centre). Offsets are `(dcol, drow)`; `apply_stamp` rotates them by `dir`.
fn stamp_cells(kind: StampKind) -> Vec<(i32, i32, CellShape)> {
    use CellShape::*;
    match kind {
        // A long slim sharp triangle: 3-wide base → 45° converge → 1-wide shaft → Point tip.
        StampKind::Blade5 => vec![
            (-1, 0, Full),
            (0, 0, Full),
            (1, 0, Full),
            (-1, 1, HalfNE),
            (0, 1, Full),
            (1, 1, HalfNW),
            (0, 2, Full),
            (0, 3, Full),
            (0, 4, PointN),
        ],
        StampKind::Blade7 => vec![
            (-1, 0, Full),
            (0, 0, Full),
            (1, 0, Full),
            (-1, 1, HalfNE),
            (0, 1, Full),
            (1, 1, HalfNW),
            (0, 2, Full),
            (0, 3, Full),
            (0, 4, Full),
            (0, 5, Full),
            (0, 6, PointN),
        ],
        // A 1-wide pointed spike.
        StampKind::Needle => vec![(0, 0, Full), (0, 1, Full), (0, 2, PointN)],
        // Cones: a wide base converging to a Point apex.
        StampKind::Cone3 => vec![
            (-1, 0, HalfNE),
            (0, 0, Full),
            (1, 0, HalfNW),
            (0, 1, PointN),
        ],
        StampKind::Cone5 => vec![
            (-2, 0, HalfNE),
            (-1, 0, Full),
            (0, 0, Full),
            (1, 0, Full),
            (2, 0, HalfNW),
            (-1, 1, HalfNE),
            (0, 1, Full),
            (1, 1, HalfNW),
            (0, 2, PointN),
        ],
        // Round caps: a rounded front edge.
        StampKind::RoundCap3 => vec![(-1, 0, ChamferNW), (0, 0, RoundN), (1, 0, ChamferNE)],
        StampKind::RoundCap5 => vec![
            (-2, 0, HalfNW),
            (-1, 0, ChamferNW),
            (0, 0, RoundN),
            (1, 0, ChamferNE),
            (2, 0, HalfNE),
        ],
    }
}

/// R63 — paint the selected stamp (oriented by `stamp_dir`) at the clicked `anchor`. Out-of-bounds
/// cells are skipped; in-bounds cells are added/overwritten with the (rotated) shape.
/// R88 — the stamp's per-cell pattern ROTATED to `dir` (factored out of [`apply_stamp`] so the hover
/// GHOST can preview the exact oriented footprint). A CW turn maps `(dc, dr) → (dr, -dc)`.
fn oriented_stamp_cells(kind: StampKind, dir: Dir) -> Vec<(i32, i32, CellShape)> {
    let turns = match dir {
        Dir::N => 0,
        Dir::E => 1,
        Dir::S => 2,
        Dir::W => 3,
    };
    stamp_cells(kind)
        .into_iter()
        .map(|(mut dc, mut dr, mut shape)| {
            for _ in 0..turns {
                (dc, dr) = (dr, -dc);
                shape = shape.rotate_cw();
            }
            (dc, dr, shape)
        })
        .collect()
}

fn apply_stamp(s: &mut HullDesignSession, anchor: (u16, u16)) {
    let (cols, rows) = s.working.grid_dims;
    for (dc, dr, shape) in oriented_stamp_cells(s.stamp_kind, s.stamp_dir) {
        let (nc, nr) = (anchor.0 as i32 + dc, anchor.1 as i32 + dr);
        if nc < 0 || nr < 0 || nc >= cols as i32 || nr >= rows as i32 {
            continue;
        }
        let coord = (nc as u16, nr as u16);
        if let Some(c) = s.working.cells.iter_mut().find(|c| c.coord == coord) {
            c.shape = shape;
        } else {
            s.working.cells.push(GridCell {
                coord,
                section: SectionId(10000),
                structural: true,
                shape,
                hull_material: 0,
                armor_material: 0,
            });
        }
    }
    s.dirty = true;
}

/// Paint every in-bounds coord with the brush shape (a quick way to start a solid block).
fn fill_bounding_box(hull: &mut Hull, brush: CellShape) {
    let (cols, rows) = hull.grid_dims;
    for row in 0..rows {
        for col in 0..cols {
            if let Some(c) = hull.cells.iter_mut().find(|c| c.coord == (col, row)) {
                c.shape = brush;
            } else {
                hull.cells.push(GridCell {
                    coord: (col, row),
                    section: SectionId(10000),
                    structural: true,
                    shape: brush,
                    hull_material: 0,
                    armor_material: 0,
                });
            }
        }
    }
}

/// Edit a slot's type / size / facing / weapon-mount in place.
fn slot_editor(ui: &mut egui::Ui, slot: &mut Slot) {
    // R68 — the slot TYPE is now picked by the `module_palette` icons; this editor covers the rest.
    egui::ComboBox::from_id_salt("slot_size")
        .selected_text(format!("{:?}", slot.size))
        .show_ui(ui, |ui| {
            for sz in [
                SlotSize::Small,
                SlotSize::Medium,
                SlotSize::Large,
                SlotSize::XLarge,
            ] {
                ui.selectable_value(&mut slot.size, sz, format!("{sz:?}"));
            }
        });
    ui.add(
        egui::Slider::new(
            &mut slot.facing,
            -std::f32::consts::PI..=std::f32::consts::PI,
        )
        .text("facing"),
    );
    ui.checkbox(&mut slot.is_weapon_mount, "weapon mount");
}

/// Apply `working` to the live game's `HullCatalog` (and re-derive flying ships on that hull WITHOUT
/// healing). Returns a status string.
fn apply_to_live(host: Option<NonSendMut<LoopbackHost>>, s: &mut HullDesignSession) -> String {
    if let Err(e) = validate_design(&s.working) {
        return format!("Can't apply — {e}");
    }
    let mut hull = s.working.clone();
    normalize_hull(&mut hull);
    s.catalog.hulls.insert(s.selected_hull, hull.clone());
    let Some(mut host) = host else {
        return "no embedded server".to_string();
    };
    let server = &mut host.server;
    if let Some(mut cat) = server.world_mut().get_resource_mut::<HullCatalog>() {
        cat.hulls.insert(s.selected_hull, hull);
    }
    sim::fitting::force_rederive_keep_health(server.world_mut());
    format!("Applied #{} to live", s.selected_hull.0)
}

/// Validate + save the working hull (with the live catalog) to `ships.ron`.
fn save_design(s: &mut HullDesignSession) -> String {
    if let Err(e) = validate_design(&s.working) {
        return format!("Can't save — {e}");
    }
    let mut hull = s.working.clone();
    normalize_hull(&mut hull);
    s.catalog.hulls.insert(s.selected_hull, hull.clone());
    // R65 — write ONLY this ship's own file (not the whole catalog).
    match crate::tuning_io::save_ship(&hull) {
        Ok(m) => m,
        Err(e) => format!("save failed: {e}"),
    }
}

/// R65 — save the working design as a BRAND-NEW ship (fresh id + "(copy)" name) to its own file,
/// leaving the original ship's file untouched. Selects the new ship.
fn save_as_new(s: &mut HullDesignSession) -> String {
    let id = next_hull_id(&s.catalog);
    let mut hull = s.working.clone();
    hull.id = id;
    if hull.name.trim().is_empty() {
        hull.name = "New Hull".to_string();
    }
    hull.name = format!("{} (copy)", hull.name);
    if let Err(e) = validate_design(&hull) {
        return format!("Can't save — {e}");
    }
    normalize_hull(&mut hull);
    s.working = hull.clone();
    s.selected_hull = id;
    s.catalog.hulls.insert(id, hull.clone());
    match crate::tuning_io::save_ship(&hull) {
        Ok(m) => m,
        Err(e) => format!("save failed: {e}"),
    }
}

/// Editor consistency: ≥1 cell, a name, in-bounds cells, and every slot on an authored cell (mirrors
/// `parse_catalogs` so the saved file always re-parses).
fn validate_design(hull: &Hull) -> Result<(), String> {
    if hull.name.trim().is_empty() {
        return Err("name is empty".to_string());
    }
    if hull.cells.is_empty() {
        return Err("no cells".to_string());
    }
    let (cols, rows) = hull.grid_dims;
    for c in &hull.cells {
        if c.coord.0 >= cols || c.coord.1 >= rows {
            return Err(format!("cell {:?} out of bounds", c.coord));
        }
    }
    for sl in &hull.slots {
        if !hull.cells.iter().any(|c| c.coord == sl.coord) {
            return Err(format!("slot #{} not on a cell", sl.id.0));
        }
    }
    Ok(())
}

/// Recompute each cell's `structural` flag from the slot list (a module cell iff a slot sits on it).
fn normalize_hull(hull: &mut Hull) {
    let slot_coords: std::collections::HashSet<(u16, u16)> =
        hull.slots.iter().map(|sl| sl.coord).collect();
    for c in &mut hull.cells {
        c.structural = !slot_coords.contains(&c.coord);
        if c.structural {
            c.section = SectionId(10000);
        }
    }
}

/// A fresh empty hull at `id`.
fn blank_hull(id: HullId) -> Hull {
    Hull {
        id,
        name: format!("New Hull {}", id.0),
        class: ShipClass::Fighter,
        role: ShipRole::Interceptor,
        grid_dims: (9, 11),
        cells: Vec::new(),
        power_capacity: 10.0,
        cpu_capacity: 8.0,
        mass_capacity: 36.0,
        hull_base_mass: 8.0,
        slots: Vec::new(),
    }
}

/// Next authored hull id (below the 1000 runtime-scenario range).
fn next_hull_id(catalog: &HullCatalog) -> HullId {
    let max = catalog
        .hulls
        .keys()
        .map(|h| h.0)
        .filter(|&i| i < 1000)
        .max()
        .unwrap_or(0);
    HullId(max + 1)
}

/// Next slot id within a hull.
fn next_slot_id(hull: &Hull) -> SlotId {
    let max = hull.slots.iter().map(|s| s.id.0).max().unwrap_or(0);
    SlotId(if hull.slots.is_empty() { 0 } else { max + 1 })
}

fn shape_color(shape: CellShape) -> egui::Color32 {
    use CellShape::*;
    match shape {
        Full => egui::Color32::from_rgb(96, 104, 120),
        HalfSW | HalfSE | HalfNE | HalfNW => egui::Color32::from_rgb(70, 110, 150),
        QuarterSW | QuarterSE | QuarterNE | QuarterNW => egui::Color32::from_rgb(70, 140, 90),
        ChamferSW | ChamferSE | ChamferNE | ChamferNW => egui::Color32::from_rgb(170, 120, 60),
        _ => egui::Color32::from_rgb(130, 90, 160), // slopes
    }
}

/// R66/R67 — a HULL-material colour (id 0 = Standard neutral; 1 = Light; 2 = Heavy; 3+ = palette).
/// Used both for the R66 overlay tint dot (id > 0 only) and the R67 `HullMat` grid view + legend.
fn hull_mat_color(id: u8) -> egui::Color32 {
    match id {
        0 => egui::Color32::from_rgb(96, 104, 120), // Standard (neutral)
        1 => egui::Color32::from_rgb(120, 180, 210), // Light
        2 => egui::Color32::from_rgb(210, 150, 90), // Heavy
        _ => material_palette(id),
    }
}

/// R66/R67 — an ARMOR-material colour (id 0 = None dark/empty; 1 Light; 2 Medium; 3 Heavy; 4+ palette).
/// Used for the R66 overlay border (id > 0 only) and the R67 `ArmorMat` grid view + legend.
fn armor_mat_color(id: u8) -> egui::Color32 {
    match id {
        0 => egui::Color32::from_rgb(50, 56, 66), // None (dark / no plate)
        1 => egui::Color32::from_rgb(180, 200, 210), // Light
        2 => egui::Color32::from_rgb(220, 200, 120), // Medium
        3 => egui::Color32::from_rgb(235, 150, 70), // Heavy
        _ => material_palette(id),
    }
}

/// R67 — a distinct colour for any user-added material id (beyond the named defaults).
fn material_palette(id: u8) -> egui::Color32 {
    const P: [(u8, u8, u8); 6] = [
        (150, 110, 200),
        (110, 200, 150),
        (200, 110, 150),
        (150, 200, 110),
        (110, 150, 200),
        (200, 170, 110),
    ];
    let (r, g, b) = P[(id as usize) % P.len()];
    egui::Color32::from_rgb(r, g, b)
}

/// R67 — a legend row: a small colour swatch + the material's name (readable for any swatch colour).
fn material_legend_row(ui: &mut egui::Ui, color: egui::Color32, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 2.0, color);
        ui.label(text);
    });
}

fn slot_color(ty: HardpointType) -> egui::Color32 {
    match ty {
        HardpointType::Reactor => egui::Color32::from_rgb(200, 80, 200),
        HardpointType::Thruster => egui::Color32::from_rgb(220, 140, 40),
        HardpointType::Weapon => egui::Color32::from_rgb(210, 70, 70),
        HardpointType::Shield => egui::Color32::from_rgb(70, 160, 210),
        HardpointType::Armor => egui::Color32::from_rgb(150, 150, 150),
        HardpointType::Sensor => egui::Color32::from_rgb(90, 200, 160),
        HardpointType::Utility => egui::Color32::from_rgb(120, 120, 90),
    }
}

fn slot_initial(ty: HardpointType) -> &'static str {
    match ty {
        HardpointType::Reactor => "R",
        HardpointType::Thruster => "T",
        HardpointType::Weapon => "W",
        HardpointType::Shield => "S",
        HardpointType::Armor => "A",
        HardpointType::Sensor => "N",
        HardpointType::Utility => "U",
    }
}
