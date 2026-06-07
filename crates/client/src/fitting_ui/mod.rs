//! The interactive ship-fitting screen (E006 US5 + Refinement 44).
//!
//! R44 rebuilt this as an **EVE-style egui modal**: the player's ship is drawn as a SILHOUETTE (its
//! hull cells laid out on the grid) with clickable **slot markers** at their hull positions. Clicking
//! a slot lists the **compatible** modules from the inventory ("cargo") to install; live power/CPU/mass
//! budget bars + a before-commit preview show the result; an **Apply** button commits the edited fit
//! to the LIVE embedded-server ship.
//!
//! All gameplay logic stays in `sim` (install legality `Fit::install_module` + `check_slot_fit`,
//! budget/stat derivation `preview_stats`, weapon stats `derive_weapon`); this module only draws +
//! routes input. It carries no authoritative fit truth until the player presses Apply.
//!
//! egui 0.39 note: UI runs in [`EguiPrimaryContextPass`]; [`EguiContexts::ctx_mut`] returns a
//! `Result` (we early-return before the context exists — e.g. a headless test with no window).

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};
use sim::components::{FireMapping, Trigger, WeaponGroups};
use sim::fitting::{
    check_slot_fit, derive_weapon, preview_stats, seed_catalogs, validate_fit, Fit, FitRejection,
    HardpointType, Hull, HullCatalog, Module, ModuleCatalog, ModuleId, SlotId, HULL_FIGHTER,
};

use crate::net::{LoopbackHost, NetClientState};

/// The client app-state toggling the flying view and the interactive fitting screen (FR-012). `run()`
/// binds a key (Tab) that flips `Flying ⇄ Fitting`; the screen's systems are gated on `Fitting`.
#[derive(States, Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FittingScreenState {
    /// The normal flying / combat view (default). The fitting modal is hidden.
    #[default]
    Flying,
    /// The interactive fitting screen is open. Flight input still runs behind it.
    Fitting,
}

/// The interactive fitting screen plugin (R44). On entering `Fitting`, [`load_fitting_session`] pulls
/// the player's LIVE loadout + catalog into the [`FittingSession`]; while open, [`fitting_screen_ui`]
/// draws the egui modal (silhouette + inventory) and commits edits to the live ship on Apply.
pub struct FittingUiPlugin;

impl Plugin for FittingUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<FittingScreenState>()
            .init_resource::<FittingSession>()
            .init_resource::<Inventory>()
            // Data-only load on enter (no UI); the egui draw runs every frame while open.
            .add_systems(OnEnter(FittingScreenState::Fitting), load_fitting_session)
            .add_systems(
                EguiPrimaryContextPass,
                fitting_screen_ui.run_if(in_state(FittingScreenState::Fitting)),
            );
    }
}

/// R43/R44 — the player's available modules ("cargo / inventory") the screen installs from. For now
/// it is EVERY catalog module (refreshed on enter); a real game limits it to OWNED items. Installing
/// is gated by ship capabilities (hardpoint type/size + budget) via `Fit::install_module`.
#[derive(Resource, Default)]
pub struct Inventory {
    /// Owned module ids in display order (sorted by id — `ModuleCatalog` is a `BTreeMap`).
    pub available: Vec<ModuleId>,
}

/// The screen's working state — the in-progress [`Fit`] the player edits (a COPY of the live ship's
/// fit), the catalogs `sim` resolves ids against, the selected slot, and the last action's message.
/// Client-only: edits never touch the running ship until **Apply**.
#[derive(Resource)]
pub struct FittingSession {
    pub modules: ModuleCatalog,
    pub hulls: HullCatalog,
    /// The fit being edited (a copy of the live ship's fit, loaded on enter).
    pub working_fit: Fit,
    /// The committed baseline the preview deltas measure against (rebased on Apply).
    pub applied_fit: Fit,
    /// The slot the player clicked — drives the compatible-module list on the right.
    pub selected_slot: Option<SlotId>,
    /// R45 — the working fire-group assignment (slot → group/trigger), loaded from the live ship on
    /// enter, edited via the slot panel, committed alongside the `Fit` on Apply. Slots absent from
    /// the map default to group 1 / Primary (so an unconfigured ship fires everything on Space).
    pub weapon_groups: WeaponGroups,
    /// Human-readable outcome of the last action (install/remove/apply rejection or success).
    pub status: String,
}

impl Default for FittingSession {
    fn default() -> Self {
        let (modules, hulls) = seed_catalogs();
        let working_fit = Fit::new(HULL_FIGHTER);
        let applied_fit = working_fit.clone();
        Self {
            modules,
            hulls,
            working_fit,
            applied_fit,
            selected_slot: None,
            weapon_groups: WeaponGroups::default(),
            status: String::new(),
        }
    }
}

impl FittingSession {
    /// The hull the working fit is built on (resolved in the catalog).
    fn working_hull(&self) -> Option<&Hull> {
        self.hulls.get(self.working_fit.hull)
    }
}

/// `OnEnter(Fitting)` — DATA only (no UI): refresh the catalog/hulls from the LIVE embedded server,
/// load the player ship's current `Fit` into `working`/`applied` (so you edit your REAL loadout), and
/// rebuild the inventory ("cargo") from the catalog. Windowed-only — the player ship lives only here.
fn load_fitting_session(
    mut session: ResMut<FittingSession>,
    mut inventory: ResMut<Inventory>,
    host: Option<NonSend<LoopbackHost>>,
    state: Option<NonSend<NetClientState>>,
) {
    if let Some(host) = host {
        let w = host.server.world();
        if let Some(m) = w.get_resource::<ModuleCatalog>() {
            session.modules = m.clone();
        }
        if let Some(h) = w.get_resource::<HullCatalog>() {
            session.hulls = h.clone();
        }
        if let Some(ship) = state
            .as_ref()
            .and_then(|s| host.server.ship_entity_for(s.local_id))
        {
            if let Some(fit) = w.get::<Fit>(ship) {
                session.working_fit = fit.clone();
                session.applied_fit = fit.clone();
            }
            // R45 — load the ship's live fire-group assignment (absent → all weapons default to
            // group 1 / Primary, so editing starts from the same "fires on Space" baseline).
            session.weapon_groups = w.get::<WeaponGroups>(ship).cloned().unwrap_or_default();
        }
    }
    inventory.available = session.modules.modules.keys().copied().collect();
    session.selected_slot = None;
    session.status.clear();
}

/// `EguiPrimaryContextPass` (while Fitting) — the EVE-style modal. Draws the ship silhouette with
/// clickable slot markers (left), the selected slot's compatible-module list (right), live budget
/// bars + preview deltas (top), and Apply/Close (bottom). Edits mutate `working_fit`; **Apply**
/// commits it to the live ship (`Changed<Fit>` → `recompute_ship_stats_system` re-derives next tick).
fn fitting_screen_ui(
    mut contexts: EguiContexts,
    mut session: ResMut<FittingSession>,
    inventory: Res<Inventory>,
    host: Option<NonSendMut<LoopbackHost>>,
    state: Option<NonSend<NetClientState>>,
    mut next_state: ResMut<NextState<FittingScreenState>>,
) {
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let Some(hull) = session.working_hull().cloned() else {
        return;
    };
    // Clone the data the closure needs so it never borrows `session` mutably (we apply edits AFTER).
    let working = session.working_fit.clone();
    let modules = session.modules.clone();
    let selected = session.selected_slot;
    let groups = session.weapon_groups.clone();
    let (budget, stats) = preview_stats(&hull, &working, &modules);
    let baseline = preview_stats(&hull, &session.applied_fit, &modules).1;

    // Actions collected in the closure, applied to `session` after it returns.
    let mut select: Option<SlotId> = None;
    let mut to_install: Option<(SlotId, ModuleId)> = None;
    let mut to_remove: Option<SlotId> = None;
    // R45 — a fire-group edit (slot → new group/trigger), applied to `session.weapon_groups` after.
    let mut to_assign: Option<(SlotId, FireMapping)> = None;
    let mut do_apply = false;
    let mut do_close = false;

    egui::Window::new("⚙ Ship Fitting")
        .collapsible(false)
        .resizable(true)
        .default_size([720.0, 540.0])
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            budget_bar(ui, "POWER", budget.power);
            budget_bar(ui, "CPU", budget.cpu);
            budget_bar(ui, "MASS", budget.mass);
            ui.label(format!(
                "TOP {:.0} ({:+.0})    TURN {:.2} ({:+.2})    MASS {:.0} ({:+.0})    {}",
                stats.top_speed(),
                stats.top_speed() - baseline.top_speed(),
                stats.max_turn_rate(),
                stats.max_turn_rate() - baseline.max_turn_rate(),
                stats.total_mass,
                stats.total_mass - baseline.total_mass,
                if stats.can_fire { "ARMED" } else { "NO WEAPON" },
            ));
            ui.separator();
            ui.columns(2, |cols| {
                cols[0].label("SHIP — click a slot");
                draw_ship(
                    &mut cols[0],
                    &hull,
                    &working,
                    &modules,
                    &groups,
                    selected,
                    &mut select,
                );
                slot_panel(
                    &mut cols[1],
                    &hull,
                    &working,
                    &modules,
                    &inventory,
                    &groups,
                    selected,
                    &mut to_install,
                    &mut to_remove,
                    &mut to_assign,
                );
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("✔ Apply to ship").clicked() {
                    do_apply = true;
                }
                if ui.button("✖ Close (Tab)").clicked() {
                    do_close = true;
                }
            });
            if !session.status.is_empty() {
                ui.label(&session.status);
            }
        });

    // --- apply collected actions (the closure's borrows have been released) ---
    if let Some(s) = select {
        session.selected_slot = Some(s);
    }
    if let Some(slot) = to_remove {
        match session.working_fit.remove_raw(slot) {
            Some(rem) => session.status = format!("Removed {:?} from slot {}", rem.module, slot.0),
            None => session.status = format!("Slot {} was empty", slot.0),
        }
    }
    if let Some((slot, module)) = to_install {
        let cat = session.modules.clone();
        match session
            .working_fit
            .install_module(slot, module, &hull, &cat)
        {
            Ok(()) => session.status = format!("Installed into slot {}", slot.0),
            Err(rej) => session.status = describe_rejection(&rej),
        }
    }
    if let Some((slot, mapping)) = to_assign {
        // R45 — group 1 / Primary is the implicit default, so drop a default-mapping back to "absent"
        // to keep the committed `WeaponGroups` minimal (and a no-config ship truly empty).
        if mapping == FireMapping::default() {
            session.weapon_groups.mapping.remove(&slot);
        } else {
            session.weapon_groups.mapping.insert(slot, mapping);
        }
        session.status = format!(
            "Slot {} → group {} / {:?}",
            slot.0,
            mapping.group + 1,
            mapping.trigger
        );
    }
    if do_apply {
        // R45 (Bug #1): validate BEFORE committing — refuse an over-budget / invalid fit (you can
        // over-REMOVE, e.g. drop a reactor → power.over, which install never lets you do) instead of
        // a false "Applied ✓".
        let v = validate_fit(&hull, &session.working_fit, &session.modules);
        if v.valid {
            session.status =
                apply_to_ship(host, state, &session.working_fit, &session.weapon_groups);
            session.applied_fit = session.working_fit.clone();
        } else {
            let over = if v.usage.power.over {
                "power"
            } else if v.usage.cpu.over {
                "CPU"
            } else if v.usage.mass.over {
                "mass"
            } else {
                ""
            };
            session.status = if over.is_empty() {
                "Can't apply — invalid fit".to_string()
            } else {
                format!("Can't apply — over budget: {over}")
            };
        }
    }
    if do_close {
        next_state.set(FittingScreenState::Flying);
    }
}

/// R44/R45 — commit the working fit to the live player ship (the R43 path): resolve the ship via the
/// wire id, write the `Fit` (→ `recompute_ship_stats_system` re-derives next tick) **and** the
/// `WeaponGroups` (read by `weapon_fire_system` to gate each weapon by group/trigger). Returns a status.
fn apply_to_ship(
    host: Option<NonSendMut<LoopbackHost>>,
    state: Option<NonSend<NetClientState>>,
    fit: &Fit,
    groups: &WeaponGroups,
) -> String {
    let (Some(mut host), Some(state)) = (host, state) else {
        return "No embedded server to apply to".to_string();
    };
    let Some(ship) = host.server.ship_entity_for(state.local_id) else {
        return "No player ship to apply to".to_string();
    };
    let fit = fit.clone();
    let groups = groups.clone();
    let world = host.server.world_mut();
    match world.get_entity_mut(ship) {
        Ok(mut e) => {
            e.insert((fit, groups));
            "Applied to ship ✓".to_string()
        }
        Err(_) => "Player ship entity missing".to_string(),
    }
}

/// Pixel size of one grid cell in the silhouette.
const CELL: f32 = 16.0;

/// Draw the ship as an egui grid: each hull cell is a dim filler, each SLOT is a clickable coloured
/// button at its hull position (label = installed module's short name, or the slot-type initial). The
/// selected slot is highlighted. Rows are drawn top-down so the nose (high row) points up.
fn draw_ship(
    ui: &mut egui::Ui,
    hull: &Hull,
    fit: &Fit,
    modules: &ModuleCatalog,
    groups: &WeaponGroups,
    selected: Option<SlotId>,
    select: &mut Option<SlotId>,
) {
    let (cols, rows) = hull.grid_dims;
    egui::Grid::new("ship_silhouette")
        .spacing(egui::vec2(2.0, 2.0))
        .show(ui, |ui| {
            for row in (0..rows).rev() {
                for col in 0..cols {
                    let coord = (col, row);
                    if let Some(slot) = hull.slots.iter().find(|s| s.coord == coord) {
                        let installed = fit.assignments.get(&slot.id).and_then(|m| modules.get(*m));
                        // R45 — a weapon slot holding a weapon shows its FIRE GROUP digit (1-6) so the
                        // silhouette reads as a fire-group map at a glance; its trigger tints it (Off =
                        // dim grey, Secondary = blue). Other slots keep the module's short name.
                        let map = groups.for_slot(slot.id);
                        let is_weapon =
                            slot.slot_type == HardpointType::Weapon && installed.is_some();
                        let label = if is_weapon {
                            (map.group + 1).to_string()
                        } else {
                            installed
                                .map(|m| short(&m.name))
                                .unwrap_or_else(|| type_initial(slot.slot_type).to_string())
                        };
                        let fill = if selected == Some(slot.id) {
                            egui::Color32::from_rgb(240, 220, 80)
                        } else if is_weapon {
                            match map.trigger {
                                Trigger::Primary => egui::Color32::from_rgb(210, 120, 70),
                                Trigger::Secondary => egui::Color32::from_rgb(80, 140, 210),
                                Trigger::Off => egui::Color32::from_rgb(90, 90, 95),
                            }
                        } else {
                            slot_color(slot.slot_type)
                        };
                        let resp = ui
                            .add_sized(
                                [CELL, CELL],
                                egui::Button::new(
                                    egui::RichText::new(label)
                                        .size(if is_weapon { 10.0 } else { 8.0 })
                                        .color(egui::Color32::BLACK),
                                )
                                .fill(fill),
                            )
                            .on_hover_text(format!(
                                "{:?} slot {} ({:?}) — {}{}",
                                slot.slot_type,
                                slot.id.0,
                                slot.size,
                                installed.map(|m| m.name.as_str()).unwrap_or("empty"),
                                if is_weapon {
                                    format!("  [group {} / {:?}]", map.group + 1, map.trigger)
                                } else {
                                    String::new()
                                },
                            ));
                        if resp.clicked() {
                            *select = Some(slot.id);
                        }
                    } else if hull.cells.iter().any(|c| c.coord == coord) {
                        ui.add_sized(
                            [CELL, CELL],
                            egui::Button::new("").fill(egui::Color32::from_rgb(40, 50, 62)),
                        );
                    } else {
                        ui.add_sized([CELL, CELL], egui::Label::new(""));
                    }
                }
                ui.end_row();
            }
        });
}

/// The right-hand panel: the selected slot's type/size + installed module (with Remove), then a
/// scrollable list of inventory modules COMPATIBLE with that slot (`check_slot_fit` ⇒ type+size ok),
/// each with a hover tooltip of full stats. Clicking a module installs it (budget checked on install).
#[allow(clippy::too_many_arguments)]
fn slot_panel(
    ui: &mut egui::Ui,
    hull: &Hull,
    fit: &Fit,
    modules: &ModuleCatalog,
    inventory: &Inventory,
    groups: &WeaponGroups,
    selected: Option<SlotId>,
    to_install: &mut Option<(SlotId, ModuleId)>,
    to_remove: &mut Option<SlotId>,
    to_assign: &mut Option<(SlotId, FireMapping)>,
) {
    let Some(slot_id) = selected else {
        ui.label("Click a slot on the ship to fit a module.");
        return;
    };
    let Some(slot) = hull.slots.iter().find(|s| s.id == slot_id) else {
        return;
    };
    ui.heading(format!(
        "Slot {} — {:?} ({:?})",
        slot.id.0, slot.slot_type, slot.size
    ));
    let installed = fit.assignments.get(&slot_id).and_then(|m| modules.get(*m));
    if let Some(m) = installed {
        ui.horizontal(|ui| {
            ui.label(format!("Installed: {}", m.name));
            if ui.button("Remove").clicked() {
                *to_remove = Some(slot_id);
            }
        });
    } else {
        ui.label("Empty");
    }
    // R45 — a weapon slot with a weapon installed gets a FIRE-GROUP editor: a group (1-6) it belongs
    // to + the trigger that fires it. In combat, number keys 1-6 select the active group; Space fires
    // its Primary weapons, Ctrl its Secondary. Off = assigned but never fires.
    if slot.slot_type == HardpointType::Weapon && installed.is_some() {
        ui.separator();
        ui.label("Fire group");
        let map = groups.for_slot(slot_id);
        ui.horizontal(|ui| {
            for g in 0u8..6 {
                if ui
                    .selectable_label(map.group == g, format!("{}", g + 1))
                    .clicked()
                {
                    *to_assign = Some((
                        slot_id,
                        FireMapping {
                            group: g,
                            trigger: map.trigger,
                        },
                    ));
                }
            }
        });
        ui.horizontal(|ui| {
            for (label, trig) in [
                ("Primary", Trigger::Primary),
                ("Secondary", Trigger::Secondary),
                ("Off", Trigger::Off),
            ] {
                if ui.selectable_label(map.trigger == trig, label).clicked() {
                    *to_assign = Some((
                        slot_id,
                        FireMapping {
                            group: map.group,
                            trigger: trig,
                        },
                    ));
                }
            }
        });
    }
    ui.separator();
    ui.label("Compatible modules (inventory):");
    egui::ScrollArea::vertical()
        .max_height(300.0)
        .show(ui, |ui| {
            for id in &inventory.available {
                let Some(m) = modules.get(*id) else {
                    continue;
                };
                // Type + size must fit this slot (budget is checked on install).
                if check_slot_fit(slot, m).is_some() {
                    continue;
                }
                let resp = ui
                    .button(format!(
                        "{}    p{:.0} c{:.0} m{:.0}",
                        m.name, m.power_draw, m.cpu_draw, m.mass
                    ))
                    .on_hover_ui(|ui| module_tooltip(ui, m));
                if resp.clicked() {
                    *to_install = Some((slot_id, *id));
                }
            }
        });
}

/// A hover tooltip with a module's full stats — incl. the R42 derived weapon readout for weapons.
fn module_tooltip(ui: &mut egui::Ui, m: &Module) {
    ui.label(format!(
        "{}  [{:?} / {:?}]",
        m.name, m.kind, m.hardpoint_size
    ));
    ui.label(format!(
        "power {:.0}   cpu {:.0}   mass {:.0}   hp {:.0}",
        m.power_draw, m.cpu_draw, m.mass, m.health_max
    ));
    if let Some(d) = derive_weapon(&m.specifics, &sim::SimTuning::default()) {
        ui.label(format!(
            "muzzle {:.0}   rof {:.1}/s   dmg {:.1}   radius {:.3}",
            d.muzzle_speed, d.fire_rate, d.damage, d.projectile_radius
        ));
        ui.label(format!(
            "spin-up {:.1}s   dispersion {:.1}°   range {:.0}",
            d.spin_up_time,
            d.dispersion_rad.to_degrees(),
            d.lifetime * d.muzzle_speed
        ));
    }
}

/// One budget bar (power/cpu/mass): a coloured [`egui::ProgressBar`] (red when over capacity).
fn budget_bar(ui: &mut egui::Ui, label: &str, axis: sim::fitting::AxisUsage) {
    let frac = if axis.capacity > 0.0 {
        (axis.used / axis.capacity).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let color = if axis.over {
        egui::Color32::from_rgb(220, 60, 50)
    } else {
        egui::Color32::from_rgb(80, 160, 220)
    };
    ui.add(
        egui::ProgressBar::new(frac)
            .fill(color)
            .text(format!("{label} {:.0}/{:.0}", axis.used, axis.capacity)),
    );
}

/// The marker colour for a slot by its hardpoint type.
fn slot_color(t: HardpointType) -> egui::Color32 {
    match t {
        HardpointType::Weapon => egui::Color32::from_rgb(210, 90, 75),
        HardpointType::Reactor => egui::Color32::from_rgb(225, 185, 70),
        HardpointType::Thruster => egui::Color32::from_rgb(90, 170, 225),
        HardpointType::Shield => egui::Color32::from_rgb(95, 130, 230),
        HardpointType::Armor => egui::Color32::from_rgb(165, 165, 175),
        HardpointType::Sensor => egui::Color32::from_rgb(130, 205, 130),
        HardpointType::Utility => egui::Color32::from_rgb(155, 130, 195),
    }
}

/// The single-letter tag for an empty slot's hardpoint type.
fn type_initial(t: HardpointType) -> &'static str {
    match t {
        HardpointType::Weapon => "W",
        HardpointType::Reactor => "R",
        HardpointType::Thruster => "T",
        HardpointType::Shield => "S",
        HardpointType::Armor => "A",
        HardpointType::Sensor => "E",
        HardpointType::Utility => "U",
    }
}

/// A short label (≤ 3 chars) for a slot marker.
fn short(name: &str) -> String {
    name.chars().take(3).collect()
}

/// Turn a [`FitRejection`] into the on-screen reason an install was refused (FR-005/006/007).
fn describe_rejection(rejection: &FitRejection) -> String {
    match rejection {
        FitRejection::SlotTypeMismatch { slot, module } => {
            format!("Type mismatch: {module:?} does not fit slot {}", slot.0)
        }
        FitRejection::SlotSizeMismatch { slot, module } => {
            format!("Size mismatch: {module:?} too large for slot {}", slot.0)
        }
        FitRejection::WouldExceedBudget { axis } => format!("Over budget: {axis:?}"),
        FitRejection::UnknownSlot { slot } => format!("Unknown slot {}", slot.0),
        FitRejection::UnknownModule { module } => format!("Unknown module {module:?}"),
    }
}
