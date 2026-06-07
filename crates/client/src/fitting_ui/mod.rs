//! The interactive ship-fitting screen (E006 US5, FR-012/009/013/024).
//!
//! A thin Bevy `bevy_ui` screen over the shared `sim` fitting domain (Principle
//! II): **all** gameplay logic — install legality, budget math, stat derivation,
//! preset save/reload — lives in `sim`; this screen only *calls* those pure
//! functions and renders their results. It carries no authoritative fit truth and
//! never re-derives stats with a forked formula (data-model.md `FittingPreview`).
//!
//! What it offers (the US5 interactions, all wired to `sim` fns):
//! - **place / remove** a module into a positional slot — driven by
//!   [`sim::fitting::Fit::install_module`] (on a [`sim::fitting::FitRejection`] the
//!   reason is surfaced on screen) and [`sim::fitting::Fit::remove_raw`];
//! - live **power / CPU / mass budget bars** from [`sim::fitting::budget_usage`],
//!   recomputed on every change (FR-009);
//! - a **before-commit preview** ([`FittingPreview`], FR-013): the candidate fit's
//!   derived [`sim::fitting::ShipStats`] + budget via [`sim::fitting::preview_stats`],
//!   with per-axis deltas vs. the *applied* baseline coloured green (improved) /
//!   red (worse);
//! - **preset** save / name / reload controls (FR-024) via
//!   [`sim::fitting::save_preset`] / [`sim::fitting::load_preset`].
//!
//! State model: a [`FittingScreenState`] app-state toggles between the flying view
//! and the fitting screen (the toggle key is wired in `main.rs`, T034). The UI is
//! built on [`OnEnter`] the `Fitting` state and torn down on [`OnExit`]; the
//! interaction + readout systems run only `in_state(FittingScreenState::Fitting)`.
//!
//! **Verification status:** this screen is *compile- and structure-verified* +
//! requires **manual playtest** — it cannot be headless-tested (no window). The
//! pure `sim` fns it drives ARE headless-tested (T029); this module is the thin
//! input/render wiring over them.

use bevy::prelude::*;
use sim::fitting::{
    load_preset, preview_stats, save_preset, seed_catalogs, BudgetUsage, Fit, FitPreset,
    FitRejection, HullCatalog, ModuleCatalog, ModuleId, ShipStats, SlotId, HULL_FIGHTER,
    MODULE_ARMOR_PLATE, MODULE_AUTOCANNON, MODULE_REACTOR_BASIC, MODULE_SHIELD_BASIC,
    MODULE_THRUSTER_BASIC, MODULE_UTILITY_BASIC,
};

use crate::net::{LoopbackHost, NetClientState};

/// The client app-state toggling the flying view and the interactive fitting
/// screen (FR-012). `main.rs` registers it ([`bevy::app::App::init_state`]) and
/// binds a key that flips `Flying ⇄ Fitting`; the screen's UI + interaction
/// systems are gated `in_state(FittingScreenState::Fitting)`.
#[derive(States, Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FittingScreenState {
    /// The normal flying / combat view (default). The fitting UI is hidden.
    #[default]
    Flying,
    /// The interactive fitting screen is open (place/remove, budgets, preview,
    /// presets). Flight input still runs, but the player is editing the fit.
    Fitting,
}

/// The interactive fitting screen plugin (T030/T031/T032). Registers the
/// [`FittingScreenState`], the working-fit [`FittingSession`] + [`FittingPreview`]
/// resources, builds/tears down the screen on state transitions, and runs the
/// place/remove + budget/preview + preset systems while the screen is open.
///
/// Add it after `DefaultPlugins`; pair it with a state-toggle key in `main.rs`
/// (T034) so the screen is reachable.
pub struct FittingUiPlugin;

impl Plugin for FittingUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<FittingScreenState>()
            .init_resource::<FittingSession>()
            .init_resource::<FittingPreview>()
            // R43: the player's available modules ("cargo/inventory") the screen installs from.
            .init_resource::<Inventory>()
            // Build the screen UI when entering Fitting; tear it down on exit so
            // the flying HUD is unobstructed.
            .add_systems(OnEnter(FittingScreenState::Fitting), build_fitting_screen)
            .add_systems(OnExit(FittingScreenState::Fitting), teardown_fitting_screen)
            // Interaction + readouts run only while the screen is open. Input drives
            // the `sim` install/remove/preset fns; the readout systems re-run the
            // pure `sim` budget/preview fns and paint the bars + deltas. They chain
            // so a place/remove this frame is reflected in the same frame's bars.
            // R43: `commit_fitting_to_ship` (Enter) writes the working fit to the live ship;
            // `refresh_loadout` repaints the inventory + per-slot loadout text.
            .add_systems(
                Update,
                (
                    handle_fitting_input,
                    commit_fitting_to_ship,
                    refresh_preview,
                    refresh_budget_bars,
                    refresh_status_text,
                    refresh_loadout,
                )
                    .chain()
                    .run_if(in_state(FittingScreenState::Fitting)),
            );
    }
}

/// The slot palette the screen edits, in stable order — one entry per fighter slot
/// keyed to a number key (1..=7). Pairing a slot with the module the player most
/// recently chose to install lets the place action call `install_module` with a
/// concrete `(SlotId, ModuleId)`.
const SLOT_KEYS: [(KeyCode, SlotId); 7] = [
    (KeyCode::Digit1, SlotId(0)),
    (KeyCode::Digit2, SlotId(1)),
    (KeyCode::Digit3, SlotId(2)),
    (KeyCode::Digit4, SlotId(3)),
    (KeyCode::Digit5, SlotId(4)),
    (KeyCode::Digit6, SlotId(5)),
    (KeyCode::Digit7, SlotId(6)),
];

/// The module palette the screen can install, in stable order — one entry per
/// archetype keyed to a letter key. The player selects the *active module* with
/// these keys, then presses a slot number to install it there.
const MODULE_KEYS: [(KeyCode, ModuleId, &str); 6] = [
    (KeyCode::KeyR, MODULE_REACTOR_BASIC, "Reactor"),
    (KeyCode::KeyT, MODULE_THRUSTER_BASIC, "Thruster"),
    (KeyCode::KeyG, MODULE_AUTOCANNON, "Autocannon"),
    (KeyCode::KeyH, MODULE_SHIELD_BASIC, "Shield"),
    (KeyCode::KeyJ, MODULE_ARMOR_PLATE, "Armor"),
    (KeyCode::KeyK, MODULE_UTILITY_BASIC, "Utility"),
];

/// The screen's working state — the in-progress [`Fit`] the player edits, the
/// catalogs the `sim` fns resolve ids against, the currently-selected module to
/// install, the in-memory saved presets, and the last action's outcome message.
///
/// Client-only (no authoritative truth): editing this never touches a running
/// ship until the player chooses to apply/commit. The catalogs are the SAME seed
/// content the server loads ([`seed_catalogs`]), so the screen's preview matches
/// what a commit would derive (Principle II).
#[derive(Resource)]
pub struct FittingSession {
    /// The catalogs the `sim` fns resolve `ModuleId`/`SlotId` against.
    pub modules: ModuleCatalog,
    pub hulls: HullCatalog,
    /// The working fit being edited (a fighter by default).
    pub working_fit: Fit,
    /// The "applied" fit the preview deltas are measured against — the fit as it
    /// was when the screen was last opened / committed. The before-commit preview
    /// (FR-013) diffs the working fit's derived stats against this baseline.
    pub applied_fit: Fit,
    /// The module the next place action installs (selected via [`MODULE_KEYS`]).
    pub active_module: ModuleId,
    /// In-memory saved presets (FR-024; durable save is E004). Indexed for reload.
    pub presets: Vec<FitPreset>,
    /// Human-readable outcome of the last action (an install rejection reason, a
    /// preset save/reload result) — surfaced on screen (FR-005/006/007/024).
    pub status: String,
}

impl Default for FittingSession {
    fn default() -> Self {
        let (modules, hulls) = seed_catalogs();
        // Start on the fighter with an empty baseline fit (INV-F05) — the player
        // builds it up. `applied_fit` starts equal so the first preview shows a
        // zero delta until the player edits.
        let working_fit = Fit::new(HULL_FIGHTER);
        let applied_fit = working_fit.clone();
        Self {
            modules,
            hulls,
            working_fit,
            applied_fit,
            active_module: MODULE_REACTOR_BASIC,
            presets: Vec::new(),
            status: "Select a module (R/T/G/H/J/K), press a slot (1-7) to install".to_string(),
        }
    }
}

impl FittingSession {
    /// The hull the working fit is built on (resolved in the catalog). Panics only
    /// if the catalog is missing the working hull, which `Default` guarantees it is
    /// not — kept as an `Option` accessor for null-safety at the call sites.
    fn working_hull(&self) -> Option<&sim::fitting::Hull> {
        self.hulls.get(self.working_fit.hull)
    }
}

/// The before-commit preview (FR-013; data-model.md `FittingPreview`) — the
/// candidate working fit's derived [`ShipStats`] + [`BudgetUsage`] and the per-axis
/// **deltas vs. the applied baseline**, recomputed every change. Client-only
/// sandbox: never applied to a live ship until the player commits.
///
/// Deltas are signed so the UI paints an improvement green and a regression red
/// (e.g. more top speed = green, more mass = red). Computed off the SAME
/// [`preview_stats`] the running sim and a future server derive on (Principle II),
/// so the preview is exactly what a commit would produce (SC-006).
#[derive(Resource, Default)]
pub struct FittingPreview {
    /// The candidate fit's full derived flight/weapon stats (the commit result).
    pub candidate_stats: Option<ShipStats>,
    /// The candidate fit's per-axis budget usage (drives the live bars, FR-009).
    pub candidate_budget: Option<BudgetUsage>,
    /// Δ top speed vs. the applied baseline (positive = faster ⇒ green).
    pub delta_top_speed: f32,
    /// Δ max turn rate vs. baseline (positive = more agile ⇒ green).
    pub delta_turn_rate: f32,
    /// Δ total mass vs. baseline (positive = heavier ⇒ red).
    pub delta_mass: f32,
    /// Whether the candidate fit can fire (≥1 weapon module, FR-016).
    pub can_fire: bool,
}

/// Refinement 43 — the player's available modules ("cargo / inventory") the fitting screen can
/// install. For now it holds EVERY catalog module (refreshed from the live catalog on enter); a real
/// game will limit it to OWNED items. Installing is still gated by ship capabilities (hardpoint
/// type/size + budget) via [`Fit::install_module`], so the inventory is "what you have" and the slots
/// are "what fits".
#[derive(Resource, Default)]
pub struct Inventory {
    /// Owned module ids in display order (sorted by id — `ModuleCatalog` is a `BTreeMap`).
    pub available: Vec<ModuleId>,
    /// Cursor into `available`; the highlighted (`▶`) row is the active module to install.
    pub cursor: usize,
}

// --- UI marker components ----------------------------------------------------

/// Root node of the fitting screen — despawned wholesale on exit.
#[derive(Component)]
struct FittingScreenRoot;

/// R43 — the scrolling INVENTORY list text (all available modules; the active one marked `▶`).
#[derive(Component)]
struct InventoryText;
/// R43 — the per-slot LOADOUT readout text (each slot + its installed module).
#[derive(Component)]
struct LoadoutText;

/// The power-budget bar's fill node (width tracks `used / capacity`).
#[derive(Component)]
struct PowerBarFill;
/// The CPU-budget bar's fill node.
#[derive(Component)]
struct CpuBarFill;
/// The mass-budget bar's fill node.
#[derive(Component)]
struct MassBarFill;
/// The per-axis numeric budget readout line.
#[derive(Component)]
struct BudgetText;
/// The preview / stat-delta readout line (green/red deltas).
#[derive(Component)]
struct PreviewText;
/// The status / last-action-outcome line (install rejection, preset result).
#[derive(Component)]
struct StatusText;

/// Pixel width of a full budget bar; a fill node's width is `frac * BAR_WIDTH`.
const BAR_WIDTH: f32 = 240.0;

/// `OnEnter(Fitting)`: build the screen's `bevy_ui` node tree (mirrors `hud.rs`'s
/// node/text pattern). One root panel anchored top-right holds the three budget
/// bars (each a track + a coloured fill), the budget numeric line, the preview /
/// delta line, the status line, and a static controls legend.
fn build_fitting_screen(
    mut commands: Commands,
    mut session: ResMut<FittingSession>,
    mut inventory: ResMut<Inventory>,
    host: Option<NonSend<LoopbackHost>>,
    // R43: the local player's wire id → load the LIVE ship's current fit so you edit your real loadout.
    state: Option<NonSend<NetClientState>>,
) {
    // R39/R43: refresh the catalog AND load the player's CURRENT loadout from the LIVE embedded server
    // (so previews + the editable fit match what flies), instead of the stale seed clone + empty fit.
    if let Some(host) = host {
        let w = host.server.world();
        if let Some(m) = w.get_resource::<ModuleCatalog>() {
            session.modules = m.clone();
        }
        if let Some(h) = w.get_resource::<HullCatalog>() {
            session.hulls = h.clone();
        }
        // Load the live ship's fit → edit a copy; `applied_fit` is the baseline for preview deltas.
        if let Some(ship) = state
            .as_ref()
            .and_then(|s| host.server.ship_entity_for(s.local_id))
        {
            if let Some(fit) = w.get::<Fit>(ship) {
                session.working_fit = fit.clone();
                session.applied_fit = fit.clone();
            }
        }
    }
    // R43: the inventory ("cargo") is every catalog module (sorted by id); clamp the cursor + set the
    // active module from it.
    inventory.available = session.modules.modules.keys().copied().collect();
    if inventory.cursor >= inventory.available.len() {
        inventory.cursor = 0;
    }
    if let Some(id) = inventory.available.get(inventory.cursor) {
        session.active_module = *id;
    }
    commands
        .spawn((
            FittingScreenRoot,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(10.0),
                right: Val::Px(10.0),
                width: Val::Px(BAR_WIDTH + 40.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                padding: UiRect::all(Val::Px(12.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.02, 0.04, 0.08, 0.85)),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("— SHIP FITTING —"),
                text_font(18.0),
                TextColor(Color::srgb(0.85, 0.95, 1.0)),
            ));

            // Three labelled budget bars: a fixed-width track with a coloured fill
            // node whose width `refresh_budget_bars` updates each change.
            spawn_budget_bar(root, "POWER", Color::srgb(0.3, 0.8, 1.0), PowerBarFill);
            spawn_budget_bar(root, "CPU", Color::srgb(0.9, 0.7, 0.3), CpuBarFill);
            spawn_budget_bar(root, "MASS", Color::srgb(0.8, 0.5, 0.9), MassBarFill);

            root.spawn((
                Text::new("POWER 0/0  CPU 0/0  MASS 0/0"),
                text_font(13.0),
                TextColor(Color::srgb(0.75, 0.85, 0.95)),
                BudgetText,
            ));
            root.spawn((
                Text::new("PREVIEW —"),
                text_font(13.0),
                TextColor(Color::srgb(0.75, 0.85, 0.95)),
                PreviewText,
            ));
            root.spawn((
                Text::new(""),
                text_font(13.0),
                TextColor(Color::srgb(1.0, 0.85, 0.4)),
                StatusText,
            ));
            root.spawn((
                Text::new(
                    "↑/↓ select module   1-7 install into slot   X+1-7 remove\n\
                     ENTER apply to ship   P save preset   L load preset",
                ),
                text_font(11.0),
                TextColor(Color::srgb(0.6, 0.68, 0.78)),
            ));
            // R43: per-slot loadout + the inventory ("cargo") list — filled each frame by
            // `refresh_loadout`.
            root.spawn((
                Text::new(""),
                text_font(11.0),
                TextColor(Color::srgb(0.8, 0.9, 0.8)),
                LoadoutText,
            ));
            root.spawn((
                Text::new(""),
                text_font(11.0),
                TextColor(Color::srgb(0.78, 0.86, 0.95)),
                InventoryText,
            ));
        });
}

/// Spawn one labelled budget bar (label + track holding a tagged fill node). The
/// fill node carries the `Marker` component so its width can be set per axis by
/// [`refresh_budget_bars`].
fn spawn_budget_bar<M: Component>(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    color: Color,
    marker: M,
) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: Val::Px(6.0),
            ..default()
        })
        .with_children(|row| {
            row.spawn((
                Text::new(label),
                text_font(12.0),
                TextColor(Color::srgb(0.7, 0.8, 0.9)),
                Node {
                    width: Val::Px(48.0),
                    ..default()
                },
            ));
            // Track (background) with the fill node inside it.
            row.spawn((
                Node {
                    width: Val::Px(BAR_WIDTH),
                    height: Val::Px(12.0),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.15, 0.18, 0.22, 1.0)),
            ))
            .with_children(|track| {
                track.spawn((
                    Node {
                        width: Val::Px(0.0),
                        height: Val::Px(12.0),
                        ..default()
                    },
                    BackgroundColor(color),
                    marker,
                ));
            });
        });
}

/// A `TextFont` of the given size — the one-liner `hud.rs` uses inline, factored
/// out here because the screen spawns many text nodes.
fn text_font(size: f32) -> TextFont {
    TextFont {
        font_size: size,
        ..default()
    }
}

/// `OnExit(Fitting)`: tear the whole screen down so the flying view/HUD is
/// unobstructed. Despawning the root despawns the bars/text children with it.
fn teardown_fitting_screen(mut commands: Commands, root_q: Query<Entity, With<FittingScreenRoot>>) {
    for entity in &root_q {
        commands.entity(entity).despawn();
    }
}

/// `Update` (while Fitting): map keys to the `sim` fitting fns (T030/T032).
///
/// - a **module key** ([`MODULE_KEYS`]) selects the active module to install;
/// - a **slot key** ([`SLOT_KEYS`]) installs the active module into that slot via
///   [`Fit::install_module`] — on a [`FitRejection`] the reason is stored in
///   `status` (FR-005/006/007); a success updates `status` too;
/// - **X + slot key** removes the module in that slot ([`Fit::remove_raw`]);
/// - **P** saves the working fit as a named preset ([`save_preset`], FR-024);
/// - **L** reloads the most-recent preset onto the working hull ([`load_preset`]),
///   surfacing a compatibility rejection in `status`.
///
/// Every gameplay decision is the `sim` fn's — this only routes input and records
/// the human-readable outcome.
fn handle_fitting_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut session: ResMut<FittingSession>,
    mut inventory: ResMut<Inventory>,
) {
    // R43: inventory selection — letter quick-jumps move the cursor to that module; ↑/↓ step it. The
    // cursor row is the active module to install (the inventory list marks it `▶`).
    for (key, module_id, _name) in MODULE_KEYS {
        if keys.just_pressed(key) {
            if let Some(i) = inventory.available.iter().position(|id| *id == module_id) {
                inventory.cursor = i;
            }
        }
    }
    if !inventory.available.is_empty() {
        let n = inventory.available.len();
        if keys.just_pressed(KeyCode::ArrowDown) {
            inventory.cursor = (inventory.cursor + 1) % n;
        }
        if keys.just_pressed(KeyCode::ArrowUp) {
            inventory.cursor = (inventory.cursor + n - 1) % n;
        }
        inventory.cursor = inventory.cursor.min(n - 1);
        session.active_module = inventory.available[inventory.cursor];
    }

    let removing = keys.pressed(KeyCode::KeyX);

    // Slot actions: install the active module, or (with X held) remove.
    for (key, slot) in SLOT_KEYS {
        if !keys.just_pressed(key) {
            continue;
        }
        if removing {
            match session.working_fit.remove_raw(slot) {
                Some(removed) => {
                    session.status = format!("Removed {:?} from slot {}", removed.module, slot.0)
                }
                None => session.status = format!("Slot {} was empty", slot.0),
            }
            continue;
        }
        // Install: resolve the hull, then call the pure `sim` validate-then-apply.
        let module = session.active_module;
        let Some(hull) = session.working_hull().cloned() else {
            session.status = "Working hull missing from catalog".to_string();
            continue;
        };
        let modules = session.modules.clone();
        match session
            .working_fit
            .install_module(slot, module, &hull, &modules)
        {
            Ok(()) => session.status = format!("Installed {:?} into slot {}", module, slot.0),
            Err(rejection) => session.status = describe_rejection(&rejection),
        }
    }

    // Preset save (P): snapshot the working fit under a generated name.
    if keys.just_pressed(KeyCode::KeyP) {
        let name = format!("Preset {}", session.presets.len() + 1);
        let preset = save_preset(&name, &session.working_fit);
        session.status = format!("Saved \"{}\"", preset.name);
        session.presets.push(preset);
    }

    // Preset load (L): reload the most-recent preset onto the working hull.
    if keys.just_pressed(KeyCode::KeyL) {
        let Some(preset) = session.presets.last().cloned() else {
            session.status = "No presets saved yet".to_string();
            return;
        };
        let Some(hull) = session.working_hull().cloned() else {
            session.status = "Working hull missing from catalog".to_string();
            return;
        };
        let modules = session.modules.clone();
        match load_preset(&preset, &hull, &modules) {
            Ok(fit) => {
                session.working_fit = fit;
                session.status = format!("Loaded \"{}\"", preset.name);
            }
            Err(rejection) => {
                session.status = format!(
                    "Cannot load \"{}\": {}",
                    preset.name,
                    describe_rejection(&rejection)
                )
            }
        }
    }
}

/// Turn a [`FitRejection`] into the on-screen reason the install/reload was refused
/// (FR-005/006/007/024). Each variant names the offending rule so the player sees
/// *why* — never a silent rejection.
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

/// `Update` (while Fitting): recompute the before-commit [`FittingPreview`] from
/// the working fit (FR-013, SC-006). Calls [`preview_stats`] (the SAME composition
/// the running sim derives on) for the candidate budget + flight/weapon stats, then
/// diffs the candidate's emergent metrics against the applied baseline so the UI
/// can paint improvements green and regressions red.
fn refresh_preview(session: Res<FittingSession>, mut preview: ResMut<FittingPreview>) {
    let Some(hull) = session.working_hull() else {
        return;
    };
    let (budget, stats) = preview_stats(hull, &session.working_fit, &session.modules);
    let baseline = preview_stats(hull, &session.applied_fit, &session.modules).1;

    preview.delta_top_speed = stats.top_speed() - baseline.top_speed();
    preview.delta_turn_rate = stats.max_turn_rate() - baseline.max_turn_rate();
    preview.delta_mass = stats.total_mass - baseline.total_mass;
    preview.can_fire = stats.can_fire;
    preview.candidate_stats = Some(stats);
    preview.candidate_budget = Some(budget);
}

/// `Update` (while Fitting): paint the three budget bars from the candidate
/// [`BudgetUsage`] (FR-009). Each fill node's width is `clamp(used/capacity)·BAR_WIDTH`
/// and turns red when the axis is over budget (INV-F03) — the live readout the
/// player edits against.
fn refresh_budget_bars(
    preview: Res<FittingPreview>,
    mut power_q: Query<
        (&mut Node, &mut BackgroundColor),
        (
            With<PowerBarFill>,
            Without<CpuBarFill>,
            Without<MassBarFill>,
        ),
    >,
    mut cpu_q: Query<
        (&mut Node, &mut BackgroundColor),
        (
            With<CpuBarFill>,
            Without<PowerBarFill>,
            Without<MassBarFill>,
        ),
    >,
    mut mass_q: Query<
        (&mut Node, &mut BackgroundColor),
        (
            With<MassBarFill>,
            Without<PowerBarFill>,
            Without<CpuBarFill>,
        ),
    >,
    mut text_q: Query<&mut Text, With<BudgetText>>,
) {
    let Some(budget) = preview.candidate_budget else {
        return;
    };

    set_bar(
        power_q.iter_mut().next(),
        budget.power,
        Color::srgb(0.3, 0.8, 1.0),
    );
    set_bar(
        cpu_q.iter_mut().next(),
        budget.cpu,
        Color::srgb(0.9, 0.7, 0.3),
    );
    set_bar(
        mass_q.iter_mut().next(),
        budget.mass,
        Color::srgb(0.8, 0.5, 0.9),
    );

    if let Ok(mut text) = text_q.single_mut() {
        text.0 = format!(
            "POWER {:.0}/{:.0}  CPU {:.0}/{:.0}  MASS {:.0}/{:.0}",
            budget.power.used,
            budget.power.capacity,
            budget.cpu.used,
            budget.cpu.capacity,
            budget.mass.used,
            budget.mass.capacity,
        );
    }
}

/// Set one budget bar's fill width + colour from an [`sim::fitting::AxisUsage`].
/// Width is the clamped `used/capacity` fraction of [`BAR_WIDTH`]; an over-budget
/// axis (INV-F03) paints red regardless of its base colour.
fn set_bar(
    fill: Option<(Mut<Node>, Mut<BackgroundColor>)>,
    axis: sim::fitting::AxisUsage,
    base: Color,
) {
    let Some((mut node, mut bg)) = fill else {
        return;
    };
    let frac = if axis.capacity > 0.0 {
        (axis.used / axis.capacity).clamp(0.0, 1.0)
    } else {
        0.0
    };
    node.width = Val::Px(frac * BAR_WIDTH);
    bg.0 = if axis.over {
        Color::srgb(1.0, 0.25, 0.2)
    } else {
        base
    };
}

/// `Update` (while Fitting): render the preview/delta line (FR-013) and recolour it
/// green (net improvement) or red (regression). Kept separate from the bars so the
/// flight-stat preview and the budget bars update independently.
fn refresh_status_text(
    preview: Res<FittingPreview>,
    session: Res<FittingSession>,
    mut preview_q: Query<&mut Text, (With<PreviewText>, Without<StatusText>)>,
    mut status_q: Query<(&mut Text, &mut TextColor), With<StatusText>>,
) {
    if let (Ok(mut text), Some(stats)) = (preview_q.single_mut(), preview.candidate_stats) {
        text.0 = format!(
            "TOPSPD {:.0} ({}{:.0})  TURN {:.2} ({}{:.2})  MASS {:.0} ({}{:.0})  {}",
            stats.top_speed(),
            sign(preview.delta_top_speed),
            preview.delta_top_speed.abs(),
            stats.max_turn_rate(),
            sign(preview.delta_turn_rate),
            preview.delta_turn_rate.abs(),
            stats.total_mass,
            sign(preview.delta_mass),
            preview.delta_mass.abs(),
            if preview.can_fire {
                "ARMED"
            } else {
                "NO WEAPON"
            },
        );
    }

    if let Ok((mut text, mut color)) = status_q.single_mut() {
        text.0 = session.status.clone();
        // Green when the last preview shows a speed/agility gain without going
        // over budget; otherwise amber (neutral) — the over-budget red is on the
        // bars themselves.
        let improved = preview.delta_top_speed > 0.0 || preview.delta_turn_rate > 0.0;
        color.0 = if improved {
            Color::srgb(0.4, 1.0, 0.5)
        } else {
            Color::srgb(1.0, 0.85, 0.4)
        };
    }
}

/// `"+"` / `"-"` prefix for a signed delta readout (zero reads as `"+"`).
fn sign(delta: f32) -> &'static str {
    if delta < 0.0 {
        "-"
    } else {
        "+"
    }
}

/// `Update` (while Fitting) — R43: on **Enter**, COMMIT the working fit to the live player ship so the
/// edited loadout actually flies. Writes the `Fit` onto the embedded-server ship entity (which
/// triggers [`sim::fitting::recompute_ship_stats_system`] → fresh layout + `ShipStats` next tick) and
/// rebases the preview baseline. Windowed-only — the player ship exists only on this embedded path, so
/// the headless/determinism worlds are untouched.
fn commit_fitting_to_ship(
    keys: Res<ButtonInput<KeyCode>>,
    host: Option<NonSendMut<LoopbackHost>>,
    state: Option<NonSend<NetClientState>>,
    mut session: ResMut<FittingSession>,
) {
    if !keys.just_pressed(KeyCode::Enter) {
        return;
    }
    let (Some(mut host), Some(state)) = (host, state) else {
        session.status = "No embedded server to apply to".to_string();
        return;
    };
    let Some(ship) = host.server.ship_entity_for(state.local_id) else {
        session.status = "No player ship to apply to".to_string();
        return;
    };
    let fit = session.working_fit.clone();
    let world = host.server.world_mut();
    if let Ok(mut entity) = world.get_entity_mut(ship) {
        entity.insert(fit);
        session.applied_fit = session.working_fit.clone();
        session.status = "Applied to ship ✓".to_string();
    } else {
        session.status = "Player ship entity missing".to_string();
    }
}

/// `Update` (while Fitting) — R43: repaint the per-slot LOADOUT line + the INVENTORY ("cargo") list
/// from the working fit + inventory. Each slot shows its hardpoint type + the installed module (or
/// `—`); the active inventory row is marked `▶`, with each module's kind / size / power-cpu-mass cost.
fn refresh_loadout(
    session: Res<FittingSession>,
    inventory: Res<Inventory>,
    mut load_q: Query<&mut Text, (With<LoadoutText>, Without<InventoryText>)>,
    mut inv_q: Query<&mut Text, (With<InventoryText>, Without<LoadoutText>)>,
) {
    if let Ok(mut text) = load_q.single_mut() {
        let mut s = String::from("— LOADOUT —\n");
        if let Some(hull) = session.working_hull() {
            for slot in &hull.slots {
                let name = session
                    .working_fit
                    .assignments
                    .get(&slot.id)
                    .and_then(|mid| session.modules.get(*mid))
                    .map(|m| m.name.as_str())
                    .unwrap_or("—");
                s.push_str(&format!("S{} {:?}: {}\n", slot.id.0, slot.slot_type, name));
            }
        }
        text.0 = s;
    }
    if let Ok(mut text) = inv_q.single_mut() {
        let mut s = String::from("— INVENTORY —\n");
        for (i, id) in inventory.available.iter().enumerate() {
            let Some(m) = session.modules.get(*id) else {
                continue;
            };
            let cursor = if i == inventory.cursor { "▶ " } else { "  " };
            s.push_str(&format!(
                "{cursor}{}  [{:?}/{:?}]  p{:.0} c{:.0} m{:.0}\n",
                m.name, m.kind, m.hardpoint_size, m.power_draw, m.cpu_draw, m.mass
            ));
        }
        text.0 = s;
    }
}
