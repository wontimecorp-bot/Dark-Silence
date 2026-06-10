# HUD assets (Refinement 22)

The client loads its HUD type + icons by **handle** at startup (`crates/client/src/fonts.rs`,
`load_hud_assets`). They're referenced by path, so the crate compiles without them — but a given
text/icon won't render until its file is present.

## Text fonts — `assets/fonts/`
| File | Role | Font | License |
|------|------|------|---------|
| `Rajdhani-Medium.ttf` | **Labels** (`FontAssets::label`) — static UI text (SPD, ENRG, RCTR/ENGINE/…) | Rajdhani (Google Fonts) | SIL OFL 1.1 |
| `ShareTechMono-Regular.ttf` | **Numbers** (`FontAssets::mono`) — changing numeric readouts (tabular, no jitter) | Share Tech Mono (Google Fonts) | SIL OFL 1.1 |

Mixed lines (e.g. `SPD 12.3 FLIGHT`) render the label fragments in Rajdhani and the digit fragments
in Share Tech Mono on one line, via Bevy `Text` + `TextSpan` sections.

Swap a face by dropping a `.ttf` and updating `LABEL_FONT` / `MONO_FONT` in `fonts.rs`. The extra
Rajdhani weights (Bold/Light/Regular/SemiBold) are available for future emphasis. Future
title/faction/brand faces: add fields to `FontAssets`.

## egui glyph fallback — `assets/fonts/symbols.ttf` (Refinement 77/87)
The dev **hull editor** + **dev panel** (egui) need symbol glyphs (`▲ ▼ ◀ ▶ ⭯ ⭮ …`) that egui's bundled
font lacks (they'd render as tofu boxes). `install_egui_fonts` (`crates/client/src/hull_editor/mod.rs`)
loads `assets/fonts/symbols.ttf` once via `std::fs` and registers it as an egui **fallback** (used only
for glyphs the body font is missing). It's optional — absent → a one-time log + the glyphs stay boxes;
the build/tests are unaffected.

| File | Role | Font | License |
|------|------|------|---------|
| `symbols.ttf` | egui glyph fallback (arrows / geometric shapes in the editor + dev panel) | **Noto Sans Symbols 2** (`NotoSansSymbols2-Regular.ttf`, R87) | SIL OFL 1.1 — same as the HUD fonts; safe to commit/ship |

Notes (R87, verified against the character maps): font WEIGHTS/widths share identical character
coverage — one Regular per family is all that matters. Base **Noto Sans** has NONE of these symbols
(text family); **Noto Sans Symbols 2** has the triangles/`✕`/`⭯⭮` (not `→ ↶ ↷ ⟳ 💾` — the editor uses
`⭯⭮` for undo/redo and plain text for Load UI / Save UI); **Noto Sans Symbols** (1) has `→` only. The
`Noto_Sans*` source folders here are NOT read by code — only `symbols.ttf` is loaded.

## Icons — `assets/icons/`
HUD icons are **PNG images** (rendered as Bevy `ImageNode`s, tinted + pulsed) — simple + efficient
for a handful of icons. `IconAssets` holds the handles.

| File | Role | Source |
|------|------|--------|
| `module-destroyed.png` | The broken-module alarm icon (`IconAssets::module_destroyed`) | game-icons.net — white-on-transparent, ~128px (e.g. skull-crossed-bones / explosion / broken-bone) |

Save icons **white on transparent** so the in-game red tint + pulse apply. Add more HUD icons later:
drop a PNG in `assets/icons/`, add a field + path const to `IconAssets`/`fonts.rs`.

## Attribution
Rajdhani + Share Tech Mono: SIL OFL 1.1. game-icons.net icons: CC BY 3.0. Add a credit line
(top-level `CREDITS.md` or the in-game about screen).
