//! Refinement 27 — persist the client render tuning (Starfield + HUD) to a hand-editable RON file.
//!
//! The dev-panel-tunable [`StarfieldTuning`] + [`HudLayout`] otherwise live only as code `Default`s,
//! edited in-memory and lost on restart. This loads them from `render_tuning.ron` at startup (so the
//! tuning survives restarts + can be hand-edited) and saves them back from the dev panel's Save
//! button. Mirrors the sim content-RON loaders (`$DARK_SILENCE_CONTENT` / `assets/content`, with the
//! code `Default`s as the fallback). Client render config only — determinism-neutral.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use sim::damage::{
    default_resistance_matrix, PenetrationConfig, ResistanceMatrix, SalvageConfig, ShieldConfig,
    StatScalingConfig,
};
use sim::fitting::{CellMaterials, HullCatalog, ModuleCatalog};
use sim::{MiningTuning, SimTuning, Tuning};

use crate::hud_bars::HudLayout;
use crate::starfield::StarfieldTuning;

/// File name (under the content dir) holding the windowed dev override (R39: sim tuning + HUD +
/// starfield). Kept as `render_tuning.ron` so older files (which held only `starfield`+`hud`) still
/// load — the name is now a slight misnomer.
const RENDER_TUNING_RON: &str = "render_tuning.ron";

/// R44 — file name (under the content dir) holding ONLY the client HUD layout. The HUD now has its own
/// file + dedicated dev-panel Save button (it used to ride in `render_tuning.ron`); a one-time
/// migration in [`load_hud_layout`] still reads the old `render_tuning.ron` `hud` field if absent.
const HUD_LAYOUT_RON: &str = "hud_layout.ron";

/// The content dir (`$DARK_SILENCE_CONTENT` if set, else `assets/content/` relative to the CWD —
/// mirrors `server::load_content_or_default`). Shared by the dev override, the module/hull content
/// RONs, the starfield-preset library, and the hull editor's `editor_layout.ron` (R84).
pub(crate) fn content_dir() -> PathBuf {
    std::env::var_os("DARK_SILENCE_CONTENT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("assets/content"))
}

/// R65 — the per-ship hull-design directory (`assets/content/ships/`). Each `*.ron` is ONE serialized
/// [`Hull`](sim::fitting::Hull), so the editor saves a single ship without rewriting the whole catalog.
fn ships_dir() -> PathBuf {
    content_dir().join("ships")
}

/// R65 — load every per-ship hull file from `ships/` (one `Hull` each). Unparseable files are skipped
/// with a logged warning; an absent dir yields an empty vec. Sorted for a deterministic merge order.
pub fn load_ship_files() -> Vec<sim::fitting::Hull> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(ships_dir()) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ron"))
        .collect();
    paths.sort();
    for p in paths {
        match std::fs::read_to_string(&p) {
            Ok(s) => match ron::from_str::<sim::fitting::Hull>(&s) {
                Ok(h) => out.push(h),
                Err(e) => eprintln!("[content] skipping {}: {e}", p.display()),
            },
            Err(e) => eprintln!("[content] cannot read {}: {e}", p.display()),
        }
    }
    out
}

/// R65 — save ONE hull to its own `ships/<id>_<name>.ron` (creating the dir). Any existing `<id>_*.ron`
/// (e.g. from a previous name) is removed first so a rename never orphans a file. Returns a short status.
pub fn save_ship(hull: &sim::fitting::Hull) -> Result<String, String> {
    let dir = ships_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let prefix = format!("{}_", hull.id.0);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for p in entries.flatten().map(|e| e.path()) {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if name.starts_with(&prefix) && name.ends_with(".ron") {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }
    let slug: String = hull
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = dir.join(format!("{}_{}.ron", hull.id.0, slug));
    let body = ron::ser::to_string_pretty(hull, ron::ser::PrettyConfig::default())
        .map_err(|e| format!("serialize hull: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(format!(
        "wrote ships/{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
    ))
}

/// Refinement 39/41/44 — the windowed-client dev override: ALL the dev-panel-tuned **sim tuning**
/// (loaded into the embedded server world WINDOWED-ONLY — never `ServerApp::new`, so headless
/// determinism is untouched) + the client starfield.
///
/// Module/hull **DESIGN** edits are NOT stored here — R41 writes them back to the canonical
/// `modules.ron`/`ships.ron` via [`save_catalogs`]. The client **HUD layout** is NOT stored here
/// either — R44 moved it to its own `hud_layout.ron` ([`save_hud_layout`]/[`load_hud_layout`]).
/// `#[serde(default)]` (container) so an old `render_tuning.ron` still loads (any stale `hud`/`modules`
/// keys a previous build wrote are silently ignored — serde skips unknown fields).
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct DevSettings {
    pub tuning: Tuning,
    pub sim_tuning: SimTuning,
    pub penetration: PenetrationConfig,
    pub shield: ShieldConfig,
    pub salvage: SalvageConfig,
    pub stat_scaling: StatScalingConfig,
    pub resistance: ResistanceMatrix,
    pub mining: MiningTuning,
    pub starfield: StarfieldTuning,
    /// R49 — live-tunable ship visuals (glow / flame / nav / accent / fill / bloom / hull shader).
    #[serde(default)]
    pub ship_visual: crate::ship_visuals::ShipVisualTuning,
    /// R66 — the typed per-cell hull/armor materials catalog (light/heavy hull · light/medium/heavy
    /// armor). Applied windowed-only; material 0/0 = byte-identical to the globals.
    #[serde(default)]
    pub cell_materials: CellMaterials,
    /// T038 (TR-020b, 00008-ship-ai) — the live-editable AI tuning (think cadences / AOI radii /
    /// squads / utility / ram / archetype cuts / sensors / steering / debug history), persisted via
    /// the same dev-settings RON as `SimTuning`/`MiningTuning`. Applied windowed-only — golden/bench
    /// runs keep the pinned `AiTuning::default()` (a saved edit invalidates comparability, TR-020).
    #[serde(default)]
    pub ai: sim::ai::AiTuning,
    /// R99 Phase B/C — the player's PREFERRED faction, stored by the `tint_tag` convention
    /// (`1` = Red, `2` = Blue; `None` = no preference) so we don't depend on a serde derive for
    /// the sim `Faction` type. Set by the dev panel's Team buttons; HONOURED at join in
    /// `net.rs` (a saved preference picks that side; absent → the human-counting balancer). Present
    /// in BOTH feature configs (only the buttons are dev-gated) so `net.rs` compiles without `dev_panel`.
    #[serde(default)]
    pub preferred_faction: Option<u8>,
    /// R102 Part C — the dev-chosen scenario for the NEXT launch (a small `u8` so we don't depend
    /// on a serde derive for the server `Scenario` enum): `0` = Sandbox, `1` = MiningSkirmish;
    /// `None`/unknown = the code default (`net::SELECTED_SCENARIO`). Set by the dev panel's
    /// "Scenario (next launch)" dropdown; READ once at `net.rs` setup (re-spawning a live world is
    /// out of scope, so the pick applies on the next launch). Present in BOTH feature configs (only
    /// the dropdown is dev-gated) so `net.rs` compiles without `dev_panel`.
    #[serde(default)]
    pub preferred_scenario: Option<u8>,
}

impl Default for DevSettings {
    fn default() -> Self {
        Self {
            tuning: Tuning::default(),
            sim_tuning: SimTuning::default(),
            penetration: PenetrationConfig::default(),
            shield: ShieldConfig::default(),
            salvage: SalvageConfig::default(),
            stat_scaling: StatScalingConfig::default(),
            resistance: default_resistance_matrix(),
            mining: MiningTuning::default(),
            starfield: StarfieldTuning::default(),
            ship_visual: crate::ship_visuals::ShipVisualTuning::default(),
            cell_materials: CellMaterials::default(),
            ai: sim::ai::AiTuning::default(),
            preferred_faction: None,
            preferred_scenario: None,
        }
    }
}

/// Load the dev override from `render_tuning.ron`; fall back to the code `Default`s if absent or
/// unparseable (logging a note on a parse error). Called once at startup. An old file (only
/// `starfield`+`hud`) loads fine — the sim fields fall back to defaults.
pub fn load_dev_settings() -> DevSettings {
    let path = content_dir().join(RENDER_TUNING_RON);
    match std::fs::read_to_string(&path) {
        Ok(s) => match ron::from_str::<DevSettings>(&s) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "render_tuning.ron parse error ({e}) — using defaults: {}",
                    path.display()
                );
                DevSettings::default()
            }
        },
        Err(_) => DevSettings::default(),
    }
}

/// Save the dev override to `render_tuning.ron` (the dev-panel Save). Returns a status string.
pub fn save_dev_settings(dev: &DevSettings) -> Result<String, String> {
    let path = content_dir().join(RENDER_TUNING_RON);
    let s = ron::ser::to_string_pretty(dev, ron::ser::PrettyConfig::default())
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, s).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(format!("saved {}", path.display()))
}

/// R44 — save ONLY the client HUD layout to `hud_layout.ron` (the dev panel's dedicated "Save HUD"
/// button). Returns a status string.
pub fn save_hud_layout(hud: &HudLayout) -> Result<String, String> {
    let path = content_dir().join(HUD_LAYOUT_RON);
    let s = ron::ser::to_string_pretty(hud, ron::ser::PrettyConfig::default())
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, s).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(format!("saved {}", path.display()))
}

/// R44 — load the client HUD layout: `hud_layout.ron` first; ELSE a one-time migration that extracts
/// the legacy `hud` field from `render_tuning.ron` (where it used to live — serde ignores the other
/// keys); ELSE the code default. So existing HUD tweaks survive the split until the user re-saves.
/// Called once at startup.
pub fn load_hud_layout() -> HudLayout {
    let dir = content_dir();
    // The new canonical source.
    if let Ok(s) = std::fs::read_to_string(dir.join(HUD_LAYOUT_RON)) {
        if let Ok(hud) = ron::from_str::<HudLayout>(&s) {
            return hud;
        }
    }
    // Legacy migration: pull just the `hud` field out of the old combined `render_tuning.ron`.
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct LegacyHud {
        hud: HudLayout,
    }
    if let Ok(s) = std::fs::read_to_string(dir.join(RENDER_TUNING_RON)) {
        if let Ok(legacy) = ron::from_str::<LegacyHud>(&s) {
            return legacy.hud;
        }
    }
    HudLayout::default()
}

/// Refinement 39/41 — filter the live catalogs to **canonical seed ids only**, so runtime-injected
/// scenario hulls (Transport/Outpost/MineNode — procedural + huge) are excluded. Used by
/// [`save_catalogs`] before write-back, so a Save can never pollute `ships.ron` with scenario hulls
/// (the bug that earlier produced a 186k-line file + broke the seed-catalog test).
pub fn canonical_design_override(
    modules: &ModuleCatalog,
    hulls: &HullCatalog,
) -> (ModuleCatalog, HullCatalog) {
    let (seed_m, _seed_h) = sim::fitting::seed_catalogs();
    let m = ModuleCatalog {
        modules: modules
            .modules
            .iter()
            .filter(|(id, _)| seed_m.modules.contains_key(id))
            .map(|(id, m)| (*id, m.clone()))
            .collect(),
    };
    // R60 — KEEP every AUTHORED hull (the seed fighter/corvette + any new hull the design editor made),
    // dropping ONLY the runtime-injected scenario procedurals (Transport/Outpost/MineNode) so the
    // editor can persist new designs while `ships.ron` still never gains the huge scenario hulls.
    let scenario = [
        sim::fitting::HULL_TRANSPORT,
        sim::fitting::HULL_OUTPOST,
        sim::fitting::HULL_MINENODE,
    ];
    let h = HullCatalog {
        hulls: hulls
            .hulls
            .iter()
            .filter(|(id, _)| !scenario.contains(id))
            .map(|(id, h)| (*id, h.clone()))
            .collect(),
    };
    (m, h)
}

/// Refinement 41 — write the dev-edited module/hull **DESIGNS** back to the canonical
/// `modules.ron`/`ships.ron` (the user's chosen persistence — these become the real defaults that
/// `load_content_or_default` loads for both headless and windowed). The live catalogs are FILTERED to
/// canonical seed ids (via [`canonical_design_override`]) so the runtime scenario hulls never pollute
/// `ships.ron`.
///
/// A file is rewritten **only when its parsed catalog differs** from the filtered live one (or is
/// absent/unparseable). A no-edit Save therefore leaves the files — and their hand-authored comments —
/// untouched. RON has no comment-preserving writer, so a *real* rewrite strips comments + reorders
/// entries to id order (the accepted cost of write-back). Returns a short status for the dev panel.
pub fn save_catalogs(
    modules: Option<&ModuleCatalog>,
    hulls: Option<&HullCatalog>,
) -> Result<String, String> {
    let (Some(modules), Some(hulls)) = (modules, hulls) else {
        return Ok("no catalog loaded — nothing to save".to_string());
    };
    let (m, h) = canonical_design_override(modules, hulls);
    let dir = content_dir();
    let mut written: Vec<&str> = Vec::new();
    let mut unchanged: Vec<&str> = Vec::new();

    // modules.ron — a serialized `ModuleCatalog` (matches `parse_catalogs`).
    let m_path = dir.join("modules.ron");
    let m_changed = match std::fs::read_to_string(&m_path) {
        Ok(s) => ron::from_str::<ModuleCatalog>(&s)
            .map(|on_disk| on_disk != m)
            .unwrap_or(true),
        Err(_) => true,
    };
    if m_changed {
        let body = ron::ser::to_string_pretty(&m, ron::ser::PrettyConfig::default())
            .map_err(|e| format!("serialize modules: {e}"))?;
        std::fs::write(&m_path, body).map_err(|e| format!("write {}: {e}", m_path.display()))?;
        written.push("modules.ron");
    } else {
        unchanged.push("modules.ron");
    }

    // ships.ron — a serialized `HullCatalog` (matches `parse_catalogs`).
    let h_path = dir.join("ships.ron");
    let h_changed = match std::fs::read_to_string(&h_path) {
        Ok(s) => ron::from_str::<HullCatalog>(&s)
            .map(|on_disk| on_disk != h)
            .unwrap_or(true),
        Err(_) => true,
    };
    if h_changed {
        let body = ron::ser::to_string_pretty(&h, ron::ser::PrettyConfig::default())
            .map_err(|e| format!("serialize ships: {e}"))?;
        std::fs::write(&h_path, body).map_err(|e| format!("write {}: {e}", h_path.display()))?;
        written.push("ships.ron");
    } else {
        unchanged.push("ships.ron");
    }

    if written.is_empty() {
        Ok("designs unchanged — files left intact".to_string())
    } else if unchanged.is_empty() {
        Ok(format!("wrote {}", written.join(" + ")))
    } else {
        Ok(format!(
            "wrote {} (unchanged: {})",
            written.join(" + "),
            unchanged.join(" + ")
        ))
    }
}

/// Refinement 36 — drop-in starfield presets. The directory of `*.ron` presets, beside
/// `render_tuning.ron` (under `$DARK_SILENCE_CONTENT` / `assets/content`).
fn starfield_presets_dir() -> PathBuf {
    content_dir().join("starfield_presets")
}

/// List the available `*.ron` starfield presets as `(display_name, path)`, sorted by name. Empty if
/// the directory is absent/unreadable (the dev panel still shows the built-ins).
pub fn list_starfield_presets() -> Vec<(String, PathBuf)> {
    let dir = starfield_presets_dir();
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("ron") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    out.push((stem.to_string(), path));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Load a single starfield preset RON into a [`StarfieldTuning`].
pub fn load_starfield_preset(path: &std::path::Path) -> Result<StarfieldTuning, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    ron::from_str::<StarfieldTuning>(&s).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Save the current starfield tuning as a named preset RON (creating the presets dir if needed).
/// The name is sanitized to a safe file stem. Returns a short status string for the panel.
pub fn save_starfield_preset(name: &str, starfield: &StarfieldTuning) -> Result<String, String> {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = safe.trim();
    if safe.is_empty() {
        return Err("empty preset name".to_string());
    }
    let dir = starfield_presets_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let path = dir.join(format!("{safe}.ron"));
    let body = ron::ser::to_string_pretty(starfield, ron::ser::PrettyConfig::default())
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(format!("saved preset {}", path.display()))
}
