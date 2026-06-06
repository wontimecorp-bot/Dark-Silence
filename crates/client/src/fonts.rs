//! Refinement 21/22 — the shared HUD text fonts + icon images.
//!
//! Bevy renders all [`Text`] through a glyph atlas (cosmic-text): glyphs are rasterized once per
//! `(font, size)` into a shared texture, then drawn as cheap batched quads. We give the HUD a small
//! type system loaded once at `PreStartup`:
//! - [`FontAssets::label`] — Rajdhani — for LABELS (static UI text).
//! - [`FontAssets::mono`] — Share Tech Mono — for CHANGING NUMERIC readouts (tabular digits, so a
//!   value updating does not reflow/jitter the line). Mixed label+number lines use both fonts via
//!   Bevy `Text` + `TextSpan` sections.
//! - [`IconAssets`] — PNG **images** (rendered as UI `ImageNode`s, tinted + pulsed) for HUD icons —
//!   individual PNGs are simple + plenty efficient for a handful of icons.
//!
//! The asset files live under `assets/fonts/` + `assets/icons/`. They are referenced by HANDLE
//! (loaded asynchronously), so the crate compiles even before the files exist; until a file is
//! present, the text/icon using it simply does not render (Bevy logs a missing-asset warning).

use bevy::prelude::*;

/// Path (under `assets/`) of the LABEL text font — Rajdhani (clean condensed techy face).
pub const LABEL_FONT: &str = "fonts/Rajdhani-Medium.ttf";

/// Path (under `assets/`) of the MONO numeric font — Share Tech Mono (tabular digits for changing
/// readouts, so updating values don't shift the line).
pub const MONO_FONT: &str = "fonts/ShareTechMono-Regular.ttf";

/// Path (under `assets/`) of the "module destroyed" HUD icon — a white-on-transparent PNG, tinted
/// red + pulsed in-game (see `module_bars`).
pub const ICON_MODULE_DESTROYED_PNG: &str = "icons/module-destroyed.png";

/// Shared HUD TEXT fonts (Refinement 22). Loaded once by [`load_hud_assets`] at `PreStartup`; HUD
/// `TextFont`s clone [`label`](FontAssets::label) for labels and [`mono`](FontAssets::mono) for the
/// changing-number text spans. (Future title/faction/brand faces: add fields here.)
#[derive(Resource)]
pub struct FontAssets {
    /// Labels / static UI text — Rajdhani.
    pub label: Handle<Font>,
    /// Changing numeric readouts — Share Tech Mono (tabular).
    pub mono: Handle<Font>,
}

/// Shared HUD icon images (Refinement 22). PNG images rendered as UI `ImageNode`s (tinted + pulsed).
/// (Future per-module-type / faction icons: add fields here.)
#[derive(Resource)]
pub struct IconAssets {
    /// The "module destroyed" alarm icon.
    pub module_destroyed: Handle<Image>,
}

/// `PreStartup`: load the HUD text fonts + icon images and insert [`FontAssets`] + [`IconAssets`] so
/// the Startup HUD setups can clone the handles. Assets load asynchronously; they render once their
/// file is available.
pub fn load_hud_assets(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(FontAssets {
        label: assets.load(LABEL_FONT),
        mono: assets.load(MONO_FONT),
    });
    commands.insert_resource(IconAssets {
        module_destroyed: assets.load(ICON_MODULE_DESTROYED_PNG),
    });
}
