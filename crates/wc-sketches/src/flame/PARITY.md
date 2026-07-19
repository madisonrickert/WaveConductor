# Flame — Parity Record

Internal name: `Flame`. Display name: "You-niverse" (v4's `HomePage.tsx` tile label).

**Parity target:** Perceptual — the IFS attractor must be recognizably v4 for a
given name; chaotic fine detail may drift so long as the silhouette and motion
read the same.

**Reference media:**
- `.worktrees/v4/src/sketches/flame/index.tsx` — name → hash → PRNG → branch set,
  `computeDepth`, per-frame `cX`/`cY` oscillation, warp, camera.
- `.worktrees/v4/src/sketches/flame/{superPoint,transforms,branch}.ts` — the IFS
  variation/affine kernel and the recursive tree evaluation.
- `.worktrees/v4/src/sketches/flame/audio.ts` — the generative voice (noise +
  DC-through-resonant-lowpass "osc" + chord + compressor) and its mapping curves.
- `.worktrees/v4/src/sketches/flame/flamePoints.{vert,frag}.*` — additive point
  sprite rendering (`gl_PointSize`, disc texture, fog, opacity falloff).
- `.worktrees/v4/src/sketches/flame/screenshots/flame.png` — silhouette reference
  (copied to `assets/sketches/flame/screenshot.png`).

## Implementation log (plan `2026-07-02-flame-sketch-port.md`, F1–F17)

Ported in 17 tasks across 9 stages. Name → fractal generation is reproduced with
`f64` arithmetic so a given name yields the same fractal and timbre as v4
(JavaScript numbers are `f64`; `*`, `+`, `%` are IEEE-exact in both languages),
golden-tested against values generated from the v4 source.

- **F1** (`3ec5379c`) — name→branch core: `string_hash` over UTF-16 code units,
  the `f64` PRNG, `randomBranch*` draw-for-draw, the CPU kernel mirror
  (`apply_variation_cpu`/`apply_branch_cpu`), `NameAudioConfig`/`FlameSpec`.
  Golden-tested against v4 (`docs/superpowers/plans/assets/2026-07-02-flame-goldens.mjs`).
- **F2** (`c6aa1284`) — branch-major level layout (`LevelLayout`/`LevelSpan`),
  depth formula, parent-index arithmetic, ember-prefix math.
- **F3** (`e1160d36`) — `FlamePlugin` scaffold, `FlameSettings` (22 fields),
  manifest tile, clear-color swap to `#10101f`.
- **F4** (`1ebacb5d`) — re-entry: `Flame` back in `SKETCH_ORDER`, `from_name`,
  `SelectFlame`/`Digit2`, `next`/`prev` rewiring, tripwire guard tests (the
  audit-T5 reversal).
- **F5** (`8002f611`) — GPU POD types (`FlameNodeGpu`/`FlameBranchGpu`/
  `FlameSimParamsGpu`/`FlameLevelParamsGpu`) with size asserts + branch encoding.
- **F6** (`bd55322f`) — level-parallel IFS compute pipeline + `simulate.wgsl`:
  one dispatch per tree level over a persistent node buffer, 256-byte
  dynamic-offset per-level uniform, bind-group caching.
- **F7** (`3a5ca14f`) — main-world drivers: spawn/seed, name-change rebuild,
  per-frame `cX`/warp writer, idle freeze, the `ExtractSchedule` removal companion.
- **F8** (`24c12f93`) — additive billboard renderer + `render.wgsl`: in-material
  perspective projection + billboarding, `specialize` blend override, disc sprite.
- **F9** (`5903da3e`) — CPU orbit camera: autorotate, drag, wheel zoom, fling
  momentum, the two `mat4` uniform builders.
- **F10** (`a089a767`) — hand grab-and-fling from `TrackedHand`, grab→warp
  routing through `FlameGrabState.warp_px`, idle veto, bone overlay.
- **F11** (`418b824b`) — `SettingKind::TextList`: a `Vec<String>` list-editor
  setting kind (macro + panel widget), backing the editable carousel list.
- **F12** (`df5ed53a`) — centered name-input overlay + ghost seed label +
  debounced carousel admission (half-typed keystrokes never enter the list).
- **F13** (`30859150`) — `FlameSynth` fundsp voice with the in-synth v4 mapping
  curves, plumbed over the lock-free ring as `Copy` `AudioCommand`s.
- **F14** (`1dcfd8e4`) — envelope-primary audio coupling on two per-frame
  scalars (morph-energy = analytic `|dcX/dt|` + warp speed; camera distance).
- **F15** (`235f442d`) — `FlameScreensaverPlugin`: name carousel (adopt-on-wake),
  ember-decay complexity + brightness lift on `ScreensaverFade`, ghost label.
- **F16** (`2f155352`) — capture scenarios (`flame-synthetic`, `flame-warp`,
  `flame-screensaver`) + the `WC_DEBUG_FORCE_FLAME_WARP` deterministic toggle.
- **F17** — this record, roadmap correction, spec amendment, operator checklist.

## Approved deviations from v4

1. **GPU level-parallel IFS replaces v4's CPU recursive tree.** The fractal is
   parallel *within* each tree level, so the ~100k nodes live in one persistent
   level-ordered storage buffer and the sim runs one compute dispatch per level
   (5–16/frame) computing `state[i] = lerp(state[i], apply_branch(state[parent(i)]),
   0.8)`. This eliminates v4's per-frame CPU walk and the ~3.2 MB/frame upload.
   Visual math is identical; the `f64` name→branch generation is golden-tested.

2. **In-material 3D projection replaces a `Camera3d`.** The app runs exactly one
   window camera (the global HDR `Camera2d`), and `apply_render_profile`, the
   hand-mesh composite, and the shared-MSAA contract all assume it. So the orbit
   camera is a CPU `FlameCamera` resource passed to a custom `Material2d` as two
   `mat4` uniforms; the vertex shader does the perspective projection +
   billboarding + fake-DoF sizing in-material, drawing into the existing
   `Camera2d`. Bloom, tonemapping, `apply_render_profile`, and the hand-mesh
   overlay all work unchanged. (Supersedes the spec's original `Camera3d` design;
   spec amended.)

3. **Envelope/DSP audio replaces v4's per-frame stat-driven audio.** v4 sampled
   node state every 307th point per frame to drive the voice; v5 drives it from
   two CPU-side scalars instead: analytic `|dcX/dt|` + warp speed approximate the
   morph velocity, and camera distance sets a proximity gain. The chord register
   is a hash-derived pseudo-density *base* per name (replacing v4's box-counting),
   and a `tanh` shaper replaces the `DynamicsCompressor`. **Fallback seam:** if
   ear-tuning rejects the pseudo-density register feel, a one-shot ~2k-point CPU
   evaluation + box-count at name-change time (only) can replace it without
   touching the steady-state path.

   **2026-07-03 amendment — screen-Y pitch restored.** The pseudo-density register
   is now only the *base*; `chord_degree` is driven per frame from the
   pointer/hand vertical position (`flame_pitch_degree` in `audio_coupling.rs`,
   ±`PITCH_Y_RANGE` diatonic degrees around the base), restoring the mouse/hand-Y
   pitch responsiveness Madison flagged as missing vs v4. This maps screen-Y
   straight to the degree rather than reintroducing v4's per-frame box-count, so
   the soak path stays allocation-free. Ear-tune surface: `PITCH_Y_RANGE`.

4. **Instanced additive billboards + `specialize` blend override replace
   `THREE.Points`.** `gl_PointSize` sprites become instanced camera-facing quads;
   `AdditiveBlending` becomes a `Material2d::specialize` `BlendState` override to
   `(One, One)`. Blend algebra note: `(One, One)` with the in-shader alpha
   multiply is equivalent to v4's `(SrcAlpha, One)` given the `pow`/alpha applied
   in the fragment shader.

5. **v4 float quirks ported faithfully** (`f64` reproduces them): for names longer
   than ~4 chars `string_hash` returns a double that is a multiple of 1024, so
   `cY = -2.5` for most names; `hash2 % 2 == 0` for all but tiny names, so
   `is_major` is almost always true; the 0 Hz DC-square-through-resonant-lowpass
   "osc" voice is preserved.

6. **Single-branch chain not ported** (unreachable in v4). v4's name input
   substitutes the default name for empty input, so `numBranches` is always 2–8
   and v4's 1,000-node sequential chain is dead code. `normalize_name` mirrors the
   trim-or-default rule and the builder asserts `branch_count >= 2`.

7. **Not ported** (unreachable or subsumed): v4's `quality="low"` static mobile
   fallback; OrbitControls damping (fling momentum + autorotate cover the feel);
   the silent 0 Hz triangle osc; the chord minor/fifth biases (never driven in
   v4); v4's `tonemapping`/`colorspace` fragment includes (the HDR camera +
   `RenderProfile` own this now).

8. **v5-only additions:** a name carousel over the editable `carousel_names`
   `TextList` (adopt-on-wake), ember decay (a level-prefix complexity knob),
   attract-mode brightness lift, and debounced name admission. v4 Flame had no
   attract mode.

## Post-parity additions

Features added after the parity port, by design (not v4 behavior):

1. **Grab-space camera gestures (2026-07-03, reworked 2026-07-19)** — original
   spec `docs/superpowers/specs/2026-07-03-flame-two-hand-camera-gestures-design.md`;
   the mapping was rebuilt after live-party feedback (guests expected grab to
   drag the scene, not orbit it — Google Earth VR grip-nav prior art). One
   grabbed hand PANS (content follows the hand ~1:1; release throws a decaying
   pan fling). Two grabbed hands zoom (spread ratio vs. the engage anchor,
   hands apart = zoom in), rotate (twist of the inter-hand line yaws the
   azimuth), and pan (midpoint drag) about the grip (`FlameCamera::target`,
   clamped to radius 2.0); releasing keeps a modest yaw momentum from the
   twist but never a pan fling or zoom momentum. Grab engage/release uses
   0.7/0.45 hysteresis so a wavering grip doesn't stutter between modes.
   Hands never tilt (polar stays mouse-only). Dev knobs:
   `two_hand_zoom_gamma`, `two_hand_rotate_gain`, `hand_pan_sensitivity`.
2. **Settle-to-home camera ease (2026-07-03, same spec)** — whenever no hand
   grabs and no mouse drags, `polar`/`distance`/`target` ease exponentially back
   to the v4 start pose (`camera_return_seconds` Dev knob, default 8 s time
   constant) so no gesture can strand the kiosk in an ugly pose — Dots'
   `fabric_tension` philosophy applied to the camera. `azimuth` is exempt
   (autorotate owns it).

## Verdict

**Status:** PENDING — operator sign-off required before tagging.

F1–F17 delivered the full implementation: name→fractal core with v4 `f64` golden
parity (F1–F2), lifecycle scaffold + re-entry (F3–F4), level-parallel GPU compute
(F5–F7), additive in-material renderer (F8), orbit camera + hand grab-fling
(F9–F10), the `TextList` setting kind + name overlay (F11–F12), `FlameSynth` +
envelope coupling (F13–F14), the carousel/ember attract performer (F15), and
capture scenarios (F16). Automated tests cover the name→branch math, level/parent
arithmetic, ember prefix, GPU POD layout, audio config, settings serde, and
lifecycle plumbing. Visual, ear, and hardware verification are operator-deferred
per the checklist below.

## Operator pre-tag checklist

Complete each item on the deployment machine (`cargo rund`) before tagging Flame.

### Visual (`cargo rund`)

- [ ] **Renders and orbits**: `WAVECONDUCTOR_START_SKETCH=flame cargo rund` — the
  default-name ("who are you?") fractal blooms in from center over ~1–2 s, then
  continuously morphs (the `cX` oscillation) as an additive glowing point cloud
  against `#10101f`, DoF-blurred at the edges, autorotating.
- [ ] **Silhouette parity**: compare against v4's `screenshots/flame.png` — the
  attractor character should be recognizable for the same name.
- [ ] **Warp**: move the mouse over the fractal and confirm it deforms (`cDx`/`cDy`).
- [ ] **Re-entry**: `Digit2` from Home lands on Flame; `X`/`Z` cycle through all
  four sketches including Flame; Home → Flame → Home leaves no resource leak.

### Audio (`cargo rund` — ear tuning; joins the pending Line hand-audio item)

- [ ] **Voice character** matches v4 `audio.ts` by ear. Tune live in the
  **Flame → Audio** panel (flip ADVANCED for Dev knobs): `morph_energy_scale`,
  `chord_energy_scale`, `synth_volume_scale`, attack/release, filter feel per name.
- [ ] **Morph swell**: still mouse → near-silence with a quiet self-breathing
  fractal; wiggle over the shape → the noise/osc voice swells; the slow `cX`
  oscillation audibly breathes on its own at its turning points.
- [ ] **Proximity**: zoom the camera in (scroll to reduce distance) → the voice
  gets louder/closer via the camera-distance gain.
- [ ] **Pseudo-density register verdict**: judge whether the hash-derived chord
  register reads right per name; if not, adopt the documented box-count fallback
  seam (deviation #3).

### Hardware hand-tracking (`cargo rund` + Leap/MediaPipe)

- [ ] **One-hand grab-pan feel**: grab (engages above 0.7 grab strength) → the
  scene follows the hand ~1:1 with the amber (`#ffb84d`) bone overlay visible;
  a wavering grip (0.45–0.7) neither engages nor drops; release with motion →
  the pan flings/coasts and decays smoothly like a thrown map. One hand must
  never orbit. Tune `hand_pan_sensitivity` if the feel is off.
- [ ] **Two-hand zoom/rotate/pan feel**: grab with both hands → spreading them
  apart zooms in, squeezing together zooms out, twisting the pair rotates the
  scene with the hands, moving both together pans the view (content follows
  the hands); dropping to one hand resumes pan without a jump; releasing both
  keeps only a modest spin from the twist — never a pan fling. Tune
  `two_hand_zoom_gamma`, `two_hand_rotate_gain`, and `hand_pan_sensitivity` in
  **Flame** settings (flip ADVANCED to see Dev knobs) if the feel is off.
- [ ] **Settle-to-home**: zoom/pan/tilt the camera into an extreme pose, let go,
  and confirm it drifts back to the start framing over ~10–20 s (azimuth keeps
  autorotating). Tune `camera_return_seconds` if the drift reads too eager or
  too lazy.

### Attract mode (`cargo rund`)

- [ ] **Ember + carousel**: `WC_DEBUG_FORCE_SCREENSAVER=1 WAVECONDUCTOR_START_SKETCH=flame
  cargo rund` — the fractal fades to the ember (visibly thinner cloud, brightness
  lifted through the AgX white knee), still slowly rotating/morphing, audio fades
  with it, and a dim ghost label names the current seed at bottom-center. Drop
  `carousel_period_secs` to ~20 in the dock and watch a carousel advance; confirm
  the transition in and out of screensaver is smooth and that waking adopts the
  shown seed.
- [ ] **Review `BUILTIN_SEEDS`** word choices with Madison.

### Capture baselines (deployment machine + display required)

- [ ] Seed the four baselines after Reading the frames:
  `cargo xtask capture flame-synthetic --update-baselines`,
  `cargo xtask capture flame-warp --update-baselines`,
  `cargo xtask capture flame-screensaver --update-baselines`,
  `cargo xtask capture flame-camera-pose --update-baselines`. Confirm the
  synthetic frames show the recognizable fractal, the warp frames show it
  visibly displaced, the screensaver frames show the thinner lifted ember
  with the ghost label, and the camera-pose frames show a static (no
  autorotate drift) zoomed-in, off-center view — the pinned pose that
  regression-guards the two-hand gesture camera's target-aware view matrix.
  (Headless/backgrounded capture renders all-black frames — a known
  environment trap, not a bug; run on a real display.) Commit the PNGs.

### AgX / tonemap eye-tune

- [ ] Tune against v4 side-by-side: `gamma` (0.545 start), `master_brightness`,
  the bloom trio, `attract_brightness`, fog range.

### 8-hour soak

- [ ] Run the app under representative load (hand tracking + audio active, sketch
  cycling incl. Flame) for ~8 hours on the deployment hardware; watch RSS, GPU
  memory, and FPS for drift or a thermal stall. Overdraw check zoomed-in at the
  50 px point-size clamp.
