//! Refinement 27 — persist the client render tuning (Starfield + HUD) to a hand-editable RON file.
//!
//! The dev-panel-tunable [`StarfieldTuning`] + [`HudLayout`] otherwise live only as code `Default`s,
//! edited in-memory and lost on restart. This loads them from `render_tuning.ron` at startup (so the
//! tuning survives restarts + can be hand-edited) and saves them back from the dev panel's Save
//! button. Mirrors the sim content-RON loaders (`$DARK_SILENCE_CONTENT` / `assets/content`, with the
//! code `Default`s as the fallback). Client render config only — determinism-neutral.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::hud_bars::HudLayout;
use crate::starfield::StarfieldTuning;

/// File name (under the content dir) holding the persisted render tuning.
const RENDER_TUNING_RON: &str = "render_tuning.ron";

/// The on-disk wrapper: both render-tuning resources in one RON. `Default` = the code defaults of
/// both (the fallback when no file is present).
#[derive(Default, Serialize, Deserialize)]
pub struct RenderTuning {
    pub starfield: StarfieldTuning,
    pub hud: HudLayout,
}

/// The content dir (`$DARK_SILENCE_CONTENT` if set, else `assets/content/` relative to the CWD —
/// mirrors `server::load_content_or_default`) joined with [`RENDER_TUNING_RON`].
fn render_tuning_path() -> PathBuf {
    let dir = std::env::var_os("DARK_SILENCE_CONTENT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("assets/content"));
    dir.join(RENDER_TUNING_RON)
}

/// Load the render tuning from `render_tuning.ron`; fall back to the code `Default`s if the file is
/// absent or unparseable (logging a note on a parse error). Called once at startup.
pub fn load_render_tuning() -> RenderTuning {
    let path = render_tuning_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => match ron::from_str::<RenderTuning>(&s) {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!(
                    "render_tuning.ron parse error ({e}) — using defaults: {}",
                    path.display()
                );
                RenderTuning::default()
            }
        },
        // Absent file → silently use defaults (first run / not yet saved).
        Err(_) => RenderTuning::default(),
    }
}

/// Save the current render tuning to `render_tuning.ron` (the dev-panel Save button). Returns a short
/// status string (Ok or Err) for the panel to display.
pub fn save_render_tuning(starfield: &StarfieldTuning, hud: &HudLayout) -> Result<String, String> {
    let path = render_tuning_path();
    let rt = RenderTuning {
        starfield: *starfield,
        hud: *hud,
    };
    let s = ron::ser::to_string_pretty(&rt, ron::ser::PrettyConfig::default())
        .map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, s).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(format!("saved {}", path.display()))
}

/// Refinement 36 — drop-in starfield presets. The directory of `*.ron` presets, beside
/// `render_tuning.ron` (under `$DARK_SILENCE_CONTENT` / `assets/content`).
fn starfield_presets_dir() -> PathBuf {
    let dir = std::env::var_os("DARK_SILENCE_CONTENT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("assets/content"));
    dir.join("starfield_presets")
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
