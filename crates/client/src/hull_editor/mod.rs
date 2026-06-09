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
    /// R63 — stamp a multi-cell preset (round cap / cone / blade / needle) at the clicked anchor.
    Stamp,
    /// R66 — paint the clicked cell's HULL (structural) material id (light/heavy hull).
    HullMat,
    /// R66 — paint the clicked cell's ARMOR material id (none/light/medium/heavy plating).
    Armor,
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
    /// The shape painted by left-click in `Paint` mode.
    brush: CellShape,
    mode: EditMode,
    selected_cell: Option<(u16, u16)>,
    selected_slot: Option<SlotId>,
    /// R61 — staged grid size (the cols/rows fields edit this; "Apply grid" commits it so editing the
    /// numbers never wipes the design until applied).
    pending_grid: (u16, u16),
    /// R61 — the last cell painted during a click-drag (so a held drag fills continuously without
    /// re-painting the same cell, and a fast drag can line-fill between successive cells).
    last_painted: Option<(u16, u16)>,
    /// R63 — the family shown in the compact shape palette.
    palette_family: ShapeFamily,
    /// R63 — the selected multi-cell stamp + its direction (used in `Stamp` mode).
    stamp_kind: StampKind,
    stamp_dir: Dir,
    /// R66 — the hull/armor material ids painted in `HullMat`/`Armor` mode.
    hull_material_brush: u8,
    armor_material_brush: u8,
    status: String,
    /// Set on any edit → the 3-D preview rebuilds its mesh next frame.
    pub dirty: bool,
    /// Preview camera orbit (yaw, pitch) in radians, dragged on the preview image.
    pub orbit: (f32, f32),
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
            mode: EditMode::Paint,
            selected_cell: None,
            selected_slot: None,
            pending_grid,
            last_painted: None,
            palette_family: ShapeFamily::Full,
            stamp_kind: StampKind::Blade5,
            stamp_dir: Dir::N,
            hull_material_brush: 2,  // Heavy (so HullMat painting is visible)
            armor_material_brush: 1, // Light
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
    let s = &mut *session;

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
                ui.add(egui::DragValue::new(&mut s.pending_grid.0).range(1..=40));
                ui.label("×");
                ui.add(egui::DragValue::new(&mut s.pending_grid.1).range(1..=40));
            });
            // R61 — STAGED: editing the numbers above changes nothing until "Apply grid" (so a resize
            // never silently wipes the design); a shrink warns how many cells/slots it would drop.
            let (pc, pr) = s.pending_grid;
            if s.pending_grid != s.working.grid_dims {
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
                if ui.button("Apply grid").clicked() {
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
            } else {
                ui.add_enabled(false, egui::Button::new("Apply grid"));
            }
            // R62 — shift the WHOLE design (cells + slots) to re-centre after a grid grow. Screen
            // orientation: ▲ = +row (up), ◀ = +col (port-left), per the nose-up / port-left grid.
            ui.label("Move design");
            ui.horizontal(|ui| {
                if ui.button("◀").clicked() && !shift_design(s, 1, 0) {
                    s.status = "shift blocked (edge)".into();
                }
                if ui.button("▶").clicked() && !shift_design(s, -1, 0) {
                    s.status = "shift blocked (edge)".into();
                }
                if ui.button("▲").clicked() && !shift_design(s, 0, 1) {
                    s.status = "shift blocked (edge)".into();
                }
                if ui.button("▼").clicked() && !shift_design(s, 0, -1) {
                    s.status = "shift blocked (edge)".into();
                }
                if ui.button("Auto-center").clicked() {
                    auto_center(s);
                }
            });
            // R63 — mirror one screen-half onto the other across the centre line (cells + slots +
            // each cell's shape are reflected E↔W). Screen-left = high col (the grid is port-left).
            ui.label("Mirror across center");
            ui.horizontal(|ui| {
                if ui.button("◀ copy left→right").clicked() {
                    mirror_design(s, true);
                }
                if ui.button("copy right→left ▶").clicked() {
                    mirror_design(s, false);
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
                ui.selectable_value(&mut s.mode, EditMode::Stamp, "Stamp");
            });
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.mode, EditMode::HullMat, "Hull mat");
                ui.selectable_value(&mut s.mode, EditMode::Armor, "Armor");
            });
            if s.mode == EditMode::HullMat {
                // R66 — pick a HULL material; click-drag the grid to paint it onto cells.
                ui.label("Hull material (click-drag to paint):");
                egui::ComboBox::from_id_salt("hull_mat_brush")
                    .selected_text(mat_name(
                        cell_materials
                            .hull
                            .get(s.hull_material_brush as usize)
                            .map(|h| h.name.as_str()),
                        s.hull_material_brush,
                    ))
                    .show_ui(ui, |ui| {
                        for (i, h) in cell_materials.hull.iter().enumerate() {
                            ui.selectable_value(
                                &mut s.hull_material_brush,
                                i as u8,
                                format!("{i}: {}", h.name),
                            );
                        }
                    });
            } else if s.mode == EditMode::Armor {
                // R66 — pick an ARMOR material; click-drag the grid to plate cells.
                ui.label("Armor material (click-drag to paint):");
                egui::ComboBox::from_id_salt("armor_mat_brush")
                    .selected_text(mat_name(
                        cell_materials
                            .armor
                            .get(s.armor_material_brush as usize)
                            .map(|a| a.name.as_str()),
                        s.armor_material_brush,
                    ))
                    .show_ui(ui, |ui| {
                        for (i, a) in cell_materials.armor.iter().enumerate() {
                            ui.selectable_value(
                                &mut s.armor_material_brush,
                                i as u8,
                                format!("{i}: {}", a.name),
                            );
                        }
                    });
            } else if s.mode == EditMode::Stamp {
                // R63 — multi-cell stamp: pick a preset + direction, click the grid to place it.
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
            } else {
                ui.label("Shape (click an icon)");
                shape_palette(ui, &mut s.brush, &mut s.palette_family);
            }
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
                    if shape_palette(ui, &mut shape, &mut s.palette_family) {
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
    } else if do_save_new {
        s.status = save_as_new(s);
    }
}

/// R61 — the editable cell grid as ONE painted CANVAS (nose-up / port-left, matching the fitting view +
/// in-game): each present cell is drawn as its REAL `CellShape` polygon (so the design reads at a glance),
/// and a single `click_and_drag` interaction lets Paint/Erase modes drag a swath across the grid.
fn draw_grid(ui: &mut egui::Ui, s: &mut HullDesignSession) {
    let (cols, rows) = s.working.grid_dims;
    const CELL: f32 = 26.0;
    let size = egui::vec2(cols as f32 * CELL, rows as f32 * CELL);
    let (canvas, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let painter = ui.painter_at(canvas);

    let cell_rect = |col: u16, row: u16| {
        // Port-left (high col on the LEFT) + nose-up (high row at the TOP).
        let x = canvas.min.x + (cols - 1 - col) as f32 * CELL;
        let y = canvas.min.y + (rows - 1 - row) as f32 * CELL;
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(CELL, CELL))
    };
    let hovered = resp
        .hover_pos()
        .and_then(|p| cell_at_pointer(canvas, p, (cols, rows), CELL));

    for row in 0..rows {
        for col in 0..cols {
            let coord = (col, row);
            let rect = cell_rect(col, row);
            painter.rect_filled(rect.shrink(0.5), 2.0, egui::Color32::from_rgb(22, 26, 33));
            if let Some(c) = s.working.cells.iter().find(|c| c.coord == coord).copied() {
                let slot = s.working.slots.iter().find(|sl| sl.coord == coord).copied();
                let fill = slot.map_or_else(|| shape_color(c.shape), |sl| slot_color(sl.slot_type));
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
                // R66 — material overlay: a hull-material tint dot (top-left) + an armor-material
                // border so the painted hull/armor layout reads at a glance.
                if c.hull_material > 0 {
                    painter.circle_filled(
                        rect.left_top() + egui::vec2(5.0, 5.0),
                        3.0,
                        hull_mat_color(c.hull_material),
                    );
                }
                if c.armor_material > 0 {
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

    // Right-click → erase one cell.
    if resp.secondary_clicked() {
        if let Some(coord) = resp
            .hover_pos()
            .and_then(|p| cell_at_pointer(canvas, p, (cols, rows), CELL))
        {
            s.selected_cell = Some(coord);
            erase_cell(s, coord);
        }
    }
    // Primary click / DRAG → paint / erase / select (line-filled between frames so a fast drag has no gaps).
    if resp.dragged() || resp.clicked() {
        if let Some(coord) = resp
            .interact_pointer_pos()
            .and_then(|p| cell_at_pointer(canvas, p, (cols, rows), CELL))
        {
            s.selected_cell = Some(coord);
            match s.mode {
                EditMode::Paint | EditMode::Erase => {
                    let from = s.last_painted.unwrap_or(coord);
                    for cc in line_cells(from, coord) {
                        if matches!(s.mode, EditMode::Paint) {
                            paint_cell(s, cc);
                        } else {
                            erase_cell(s, cc);
                        }
                    }
                    s.last_painted = Some(coord);
                }
                // R66 — paint a hull/armor MATERIAL onto cells along the drag.
                EditMode::HullMat | EditMode::Armor => {
                    let from = s.last_painted.unwrap_or(coord);
                    for cc in line_cells(from, coord) {
                        if matches!(s.mode, EditMode::HullMat) {
                            paint_hull_material(s, cc);
                        } else {
                            paint_armor_material(s, cc);
                        }
                    }
                    s.last_painted = Some(coord);
                }
                // Stamp places on a fresh CLICK only (a drag mustn't re-stamp every frame).
                EditMode::Stamp => {
                    if resp.clicked() {
                        apply_stamp(s, coord);
                    }
                }
                EditMode::Select => {}
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

/// R63 — a COMPACT shape palette: a wrapped row of FAMILY chips, then only the SELECTED family's
/// orientation icons (each drawn as its real polygon). Keeps the panel narrow regardless of shape count.
/// Sets `*current` on click; returns true if it changed.
fn shape_palette(ui: &mut egui::Ui, current: &mut CellShape, family: &mut ShapeFamily) -> bool {
    let before = *current;
    // Family chips.
    ui.horizontal_wrapped(|ui| {
        for f in ShapeFamily::ALL {
            ui.selectable_value(family, f, f.label());
        }
    });
    // The selected family's orientation icons.
    ui.horizontal_wrapped(|ui| {
        for shape in family.shapes() {
            const ICON: f32 = 26.0;
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ICON, ICON), egui::Sense::click());
            let selected = *current == shape;
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
            if resp.on_hover_text(shape.label()).clicked() {
                *current = shape;
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
fn apply_stamp(s: &mut HullDesignSession, anchor: (u16, u16)) {
    let turns = match s.stamp_dir {
        Dir::N => 0,
        Dir::E => 1,
        Dir::S => 2,
        Dir::W => 3,
    };
    let (cols, rows) = s.working.grid_dims;
    for (dc0, dr0, shape0) in stamp_cells(s.stamp_kind) {
        // Rotate the offset + the shape `turns` times CW (a CW turn maps (dc,dr) → (dr,-dc)).
        let (mut dc, mut dr, mut shape) = (dc0, dr0, shape0);
        for _ in 0..turns {
            (dc, dr) = (dr, -dc);
            shape = shape.rotate_cw();
        }
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
    egui::ComboBox::from_id_salt("slot_type")
        .selected_text(format!("{:?}", slot.slot_type))
        .show_ui(ui, |ui| {
            for t in [
                HardpointType::Reactor,
                HardpointType::Thruster,
                HardpointType::Weapon,
                HardpointType::Shield,
                // R66 — Armor is no longer a hardpoint slot type; paint per-cell armor MATERIAL
                // instead (the "Armor" brush mode). `HardpointType::Armor` stays vestigial elsewhere.
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

/// R66 — the brush picker label for a material id (`"{id}: {name}"`; `?` if out of range).
fn mat_name(name: Option<&str>, id: u8) -> String {
    match name {
        Some(n) => format!("{id}: {n}"),
        None => format!("{id}: ?"),
    }
}

/// R66 — a HULL-material overlay tint (id-based ramp; 1 = Light blue-grey … darker = heavier).
fn hull_mat_color(id: u8) -> egui::Color32 {
    match id {
        1 => egui::Color32::from_rgb(120, 180, 210), // Light
        2 => egui::Color32::from_rgb(210, 150, 90),  // Heavy
        _ => egui::Color32::from_rgb(200, 200, 120),
    }
}

/// R66 — an ARMOR-material overlay border colour (id-based ramp; warmer = heavier plating).
fn armor_mat_color(id: u8) -> egui::Color32 {
    match id {
        1 => egui::Color32::from_rgb(180, 200, 210), // Light
        2 => egui::Color32::from_rgb(220, 200, 120), // Medium
        3 => egui::Color32::from_rgb(235, 150, 70),  // Heavy
        _ => egui::Color32::from_rgb(235, 110, 110),
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
