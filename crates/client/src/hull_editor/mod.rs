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
    CellShape, GridCell, HardpointType, Hull, HullCatalog, HullId, ModuleCatalog, SectionId,
    ShipClass, ShipRole, Slot, SlotId, SlotSize, HULL_FIGHTER,
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
            .add_plugins(PreviewPlugin);
    }
}

/// What a left-click on the grid does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditMode {
    /// Set the clicked cell's shape to the brush (adding the cell if absent).
    Paint,
    /// Remove the clicked cell (and any slot on it).
    Erase,
    /// Just select the clicked cell (inspect / edit in the right panel).
    Select,
}

/// The editor's working state — a COPY of the live hull catalog plus the hull being edited. Nothing
/// touches the running game until "Apply to live" / "Save".
#[derive(Resource)]
pub struct HullDesignSession {
    /// The live hull catalog (cloned on enter); the working hull is committed back into it on apply/save.
    catalog: HullCatalog,
    /// The module catalog (for the save round-trip; the editor authors no modules).
    modules: ModuleCatalog,
    /// The hull currently being edited.
    working: Hull,
    /// Which catalog id `working` is (so apply/save write the right row).
    selected_hull: HullId,
    /// The shape painted by left-click in `Paint` mode.
    brush: CellShape,
    mode: EditMode,
    selected_cell: Option<(u16, u16)>,
    selected_slot: Option<SlotId>,
    status: String,
    /// Set on any edit → the 3-D preview rebuilds its mesh next frame.
    pub dirty: bool,
    /// Preview camera orbit (yaw, pitch) in radians, dragged on the preview image.
    pub orbit: (f32, f32),
}

impl Default for HullDesignSession {
    fn default() -> Self {
        let (modules, catalog) = sim::fitting::seed_catalogs();
        let working = catalog
            .get(HULL_FIGHTER)
            .cloned()
            .unwrap_or_else(|| blank_hull(HULL_FIGHTER));
        Self {
            catalog,
            modules,
            working,
            selected_hull: HULL_FIGHTER,
            brush: CellShape::Full,
            mode: EditMode::Paint,
            selected_cell: None,
            selected_slot: None,
            status: String::new(),
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
        if let Some(m) = w.get_resource::<ModuleCatalog>() {
            session.modules = m.clone();
        }
        if let Some(h) = w.get_resource::<HullCatalog>() {
            session.catalog = h.clone();
        }
    }
    let id = session.selected_hull;
    if let Some(h) = session.catalog.get(id).cloned() {
        session.working = h;
    }
    session.selected_cell = None;
    session.selected_slot = None;
    session.dirty = true;
    session.status.clear();
}

/// The editor screen (egui), drawn every frame while `Designing`.
fn hull_editor_ui(
    mut contexts: EguiContexts,
    mut session: ResMut<HullDesignSession>,
    host: Option<NonSendMut<LoopbackHost>>,
    mut next_state: ResMut<NextState<HullDesignState>>,
    preview: Res<preview::PreviewTarget>,
) {
    // Register the preview render-target as an egui texture (must borrow `contexts` BEFORE `ctx_mut`).
    let preview_tex = contexts.image_id(&preview.image).unwrap_or_else(|| {
        contexts.add_image(bevy_egui::EguiTextureHandle::Strong(preview.image.clone()))
    });
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let s = &mut *session;

    // Intents collected in the panel closures, executed after (so the closures don't hold `host`).
    let mut do_apply = false;
    let mut do_save = false;
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
                    s.working = h;
                    s.selected_hull = pick;
                    s.selected_cell = None;
                    s.selected_slot = None;
                    s.dirty = true;
                }
            }
            if ui.button("New blank hull").clicked() {
                let id = next_hull_id(&s.catalog);
                s.working = blank_hull(id);
                s.selected_hull = id;
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
            if ui.button("Save → ships.ron").clicked() {
                do_save = true;
            }
            if ui.button("Close").clicked() {
                do_close = true;
            }
        });
        if !s.status.is_empty() {
            ui.label(&s.status);
        }
    });

    // ---- Left: metadata + brush ----
    egui::SidePanel::left("hull_editor_meta")
        .default_width(220.0)
        .show(ctx, |ui| {
            ui.heading("Hull");
            ui.horizontal(|ui| {
                ui.label("Name");
                if ui.text_edit_singleline(&mut s.working.name).changed() {
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
            ui.separator();
            ui.label("Grid (cols × rows)");
            ui.horizontal(|ui| {
                let mut cols = s.working.grid_dims.0;
                let mut rows = s.working.grid_dims.1;
                let c = ui.add(egui::DragValue::new(&mut cols).range(1..=40));
                let r = ui.add(egui::DragValue::new(&mut rows).range(1..=40));
                if c.changed() || r.changed() {
                    s.working.grid_dims = (cols, rows);
                    // Drop cells/slots now out of bounds.
                    s.working
                        .cells
                        .retain(|cell| cell.coord.0 < cols && cell.coord.1 < rows);
                    s.working
                        .slots
                        .retain(|sl| sl.coord.0 < cols && sl.coord.1 < rows);
                    s.dirty = true;
                }
            });
            ui.separator();
            ui.label("Budgets");
            for (label, val, range) in [
                ("Power cap", &mut s.working.power_capacity, 0.0..=500.0),
                ("CPU cap", &mut s.working.cpu_capacity, 0.0..=500.0),
                ("Mass cap", &mut s.working.mass_capacity, 0.0..=2000.0),
                ("Base mass", &mut s.working.hull_base_mass, 0.0..=500.0),
            ] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    ui.add(egui::Slider::new(val, range));
                });
            }
            ui.separator();
            ui.heading("Brush");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.mode, EditMode::Paint, "Paint");
                ui.selectable_value(&mut s.mode, EditMode::Erase, "Erase");
                ui.selectable_value(&mut s.mode, EditMode::Select, "Select");
            });
            egui::ComboBox::from_label("Shape")
                .selected_text(s.brush.label())
                .show_ui(ui, |ui| {
                    for sh in CellShape::ALL {
                        ui.selectable_value(&mut s.brush, sh, sh.label());
                    }
                });
            ui.horizontal(|ui| {
                if ui.button("Fill bounding box").clicked() {
                    fill_bounding_box(&mut s.working, s.brush);
                    s.dirty = true;
                }
                if ui.button("Clear all").clicked() {
                    s.working.cells.clear();
                    s.working.slots.clear();
                    s.dirty = true;
                }
            });
            ui.separator();
            ui.label(format!(
                "{} cells · {} slots",
                s.working.cells.len(),
                s.working.slots.len()
            ));
        });

    // ---- Right: selected-cell + slots + 3-D preview ----
    egui::SidePanel::right("hull_editor_inspect")
        .default_width(300.0)
        .show(ctx, |ui| {
            // 3-D preview pane (render-to-texture); drag to orbit.
            ui.heading("Preview");
            let size = egui::vec2(280.0, 280.0);
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
                    egui::ComboBox::from_id_salt("cell_shape")
                        .selected_text(shape.label())
                        .show_ui(ui, |ui| {
                            for sh in CellShape::ALL {
                                ui.selectable_value(&mut shape, sh, sh.label());
                            }
                        });
                    if shape != s.working.cells[idx].shape {
                        s.working.cells[idx].shape = shape;
                        s.dirty = true;
                    }
                    // Slot on this cell?
                    if let Some(sidx) = s.working.slots.iter().position(|sl| sl.coord == coord) {
                        ui.separator();
                        ui.label(format!("Slot #{}", s.working.slots[sidx].id.0));
                        slot_editor(ui, &mut s.working.slots[sidx]);
                        if ui.button("Remove slot").clicked() {
                            s.working.slots.remove(sidx);
                            s.dirty = true;
                        }
                    } else if ui.button("Add slot here").clicked() {
                        let id = next_slot_id(&s.working);
                        s.working.slots.push(Slot {
                            id,
                            slot_type: HardpointType::Weapon,
                            size: SlotSize::Small,
                            coord,
                            facing: 0.0,
                            is_weapon_mount: true,
                        });
                        s.dirty = true;
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

    // ---- Center: the cell-grid painter ----
    egui::CentralPanel::default().show(ctx, |ui| {
        egui::ScrollArea::both().show(ui, |ui| {
            draw_grid(ui, s);
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
    }
}

/// Draw the editable cell grid (nose-up / port-left, matching the fitting view + in-game).
fn draw_grid(ui: &mut egui::Ui, s: &mut HullDesignSession) {
    let (cols, rows) = s.working.grid_dims;
    const CELL: f32 = 24.0;
    egui::Grid::new("hull_grid")
        .spacing(egui::vec2(2.0, 2.0))
        .show(ui, |ui| {
            for row in (0..rows).rev() {
                for col in (0..cols).rev() {
                    let coord = (col, row);
                    let cell = s.working.cells.iter().find(|c| c.coord == coord).copied();
                    let slot = s.working.slots.iter().find(|sl| sl.coord == coord).copied();
                    let selected = s.selected_cell == Some(coord);

                    let (fill, text) = match (cell, slot) {
                        (Some(_), Some(sl)) => {
                            (slot_color(sl.slot_type), slot_initial(sl.slot_type))
                        }
                        (Some(c), None) => (shape_color(c.shape), shape_code(c.shape)),
                        (None, _) => (egui::Color32::from_rgb(26, 30, 38), ""),
                    };
                    let mut btn = egui::Button::new(egui::RichText::new(text).size(10.0))
                        .fill(fill)
                        .min_size(egui::vec2(CELL, CELL));
                    if selected {
                        btn = btn.stroke(egui::Stroke::new(
                            2.0,
                            egui::Color32::from_rgb(240, 220, 80),
                        ));
                    }
                    let resp = ui.add(btn);
                    if resp.clicked() {
                        s.selected_cell = Some(coord);
                        match s.mode {
                            EditMode::Paint => paint_cell(s, coord),
                            EditMode::Erase => erase_cell(s, coord),
                            EditMode::Select => {}
                        }
                    }
                    if resp.secondary_clicked() {
                        s.selected_cell = Some(coord);
                        erase_cell(s, coord);
                    }
                }
                ui.end_row();
            }
        });
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
                });
            }
        }
    }
}

/// Edit a slot's type / size / facing / weapon-mount in place.
fn slot_editor(ui: &mut egui::Ui, slot: &mut Slot) {
    egui::ComboBox::from_id_salt("slot_type")
        .selected_text(format!("{:?}", slot.slot_type))
        .show_ui(ui, |ui| {
            for t in [
                HardpointType::Reactor,
                HardpointType::Thruster,
                HardpointType::Weapon,
                HardpointType::Shield,
                HardpointType::Armor,
                HardpointType::Sensor,
                HardpointType::Utility,
            ] {
                ui.selectable_value(&mut slot.slot_type, t, format!("{t:?}"));
            }
        });
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
    s.catalog.hulls.insert(s.selected_hull, hull);
    match crate::tuning_io::save_catalogs(Some(&s.modules), Some(&s.catalog)) {
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

fn shape_code(shape: CellShape) -> &'static str {
    use CellShape::*;
    match shape {
        Full => "",
        HalfSW => "◣",
        HalfSE => "◢",
        HalfNE => "◥",
        HalfNW => "◤",
        QuarterSW | QuarterSE | QuarterNE | QuarterNW => "q",
        ChamferSW | ChamferSE | ChamferNE | ChamferNW => "c",
        _ => "s",
    }
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
