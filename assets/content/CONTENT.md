# Content catalogs — schema reference & notes

This documents the hand-authored game content under `assets/content/`. The RON files are loaded by
`server::load_content_or_default` (external file if present, else the embedded `include_str!`
fallback baked into the `sim` crate) for **both** the headless server and the windowed client.

> **Why this file exists.** The dev panel's **"Save designs → modules.ron/ships.ron"** button
> (Refinement 41) rewrites these RON files from the live catalog. RON has no comment-preserving
> writer, so a save **strips inline comments and reorders entries to id order**. The explanatory
> notes that used to live as `//` comments inside `modules.ron` now live here, where a save can't
> clobber them. Edit the RON for values; edit this doc for the prose.

---

## Files

| File | Deserializes to | Shape |
| --- | --- | --- |
| `modules.ron` | `sim::fitting::ModuleCatalog` | `( modules: { (id): Module, … } )` |
| `ships.ron` | `sim::fitting::HullCatalog` | `( hulls: { (id): Hull, … } )` |
| `scenario.ron` | `server` `ScenarioContent` | mining-skirmish arena + structures + turrets |
| `render_tuning.ron` | client `DevSettings` | windowed-only dev override (sim tuning + HUD + starfield) |
| `starfield_presets/*.ron` | client `StarfieldTuning` | drop-in starfield presets |

`modules.ron` / `ships.ron` are the **canonical** designs (stats apply to every ship of that
design). `render_tuning.ron` is a **windowed-only override** — it is NOT loaded by the headless
determinism/botkit/demo worlds, so sim tuning there can't break their bit-identical golden state.

---

## `Module` schema (`modules.ron`)

Universal fields on every module:

- `id`, `name`, `kind` (`Reactor`/`Thruster`/`Weapon`/`Shield`/`Armor`/`Sensor`/`Utility`)
- `power_gen` (reactors supply; most modules `0`), `power_draw`, `cpu_draw` — the three budget axes
- `mass` (∑ → ship mass), `heat` (per-shot for weapons), `health_max` (seeds per-cell HP)
- `hardpoint_type` + `hardpoint_size` (`Small`<`Medium`<`Large`<`XLarge`) — slot-fit gates
- `specifics` — the per-kind stat block (one of the variants below)

Per-kind `specifics`:

- **`Reactor`** — no extra params (contributes `power_gen`).
- **`Thruster( propulsion, thrust_force, turn_torque, strafe_force )`** — `propulsion` is a tag
  (`MainDrive`/`Maneuver`/`Rcs`); derivation **sums** thrust/torque/strafe across all thrusters, so
  an engine + an RCS unit combine automatically. The numbers, not the tag, differentiate them.
- **`Shield( shield_hp, regen )`** — health-scaled into the ship's shield pool.
- **`Armor( armor_value )`** — summed into the armor pool capacity.
- **`Sensor( sensor_type, range, resolution )`** — `Radar`/`Lidar`/`Thermal`/`Em`/`Gravimetric`.
  Detection gameplay (AOI/signatures) is a later feature; this is the data shape. No seed hull has a
  Sensor hardpoint yet, so these are catalog-only until a hull authors a Sensor slot.
- **`Utility`** — generic seam; no flight/weapon contribution yet.
- **`Weapon( … )`** — see the weapon model below.

### Seed module ids (modules.ron)

`1` Reactor · `2` Thruster · `3` Autocannon · `4` Shield · `5` Armor Plate · `6` Utility ·
`100` Baseline Thruster (HINT-002 flight-feel reference — outside the player ladder) ·
**Propulsion variants** `10` Ion Drive · `11` Maneuvering Thrusters · `12` RCS Quad ·
**Sensors** `13` Short-Range Radar · `14` Long-Range Array · `15` Passive EM ·
**Weapons** `16` Vulcan · `17` Cannon · `18` Missile Launcher · `19` Plasma Cannon ·
`20` Ion Cannon · `21` Machine Gun · `22` Heavy Machine Gun · `23` Gatling Gun.

---

## Weapon model (Refinement 42) — author REAL specs, the game derives the rest

A weapon's `specifics: Weapon( … )` carries the **delivery taxonomy** plus **real ballistic specs**;
the game **physics-derives** the game-space numbers via the global scales in `SimTuning` (all live in
the dev panel under *"R42 weapon physics"*).

Taxonomy (categorical): `class` (`Ballistic`/`Missile`/`Bomb`/`DirectedEnergy` — only `Ballistic` is
simulated as a projectile today), `ammo` (`Kinetic`/`Shell`/`Rocket`/…), `damage_type` +
`secondary_damage_type` (the armor/resistance `Channel`: `Kinetic`/`ThermalEnergy`/`Blast`/`Em`/
`Radiation`).

Authored REAL specs:

- `caliber_mm` — bore diameter (mm)
- `muzzle_velocity_ms` — real muzzle velocity (m/s)
- `rpm` — rounds per minute
- `spin_up_time` — rotary spool-up seconds to full RPM while firing; `0` = instant (non-rotary)
- `dispersion_deg` — shot dispersion half-angle (a cone of fire); `0` = pinpoint. Deterministic
  per-shot scatter (splitmix64 of owner + shot counter — no RNG).
- `range_units` — projectile travel range in game units

Optional per-field **overrides** (`Some(x)` ⇒ honor it, bypassing physics — used for energy/missile
weapons that don't fit the caliber model; omit / `None` ⇒ derive): `muzzle_speed`, `fire_rate`,
`damage`, `projectile_mass`.

### Derivation (via `SimTuning` scales)

```
muzzle_speed     = muzzle_velocity_ms · velocity_scale         (default 0.2  → 1000 m/s = 200 u/s)
fire_rate        = rpm · rpm_scale                             (default 1/60 → 300 rpm  = 5 shots/s)
projectile_radius= caliber_mm · mm_to_world                    (default 1/150 → 30 mm = 0.2 radius)
projectile_mass  = projectile_density · caliber_mm³            (calibrated so 30 mm ≈ 0.03 slug)
damage           = ½ · projectile_mass · muzzle_velocity_ms² · damage_per_joule   (30 mm ≈ 12 dmg)
lifetime         = range_units / muzzle_speed
```

The arena is **arcade-scaled** (ships ~80 u/s), so real m/s would cross the screen instantly — the
scales keep the *real proportions* (a vulcan round faster than an autocannon shell; a 40 mm shell
bigger/slower than a 20 mm) mapped to the game. The defaults are calibrated so the **30 mm
autocannon reproduces the historical feel** (≈200 u/s, 5 shots/s, ~12 damage, 0.03 slug, 0.2 radius)
— the no-regression anchor.

Because `damage ∝ caliber³ · velocity²`, small rounds hit *much* softer per shot than big shells
(physically faithful) — a machine gun's value is its rate + use against light targets, while
sustained rapid fire is throttled by the energy/heat pools. Dial `damage_per_joule` / `rpm_scale` (or
a per-weapon `damage` override) to taste.

### Determinism note

`projectile_radius` becomes a `CollisionRadius` on the fitted shot (Minkowski-summed onto the target
circle in the broad phase; the narrow-phase cell test is unchanged). The **unfitted** gun and the
scenario turrets stay point/global (no radius), and the golden determinism/botkit/demo worlds use
unfitted ships (or fitted enemies that never fire), so they remain bit-identical.

---

## `Hull` schema (`ships.ron`) — brief

A `Hull` has `id`, `name`, `class`, `role`, `grid_dims (cols, rows)`, a list of authored `cells`
(each `coord` + `section` + optional `slot` of a given `slot_type`/`size`), and base budgets
(`hull_base_mass`, `power_capacity`, `cpu_capacity`, `mass_capacity`). The voxel `cells` are the
carveable silhouette; a `slot` cell is a hardpoint a `Module` installs into (type + size must match).

Seed hulls: `1` Fighter (9×11) · `2` Corvette. Scenario structures (Transport/Outpost/MineNode) are
**injected at runtime** by `spawn_scenario` and are NOT authored here — the dev panel's design save
filters them out so they never pollute `ships.ron`.
