# Dots (Fabric) Perceptual Parity Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close five perceptual parity gaps Madison found testing the freshly-ported Dots sketch: bright/steady audio (vs v4's low warm in-out pulse), hands not rotating the hue-split, hand-grab power far exceeding the mouse, a too-easily-permanent particle tangle, a missing Dots settings panel, and attract-mode dimming.

**Architecture:** Build the Dots settings panel first as the keystone, then wire each fix to a live, ear/eye-tunable knob with a v4/Line-matched default. Reuse Line's proven mechanisms (focal-ease smoothing, the shared `attract_color` brightness lift). The audio fix is "modeled breath" (a synthesized in-out swell + a warmer/slower filter), the fabric fix is a true immutable home plus a linear restoring spring. Final feel-values are Madison's via the panel; this plan ships the structure and defaults.

**Tech Stack:** Rust / Bevy 0.19 (ECS, reflection-driven egui settings panel, `ExtractResourcePlugin`), WGSL compute (`simulate.wgsl`) + `bytemuck` `#[repr(C)]` uniforms, fundsp on the real-time audio thread driven by a lock-free `SetDotsParam` ring.

## Global Constraints

- **No allocation in hot paths.** Per-frame Bevy systems, the audio callback, and worker loops pre-allocate and reuse. All new per-frame arithmetic is on stack scalars (existing Dots systems already document this).
- **Audio thread is real-time-friendly.** Synth param changes flow only through `AudioCommand::SetDotsParam` on the lock-free ring; `DotsSynth::set_param` / `tick_mono` stay allocation-free (atomic `Shared::set` only). No new Mutex/alloc after init.
- **Line behavior is preserved.** The only Line-touching change is Task 8 (promote `attract_color_params` to a shared helper) and the audio/particle tasks share no Line code paths. Task 8 MUST produce byte-identical `attract_color` output for Line (same formula, same call sites).
- **Kernel parity.** `assets/shaders/particles/simulate.wgsl` and `crates/wc-sketches/src/particles/sim_cpu.rs` (`step_one`) MUST change together and stay term-for-term identical. (Pre-existing carry-forward: the CPU turbulence advection already diverges from the WGSL kernel — do NOT widen that gap; the spring terms must match exactly.)
- **Uniform layout.** `SimParams` MUST remain a multiple of 16 bytes and the `attractors` array MUST stay 16-byte aligned. The scalar header is currently exactly 64 bytes; adding `restoring_linear: f32` re-pads it to 80 (still a multiple of 16). The `const _: () = { assert!(size_of::<SimParams>().is_multiple_of(16)); … }` block in `particle.rs` is the guard.
- **Settings forward-compat.** Every new `DotsSettings` field gets BOTH a `#[setting(default = …)]` attribute AND a paired `#[serde(default = "default_<name>")]` with a matching `default_<name>() -> T` free function whose literal equals the `#[setting]` default. Update both sites together (the file's module docs mandate this).
- **No new dependencies.** Reuse crates already in the graph (`cargo tree -i` before adding anything).
- **Pre-release, single operator.** No backward-compat/migration shims. A re-do via the UI or a one-off fix is preferred over a permanent compatibility layer.
- **Tunable, not hardcoded.** Every value that needs Madison's ears or eyes ships as a `DotsSettings` knob with a sensible default; she dials final values live in the ADVANCED panel.
- **`cargo xtask check-secrets` clean** — no home-dir paths, emails, or secret prefixes in any added code or comment.

### Chosen default values (single source of truth)

| Knob (field) | Default | Category | Section | Why |
|---|---|---|---|---|
| `dot_spacing` *(exists)* | 20.0 | Dev | Particles | v4 |
| `gravity_constant` *(new)* | 100.0 | User | Particles | v4 `GRAVITY_CONSTANT`; mouse stays well-calibrated |
| `hand_power_scale` *(new)* | 0.3 | Dev | Particles | brings full close grab (~500–2500) down toward the mouse's ~200 cap |
| `fabric_tension` *(new)* | 1.0 | User | Particles | linear home-spring strength ("Return strength") |
| `gamma` *(exists, promote)* | 1.0 | **User** | Visual | read live; drop the spurious `requires_restart` |
| `shrink_factor` *(new)* | 0.98 | Dev | Visual | v4 explode tightness (was hardcoded) |
| `explode_focal_smoothing` *(new)* | 0.25 | Dev | Visual | hand hue-split ease τ (s); matches Line `smear_focal_smoothing` |
| `synth_volume_scale` *(new)* | 1.0 | User | Audio | master trim (mirror Line) |
| `synth_attack_ms` *(new)* | 115.0 | User | Audio | envelope attack (mirror Line; preserves current rate) |
| `synth_release_ms` *(new)* | 350.0 | User | Audio | envelope release (mirror Line; preserves current rate) |
| `bandpass_base_hz` *(new)* | 110.0 | Dev | Audio | warm anchor near the 82/165 Hz fundamentals (was 200) |
| `bandpass_range_hz` *(new)* | 280.0 | Dev | Audio | full-press cutoff ≈ 390 Hz, not 2000 (was 1800) |
| `breath_rate_hz` *(new)* | 0.7 | Dev | Audio | modeled in-out swell rate |
| `breath_depth` *(new)* | 0.3 | User | Audio | modeled in-out swell depth (0 = off) |
| `attract_particle_fraction` *(exists)* | 0.6 | Dev | Screensaver | — |
| `attract_turbulence` *(exists)* | 6.0 | Dev | Screensaver | — |
| `attract_brightness` *(new)* | 2.2 | Dev | Screensaver | AgX white-knee lift (mirror Line) |
| `attract_color_strength` *(new)* | 0.0 | Dev | Screensaver | velocity tint; Dots' calm drift won't trigger WAKE, default off |

Plus one synth constant change: `DotsSynth` `LFO_RATE_HZ` 8.66 → **1.5** (a fixed const, not a knob — Madison chose modeled breath over driving the LFO rate).

---

## Task group F1 — Settings panel (the keystone)

### Task 1: Active-sketch settings tab + section reorganization

**Files:**
- Modify: `crates/wc-core/src/settings/panel_user.rs` (the `SettingsTab` enum, `ORDER`, `tab_for_storage_key`, `draw_dock_header`, and the render-loop gate in `draw_user_panel`)
- Modify: `crates/wc-sketches/src/dots/settings.rs` (add `section = …` to the existing 4 fields; promote `gamma` to `User` and drop its `requires_restart`)
- Test: `crates/wc-core/src/settings/panel_user.rs` `#[cfg(test)] mod tests` (tab routing) and `crates/wc-sketches/src/dots/settings.rs` tests (unchanged defaults still pass)

**Interfaces:**
- Consumes: `bevy::state::state::State<AppState>` (already a resource), `SettingsRegistry` (already iterated in `draw_user_panel`).
- Produces: a `SettingsTab::Sketch` variant routed from BOTH `"line"` and `"dots"`, rendered for only the active sketch; a dynamic tab label (`"LINE"` in Line, `"FABRIC"` in Dots).

**Design.** Today `SettingsTab` is `Line | HandTracking | Display`, default `Line`, with a hardcoded `"LINE"` label and `tab_for_storage_key("line") => Line`. Replace the sketch-specific `Line` variant with a generic active-sketch tab:

- Rename variant `Line` → `Sketch` (keep it `#[default]`).
- `tab_for_storage_key`: route `"line" => SettingsTab::Sketch` AND `"dots" => SettingsTab::Sketch`; everything else unchanged (`"hand_tracking" => HandTracking`, `_ => Display`).
- Add a helper mapping the active `AppState` to the storage key of the sketch whose settings belong in the Sketch tab, and its label:
  ```rust
  /// Storage key + header label of the settings struct shown in the active-sketch
  /// tab, by current AppState. Sketches whose settings route to `SettingsTab::Sketch`
  /// are gated on this so only the running sketch's knobs render (both `LineSettings`
  /// and `DotsSettings` are always registered).
  fn active_sketch_tab(state: AppState) -> (&'static str, &'static str) {
      match state {
          AppState::Dots => ("dots", "FABRIC"),
          // Line is the default/home-adjacent sketch label.
          _ => ("line", "LINE"),
      }
  }
  ```
- `ORDER` no longer carries a literal label for the first tab. Either drop the label from `ORDER` and build the row labels at render time, or keep `ORDER` as the tab sequence and override the `Sketch` tab's label with `active_sketch_tab(state).1`. Simplest: keep `ORDER` for sequence, and in `draw_dock_header` substitute the live label for the `Sketch` entry. Thread the active-sketch label into `draw_dock_header` as a parameter.
- In `draw_user_panel`, read the active state once (before the egui borrow), e.g. `let app_state = *world.resource::<State<AppState>>().get();` (it is already read in the `settings_panel_visible` run condition, so the resource is present). Pass `active_sketch_tab(app_state)` down. In the render loop, change the gate so a key routed to `Sketch` renders ONLY when it equals the active-sketch key:
  ```rust
  for key in &keys {
      let tab = tab_for_storage_key(key);
      if tab != selected_tab { continue; }
      if tab == SettingsTab::Sketch && *key != active_key { continue; } // active sketch only
      render_section_by_key(world, ui, key, /* … */);
      render_custom_sections(world, ui, key, &style);
  }
  ```

**DotsSettings reorganization (no behavior change):** add `section = "Particles"` to `dot_spacing`; `section = "Visual"` to `gamma` and change its `category = Dev`→`User` and remove `requires_restart` (it is read live every frame in `post_params.rs:75`); add `section = "Screensaver"` to `attract_particle_fraction` and `attract_turbulence`. Defaults and serde functions are unchanged, so the existing `settings.rs` tests still pass.

- [ ] **Step 1: Write the failing test** — tab routing.

In `panel_user.rs` tests:
```rust
#[test]
fn dots_and_line_route_to_sketch_tab() {
    assert_eq!(tab_for_storage_key("line"), SettingsTab::Sketch);
    assert_eq!(tab_for_storage_key("dots"), SettingsTab::Sketch);
    assert_eq!(tab_for_storage_key("hand_tracking"), SettingsTab::HandTracking);
    assert_eq!(tab_for_storage_key("overlay_ui"), SettingsTab::Display);
}

#[test]
fn active_sketch_tab_label_follows_state() {
    assert_eq!(active_sketch_tab(AppState::Dots), ("dots", "FABRIC"));
    assert_eq!(active_sketch_tab(AppState::Line), ("line", "LINE"));
}
```

- [ ] **Step 2: Run the test to verify it fails** — `cargo test -p wc-core --lib settings::panel_user` → FAIL (`SettingsTab::Sketch` / `active_sketch_tab` not defined).
- [ ] **Step 3: Implement** the enum rename, `tab_for_storage_key` routes, `active_sketch_tab` helper, `draw_dock_header` label parameter, the `draw_user_panel` active-state read + render-loop gate, and the `DotsSettings` `section`/`category` edits.
- [ ] **Step 4: Run the tests** — `cargo test -p wc-core --lib settings::panel_user` and `cargo test -p wc-sketches --lib dots::settings` → PASS.
- [ ] **Step 5: Commit** — `feat(dots): active-sketch settings tab + Fabric section layout`.

**Verification note for the operator (not a step):** in `cargo rund`, entering Dots and opening settings (cog) should show a **FABRIC** tab (not LINE) with Particles/Visual/Screensaver sections; the existing knobs appear, and ADVANCED reveals the Dev ones. The Line tab must still read **LINE** with its full knob set.

---

### Task 2: Dots settings restart listener

**Files:**
- Modify: `crates/wc-sketches/src/dots/mod.rs` (`DotsPlugin::build` — add a `restart_on_settings_change`-style listener filtered to `DotsSettings::STORAGE_KEY`, mirroring `crates/wc-sketches/src/line/mod.rs:238,310-348`)
- Test: `crates/wc-sketches/src/dots/mod.rs` `#[cfg(test)] mod tests`

**Why:** `dot_spacing` is `requires_restart` (the compute buffer is sized at spawn), and `emit_restart_events` already fires `SketchRestart { storage_key: "dots" }` when it changes — but nothing listens, so the grid never rebuilds. Read `line/mod.rs`'s listener for the exact pattern (it re-enters the sketch state / re-runs spawn on a matching `SketchRestart`).

- [ ] **Step 1: Write the failing test** — a `SketchRestart { storage_key: "dots" }` event triggers the Dots restart path (mirror the Line listener's test if one exists; otherwise assert the system is registered and consumes the event). If a behavioral test is impractical headless, assert the listener system is added to the schedule.
- [ ] **Step 2: Run it** — FAIL.
- [ ] **Step 3: Implement** the listener mirroring Line.
- [ ] **Step 4: Run tests** — `cargo test -p wc-sketches --lib dots` → PASS.
- [ ] **Step 5: Commit** — `feat(dots): rebuild grid on dot_spacing settings change`.

---

## Task group F2 — Audio parity (warm, breathing)

### Task 3: Slow the filter LFO and correct the "8.66 Hz" comments

**Files:**
- Modify: `crates/wc-core/src/audio/dots_synth.rs` (`LFO_RATE_HZ`; the module doc, the graph-shape ASCII, and the inline comments that call 8.66 Hz "v4's LFO rate")
- Test: existing `dots_synth.rs` tests (must still pass)

**Why:** 8.66 Hz is only v4's *construction-time placeholder* for `lfo.frequency`; v4 overwrites it every frame with `flatRatio` (~1–3 Hz). The fixed 8.66 Hz wobble is a primary source of the excess high-frequency shimmer. Lower it to a warm 1.5 Hz fixed rate (Line seeds its equivalent at 1.0 Hz).

- [ ] **Step 1: Change `LFO_RATE_HZ`** `8.66` → `1.5` (`dots_synth.rs:61`).
- [ ] **Step 2: Correct the misleading comments** at `dots_synth.rs:7-8`, `:24`, `:26`, `:60`, and the graph-shape `sine(8.66)` in the ASCII (`:24,26`): note that 8.66 Hz was v4's overwritten placeholder, and the v5 voice runs a fixed warm ~1.5 Hz wobble (modeled-breath approach; the in-out swell is synthesized in the coupling, see Task 4).
- [ ] **Step 3: Run tests** — `cargo test -p wc-core --lib audio::dots_synth` → PASS (none assert the LFO rate; `volume_positive_produces_audio` still holds).
- [ ] **Step 4: Commit** — `fix(dots-audio): slow the filter LFO to a warm 1.5 Hz (8.66 was a v4 placeholder)`.

---

### Task 4: Settings-driven warm cutoff + modeled breath

**Files:**
- Modify: `crates/wc-sketches/src/dots/audio_coupling.rs` (make the bandpass window, attack/release, and volume read from `DotsSettings`; add the modeled breath; refactor the pure helpers to take parameters)
- Modify: `crates/wc-sketches/src/dots/settings.rs` (add `synth_volume_scale`, `synth_attack_ms`, `synth_release_ms`, `bandpass_base_hz`, `bandpass_range_hz`, `breath_rate_hz`, `breath_depth` with serde defaults, `section = "Audio"`)
- Test: `audio_coupling.rs` `#[cfg(test)] mod tests` (rewrite the const-based assertions to parameterized ones; add breath tests)

**Design.** `drive_dots_audio` currently keys volume/cutoff off a boolean activity envelope with hardcoded `BANDPASS_BASE_HZ`/`BANDPASS_RANGE_HZ`/`ENVELOPE_*_RATE` consts. Change it to:

1. Take `settings: Res<DotsSettings>`.
2. Derive the envelope rates from settings: `attack_rate = 1000.0 / settings.synth_attack_ms`, `release_rate = 1000.0 / settings.synth_release_ms`. Make `step_dots_envelope` take `(envelope, active_power, dt, attack_rate, release_rate)` instead of reading the module consts. (Keep the `(rate*dt).min(1.0)` clamp and `[0,1]` clamp.)
3. Compute a **modeled breath** gated by the envelope so rest is silent:
   ```rust
   // Modeled in-out swell: a slow sine, scaled by the activity envelope so it
   // is silent at rest and swells in with the press. Recreates v4's "low warm
   // pulse following the in-out particle motion" without GPU field stats.
   let t = time.elapsed_secs();
   let breath = 1.0 + settings.breath_depth * env
       * (core::f32::consts::TAU * settings.breath_rate_hz * t).sin();
   ```
4. Apply breath to both volume and cutoff, and the volume trim:
   ```rust
   let volume = (env * breath * settings.synth_volume_scale).max(0.0);
   let bandpass_freq = dots_bandpass_freq(env, settings.bandpass_base_hz, settings.bandpass_range_hz) * breath;
   let lfo_depth = dots_lfo_depth(bandpass_freq); // unchanged × 0.06 relation
   ```
   `dots_bandpass_freq(envelope, base, range)` becomes `base + envelope * range`. `dots_lfo_depth` is unchanged (`bandpass_freq * 0.06`). The synth clamps cutoff ≥ 1 Hz and `lfo_depth ≥ 0` already, so the breath dip cannot push them negative.
5. Keep the three `SetDotsParam` pushes (`"volume"`, `"bandpass_freq"`, `"lfo_depth"`) and the ring-full warn.

**Note on `elapsed_secs`:** `Time::elapsed_secs()` is monotonic and already used by the screensaver driver; it does not allocate. The breath is allocation-free (stack scalars).

The `BANDPASS_BASE_HZ`/`BANDPASS_RANGE_HZ`/`ENVELOPE_ATTACK_RATE`/`ENVELOPE_RELEASE_RATE` consts are removed (their values move to the settings defaults). Keep `LFO_DEPTH_OVER_CUTOFF = 0.06` and `HAND_ACTIVITY_THRESHOLD`.

**Settings additions** (`dots/settings.rs`, all `section = "Audio"`, with paired serde defaults):
```rust
// User
synth_volume_scale: f32  // default 1.0, min 0.0, max 2.0, step 0.05, label "Synth volume"
synth_attack_ms: f32     // default 115.0, min 5.0, max 200.0, step 5.0, label "Synth attack", unit "ms"
synth_release_ms: f32    // default 350.0, min 100.0, max 3000.0, step 50.0, label "Synth release", unit "ms"
breath_depth: f32        // default 0.3, min 0.0, max 1.0, step 0.05, label "Breath depth"
// Dev
bandpass_base_hz: f32    // default 110.0, min 50.0, max 1000.0, step 10.0, label "Bandpass base", unit "Hz"
bandpass_range_hz: f32   // default 280.0, min 50.0, max 4000.0, step 10.0, label "Bandpass range", unit "Hz"
breath_rate_hz: f32      // default 0.7, min 0.1, max 4.0, step 0.1, label "Breath rate", unit "Hz"
```

- [ ] **Step 1: Add the seven `DotsSettings` fields** with `#[setting(...)]`, `#[serde(default = "default_<name>")]`, and `default_<name>()` functions; extend the `default_values_match_serde_defaults` test to cover them.
- [ ] **Step 2: Run** `cargo test -p wc-sketches --lib dots::settings` → PASS (fields + defaults).
- [ ] **Step 3: Write/adjust the failing coupling tests.** Rewrite `bandpass_freq_at_zero_envelope_equals_base` etc. to call `dots_bandpass_freq(env, base, range)` with explicit args; add:
  - `breath_at_zero_envelope_is_unity` — with `env = 0`, `breath = 1.0` for any `t` (envelope gates the swell so rest is unmodulated).
  - `breath_modulates_within_bounds` — for `env = 1`, `depth = 0.3`, the breath stays in `[0.7, 1.3]` across a sweep of `t`.
  - Keep `lfo_depth_equals_bandpass_times_point_zero_six` against the new `dots_bandpass_freq` signature.
- [ ] **Step 4: Run** `cargo test -p wc-sketches --lib dots::audio_coupling` → FAIL (new signatures/helpers absent).
- [ ] **Step 5: Implement** the `drive_dots_audio` rewrite and helper-signature changes.
- [ ] **Step 6: Run** `cargo test -p wc-sketches --lib dots` → PASS.
- [ ] **Step 7: Commit** — `feat(dots-audio): warm settings-driven cutoff + modeled in-out breath`.

**Operator tuning note (not a step):** by ear via the FABRIC → Audio panel — `bandpass_base_hz`/`bandpass_range_hz` for warmth, `breath_rate_hz`/`breath_depth` for the in-out pulse, `synth_attack_ms`/`synth_release_ms` for feel. If the fixed 1.5 Hz filter wobble still reads wrong after this, the fallback is to expose the synth LFO rate as a `Shared` (the deferred "faithful LFO-rate drive").

---

## Task group F3 — Particles

### Task 5: Immutable home + linear restoring spring (kernel + CPU mirror)

**Files:**
- Modify: `crates/wc-sketches/src/particles/particle.rs` (add `restoring_linear: f32` + repad `SimParams` to 80 bytes)
- Modify: `assets/shaders/particles/simulate.wgsl` (struct field + spring block: add linear term, remove the idle home-drift)
- Modify: `crates/wc-sketches/src/particles/sim_cpu.rs` (mirror both edits term-for-term)
- Test: `particle.rs` layout asserts; `sim_cpu.rs` `#[cfg(test)] mod tests` (a kernel-parity / return-to-home test)

**Design.** Two changes, both in lockstep across `simulate.wgsl` and `sim_cpu.rs`:

1. **Remove the idle home-drift** — delete the `if (params.attractor_count == 0u) { p.original_xy = p.original_xy - home * 0.05; }` block (`simulate.wgsl:159-161` and its `sim_cpu.rs` mirror). This is the permanent-tangle bug: it slides the only home reference toward the deformed position ~25–30× faster than the spring returns the particle, so they meet at the tangle and the grid is lost. After removal, `original_xy` is a true immutable home and the field always relaxes toward the literal spawn grid (also resolves the screensaver soak-watch concern — respawns return to the real grid).
2. **Add a linear restoring term** alongside the existing quadratic. New WGSL/Rust uniform field `restoring_linear`. Kernel:
   ```wgsl
   // Home spring (gated; Line passes 0/0 -> full no-op). Quadratic term is v4's
   // STATIONARY_CONSTANT (big-displacement snap); the linear term gives a
   // graceful, complete return toward the immutable home (fabric tension).
   if (params.stationary_constant > 0.0 || params.restoring_linear > 0.0) {
       let home = p.original_xy - p.position;
       let home_len = length(home);
       accel = accel + params.stationary_constant * home * home_len
                     + params.restoring_linear * home;
   }
   ```
   (No `original_xy` write anymore.)

**Uniform layout (`particle.rs`).** Insert after `stationary_constant`:
```rust
    /// Linear (Hookean) home-spring coefficient — the "fabric tension" that eases
    /// each particle all the way back to its immutable `original_xy`, not just most
    /// of the way (the quadratic `stationary_constant` term goes soft near home).
    /// Baked from `DotsSettings::fabric_tension` during live play and `0.0` during
    /// the screensaver (so the turbulence morph is unimpeded). `0.0` is a no-op
    /// (Line passes 0.0). Added with three pad floats so the scalar header stays a
    /// 16-byte multiple (64 → 80) and the `attractors` array remains 16-byte aligned.
    pub restoring_linear: f32,
    /// Padding to restore 16-byte alignment for the `attractors` array after
    /// adding `restoring_linear`. Never read by the kernel.
    #[allow(clippy::pub_underscore_fields, reason = "GPU struct layout padding must be pub for bytemuck")]
    pub _spring_pad: [f32; 3],
```
WGSL `struct SimParams` (after `stationary_constant: f32,`):
```wgsl
    restoring_linear: f32,
    _spring_pad0: f32,
    _spring_pad1: f32,
    _spring_pad2: f32,
```
Update the `stationary_constant` doc/byte comment in `particle.rs` (currently says the header is 64 bytes) to note it is now 80 with `restoring_linear` + pad, still 16-byte-multiple. The `const _: ()` assert block is unchanged and continues to guard the invariant.

**Baker default.** `bake_dots_sim_params` will be threaded with the tension value in Task 6; in THIS task, set `restoring_linear: 0.0` in the baker literal so the field exists and the layout/kernel land with no behavior change beyond the drift removal. The drift removal alone is the bug fix and is independently testable.

- [ ] **Step 1: Write the failing CPU-mirror test** in `sim_cpu.rs`: seed one particle displaced from its home with `restoring_linear > 0`, no attractors, step it N times, assert it converges toward `original_xy` AND that `original_xy` itself never changes (the immutable-home guarantee). Add a second assertion that with `restoring_linear == 0` and `stationary_constant == 0` the particle does not move toward home (Line no-op).
- [ ] **Step 2: Run** `cargo test -p wc-sketches --lib particles::sim_cpu` → FAIL (`restoring_linear` field absent; `step_one` ignores it; drift still mutates home).
- [ ] **Step 3: Implement** the `particle.rs` field+pad, the `simulate.wgsl` struct+spring edits, and the `sim_cpu.rs` mirror (field read, linear term, drift removed).
- [ ] **Step 4: Run** `cargo test -p wc-sketches --lib particles` → PASS. Also `cargo build -p waveconductor` so naga validates the WGSL struct/kernel.
- [ ] **Step 5: Commit** — `fix(dots): immutable particle home + linear fabric restoring spring`.

---

### Task 6: Bake fabric tension + hand-power scale + gravity from settings

**Files:**
- Modify: `crates/wc-sketches/src/dots/systems/sim_params.rs` (`bake_dots_sim_params` gains a `restoring_linear` parameter; `update_dots_sim_params` reads `DotsSettings`; the screensaver driver passes `0.0`)
- Modify: `crates/wc-sketches/src/dots/screensaver.rs` (`drive_dots_attract` passes `restoring_linear = 0.0`)
- Modify: `crates/wc-sketches/src/dots/settings.rs` (add `gravity_constant`, `hand_power_scale`, `fabric_tension` with serde defaults, `section = "Particles"`)
- Test: `sim_params.rs` tests (tension/power threading); `settings.rs` defaults

**Design.**
- Add a `restoring_linear: f32` parameter to `bake_dots_sim_params` (place it after `turbulence`, or fold gate/turbulence/spring into the call as today plus the new scalar). Set `SimParams.restoring_linear` from it; keep `stationary_constant: 0.01` (v4).
- `update_dots_sim_params` (live writer): add `settings: Res<DotsSettings>`. Replace the `DOTS_GRAVITY_CONSTANT` mouse/hand bakes with `settings.gravity_constant`, and multiply the hand bake by `settings.hand_power_scale`:
  - mouse: `power: mouse.power * settings.gravity_constant`
  - hand: `power: hand_attractor.power * settings.gravity_constant * settings.hand_power_scale`
  - Pass `restoring_linear: settings.fabric_tension` to the baker.
  Keep `DOTS_GRAVITY_CONSTANT = 100.0` as the const backing `default_gravity_constant()`.
- `drive_dots_attract` (screensaver): pass `restoring_linear: 0.0` to the baker so the spring does not fight the turbulence morph. (The DotsSettings is already a parameter there.)

**Settings additions** (`section = "Particles"`):
```rust
gravity_constant: f32  // User, default 100.0, min 0.0, max 500.0, step 10.0, label "Gravity"
hand_power_scale: f32  // Dev,  default 0.3,   min 0.0, max 2.0,   step 0.05, label "Hand power scale"
fabric_tension: f32    // User, default 1.0,   min 0.0, max 5.0,   step 0.1,  label "Fabric tension"
```

- [ ] **Step 1: Add the three `DotsSettings` fields** + serde defaults; extend the defaults test.
- [ ] **Step 2: Run** `cargo test -p wc-sketches --lib dots::settings` → PASS.
- [ ] **Step 3: Write failing `sim_params.rs` tests:** (a) with `fabric_tension = 2.0` set in `DotsSettings`, `update_dots_sim_params` writes `restoring_linear = 2.0`; (b) with `hand_power_scale = 0.3` and a hand of raw power 0.5, the baked hand attractor power is `0.5 * 100.0 * 0.3 = 15.0`; (c) the existing mouse-power test still holds with default `gravity_constant = 100.0`. Update `setup_world` to insert a `DotsSettings` resource.
- [ ] **Step 4: Run** `cargo test -p wc-sketches --lib dots::systems::sim_params` → FAIL.
- [ ] **Step 5: Implement** the baker parameter, the live-writer settings reads, and the screensaver `0.0` pass-through.
- [ ] **Step 6: Run** `cargo test -p wc-sketches --lib dots` → PASS (including the screensaver test, which now passes `0.0`).
- [ ] **Step 7: Commit** — `feat(dots): live fabric-tension, hand-power, and gravity knobs`.

**Operator tuning note (not a step):** `fabric_tension` (graceful return strength) and `hand_power_scale` (bring grab down to mouse feel) are eye-tuned live in the FABRIC → Particles panel.

---

### Task 7: Hands rotate the hue-split (focal smoothing) + shrink_factor knob

**Files:**
- Modify: `crates/wc-sketches/src/dots/systems/post_params.rs` (`update_dots_post_params` drives `i_mouse` from a smoothed hand focal when a hand is grabbing, else the mouse; reads `shrink_factor` from settings)
- Modify: `crates/wc-sketches/src/dots/mod.rs` (insert/remove a `DotsExplodeFocal` resource on `OnEnter`/`OnExit(AppState::Dots)`; ensure `update_dots_post_params` runs after `update_dots_hand_attractors`)
- Modify: `crates/wc-sketches/src/dots/settings.rs` (add `explode_focal_smoothing`, `shrink_factor`, `section = "Visual"`)
- Test: `post_params.rs` tests (hand overrides mouse; mouse-only unchanged; no-hand+no-cursor holds)

**Design.** Mirror Line's gravity-smear focal smoothing into Dots' explode `i_mouse` (the chromatic spiral center). Reuse Line's helpers `ease_focal`, `weighted_focal`, and `FOCAL_CENTER_WEIGHT` from `crate::line::systems::sim_params` (all `pub`).

- New world-space focal resource `DotsExplodeFocal(pub Vec2)`, inserted at `Vec2::ZERO` on `OnEnter(AppState::Dots)` and removed on `OnExit`, mirroring `LineSmearFocal`.
- `update_dots_post_params` gains `time: Res<Time>`, `dots_hands: Query<&DotsHandAttractor, With<TrackedHand>>`, and `focal: ResMut<DotsExplodeFocal>`. It still writes `i_resolution`, `shrink_factor` (now `settings.shrink_factor`), and `gamma`. Replace the `i_mouse` write with:
  ```rust
  let dt = time.delta_secs();
  // Gather grabbing-hand samples (raw power weight, world position) — like Line.
  let mut samples: smallvec / fixed-cap stack array of (f32, Vec2)  // no heap alloc
  for hand in &dots_hands { if hand.power.abs() > 1e-2 { push (hand.power, hand.position) } }
  if !samples.is_empty() {
      let target = weighted_focal(&samples, FOCAL_CENTER_WEIGHT);
      focal.0 = ease_focal(focal.0, target, dt, settings.explode_focal_smoothing);
      // world -> Dots UV (inverse of the mouse world mapping; +y up already matches bottom-left UV)
      params.i_mouse = [(focal.0.x + w * 0.5) / w, (focal.0.y + h * 0.5) / h];
  } else if let Some(cursor) = pointer.cursor {
      // Existing instant mouse path (UV), and keep the focal in sync so a later
      // hand grab eases from the cursor, not from a stale point.
      params.i_mouse = [cursor.x / w, (h - cursor.y) / h];
      focal.0 = Vec2::new(cursor.x - w * 0.5, (h - cursor.y) - h * 0.5);
  }
  // (no cursor, no hand: leave i_mouse unchanged — existing guard)
  ```
  Mutually exclusive (hand overrides mouse), exactly like Line. Keep allocation-free: gather into a fixed-capacity stack buffer (`MAX_ATTRACTORS` cap) or a `[(f32, Vec2); 8]` with a count — NOT a `Vec`. Confirm `weighted_focal` accepts a slice.
- Chain `update_dots_post_params.after(update_dots_hand_attractors)` so the focal reads the current frame's hand power (the exponential ease makes a 1-frame lag harmless, but ordering is cheap and correct).

**Settings additions** (`section = "Visual"`):
```rust
shrink_factor: f32            // Dev, default 0.98, min 0.9, max 1.0, step 0.005, label "Explode shrink"
explode_focal_smoothing: f32 // Dev, default 0.25, min 0.0, max 1.0, step 0.05, label "Hand split smoothing", unit "s"
```

- [ ] **Step 1: Add the two `DotsSettings` fields** + serde defaults; extend the defaults test.
- [ ] **Step 2: Run** `cargo test -p wc-sketches --lib dots::settings` → PASS.
- [ ] **Step 3: Write failing `post_params.rs` tests:** (a) with no hands and a cursor, `i_mouse` follows the existing v4 UV formula (unchanged) AND `shrink_factor` now comes from settings; (b) with a grabbing hand (power > 1e-2) at a known world position and no cursor, after several steps `i_mouse` eases toward that hand's UV (hand overrides mouse); (c) no hand + no cursor leaves `i_mouse` unchanged. Update `setup_world` to insert `DotsExplodeFocal` and (for hand cases) spawn `TrackedHand + DotsHandAttractor`, and to set `shrink_factor` in `DotsSettings`.
- [ ] **Step 4: Run** `cargo test -p wc-sketches --lib dots::systems::post_params` → FAIL.
- [ ] **Step 5: Implement** `DotsExplodeFocal`, the focal-eased `i_mouse`, the settings `shrink_factor`, the OnEnter/OnExit wiring, and the system ordering.
- [ ] **Step 6: Run** `cargo test -p wc-sketches --lib dots` → PASS.
- [ ] **Step 7: Commit** — `feat(dots): hands rotate the hue-split via eased focal (Line-parity smoothing)`.

---

## Task group F4 — Attract-mode dimming

### Task 8: Promote `attract_color_params` to a shared `ParticleMaterial` helper

**Files:**
- Modify: `crates/wc-sketches/src/particles/material.rs` (add `pub fn attract_color_params(fade: f32, strength: f32, brightness: f32) -> Vec4`)
- Modify: `crates/wc-sketches/src/line/screensaver/mod.rs` (delete the private `attract_color_params`, call `ParticleMaterial::attract_color_params`)
- Test: `material.rs` tests (move/duplicate the existing `attract_color_params` unit tests; assert the Active no-op `y == 0` case)

**Why:** the helper is currently private to Line; Dots needs the same math. Promoting it (and pointing Line at it) is the shared-foundation move and keeps Line byte-identical. The function is a tiny pure mapping:
```rust
/// Pack the attract-mode color uniform: `x` = velocity-tint strength (scaled by
/// fade), `y` = brightness lift (so the calm field's whites clear the AgX white
/// knee), `z`/`w` = 0. `fade == 0` (Active) yields `Vec4::ZERO` — a bit-exact
/// render no-op. Mirrors the prior `line::screensaver` private helper exactly.
pub fn attract_color_params(fade_alpha: f32, strength: f32, brightness: f32) -> Vec4 {
    let fade = fade_alpha.clamp(0.0, 1.0);
    let lift = fade * (brightness.max(1.0) - 1.0);
    Vec4::new(fade * strength.max(0.0), lift, 0.0, 0.0)
}
```

- [ ] **Step 1: Move the unit tests** for `attract_color_params` into `material.rs` (verbatim behavior: `fade=0 → ZERO`; `brightness=2.2, fade=1 → y=1.2`; `strength` scales `x`). Run → FAIL (function not yet on `ParticleMaterial`).
- [ ] **Step 2: Implement** the public helper on `ParticleMaterial`; delete Line's private copy; update Line's call site.
- [ ] **Step 3: Run** `cargo test -p wc-sketches --lib particles::material` and `cargo test -p wc-sketches --lib line::screensaver` → PASS.
- [ ] **Step 4: Commit** — `refactor(particles): share attract_color_params for Line + Dots`.

---

### Task 9: Drive the Dots attract brightness lift

**Files:**
- Modify: `crates/wc-sketches/src/dots/screensaver.rs` (add `drive_dots_attract_color`; register it under BOTH `in_screensaver(AppState::Dots)` and `sketch_active(AppState::Dots)`)
- Modify: `crates/wc-sketches/src/dots/settings.rs` (add `attract_brightness`, `attract_color_strength`, `section = "Screensaver"`)
- Test: `screensaver.rs` tests (the lift writes `attract_color.y` from fade × (brightness−1); Active fade → ZERO)

**Why:** Dots spawns the shared `ParticleMaterial` with `attract_color = ZERO` and never drives it, so the fraction-killed calm field never clears the AgX white knee and reads dim. Line drives `attract_color.y` (default ×2.2). Wire the same for Dots.

**Design.** Mirror `line::screensaver::drive_attract_color` (`mod.rs:205-231`):
- Read `Res<ScreensaverFade>`, `Res<DotsSettings>`, `Query<&MeshMaterial2d<ParticleMaterial>, With<DotsRoot>>`, `ResMut<Assets<ParticleMaterial>>`, and a `Local<Vec4>` change-gate.
- Compute `let target = ParticleMaterial::attract_color_params(fade.alpha(), settings.attract_color_strength, settings.attract_brightness);` and write it to the material only when it changed (the `Local<Vec4>` gate avoids touching `Assets` every frame).
- `DotsRoot` carries the `MeshMaterial2d<ParticleMaterial>` (see `dots/systems/spawn.rs:201-208`), so `With<DotsRoot>` matches exactly like Line's `With<LineRoot>`.
- Register under both gates so the lift ramps in on fade-in (Screensaver) and back out after wake (the fade-out completes in Active):
  ```rust
  app.add_systems(Update, drive_dots_attract_color.run_if(in_screensaver(AppState::Dots)));
  app.add_systems(Update, drive_dots_attract_color.run_if(sketch_active(AppState::Dots)));
  ```

**Settings additions** (`section = "Screensaver"`, both Dev):
```rust
attract_brightness: f32     // default 2.2, min 1.0, max 4.0, step 0.1, label "Attract brightness"
attract_color_strength: f32 // default 0.0, min 0.0, max 1.0, step 0.05, label "Attract color strength"
```

- [ ] **Step 1: Add the two `DotsSettings` fields** + serde defaults; extend the defaults test.
- [ ] **Step 2: Run** `cargo test -p wc-sketches --lib dots::settings` → PASS.
- [ ] **Step 3: Write the failing test** — `drive_dots_attract_color` with a `ScreensaverFade` at full alpha and a `DotsRoot` material entity sets `attract_color.y ≈ 1.2` (brightness 2.2); at fade 0 (Active) it sets `Vec4::ZERO`. Spawn a `DotsRoot` + `MeshMaterial2d<ParticleMaterial>` and an `Assets<ParticleMaterial>` in the test world.
- [ ] **Step 4: Run** `cargo test -p wc-sketches --lib dots::screensaver` → FAIL.
- [ ] **Step 5: Implement** the driver + dual registration.
- [ ] **Step 6: Run** `cargo test -p wc-sketches --lib dots` → PASS.
- [ ] **Step 7: Commit** — `fix(dots): attract-mode brightness lift (AgX white-knee), matches Line`.

---

## Final verification (whole-branch, before finishing)

Run the full CI gate suite at the branch tip:
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features --workspace -- -D warnings`
- `cargo nextest run --workspace --all-features` + `cargo test --doc --workspace`
- `cargo doc --no-deps --workspace --document-private-items`
- `cargo deny check` and `cargo xtask check-secrets`
- `cargo build -p waveconductor` (naga validates the changed WGSL)

Then update `crates/wc-sketches/src/dots/PARITY.md`: fold these five fixes into the closed-items list, and replace the now-resolved carry-forwards (stationary_constant soak-watch drift; mouse/hand power disparity; LFO-rate gap downgraded to "fallback if breath insufficient"). Record the remaining operator pre-tag checklist (ear-tune the Audio knobs; eye-tune `fabric_tension`/`hand_power_scale`; verify hand hue-split + brightness on hardware; seed capture baselines; 8 h soak).

## Operator-deferred (post-merge, needs Madison's hardware/ears/eyes)

- Tune the Audio panel by ear; tune `fabric_tension`, `hand_power_scale`, `explode_focal_smoothing`, `attract_brightness` by eye in `cargo rund`.
- Confirm on Leap/MediaPipe: grab rotates the hue-split, grab power now feels mouse-calibrated, wireframe + brightness hold.
- Re-seed the `dots-synthetic` / `dots-screensaver` capture baselines after visual sign-off.
- Re-run the 8 h `dots_soak` before any release tag (the immutable-home change alters long-idle behavior — verify the grid holds).
