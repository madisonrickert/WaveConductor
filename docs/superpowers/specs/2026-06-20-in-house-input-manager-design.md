# In-house keyboard action manager (replace leafwing-input-manager)

- **Date:** 2026-06-20
- **Status:** Approved (design); pending implementation plan
- **Crate:** `wc-core`
- **Module:** `crates/wc-core/src/lifecycle/`

## Motivation

`leafwing-input-manager` is the hard blocker for upgrading Bevy 0.18 → 0.19: its
newest release (0.20.0) targets `bevy ^0.18`, with no 0.19-compatible release
(not even a pre-release), and it lags Bevy by one version on every release, so it
will re-block every future Bevy upgrade. It is core input infrastructure (woven
through `lifecycle/{nav,idle,actions}`, `audio/nav`, `settings/panel_dev`) and
cannot simply be dropped.

We use only a thin slice of leafwing: keyboard button actions with edge
detection (`just_pressed`/`just_released`) and two Shift chords. No analog axes,
no gamepad, no clash resolution (we have zero overlapping bindings), no
per-entity input maps, no input-source abstraction. A measured `cargo bloat`
shows leafwing is ~350 KiB of `.text` (0.7%), so this is an upgrade-unblock and
dependency-removal change, not a binary-size change.

Replacing it with a small in-house manager removes the recurring Bevy-upgrade
blocker permanently, drops a dependency (and its `syn 1` / proc-macro subtree),
and is well-scoped given how little of leafwing we use.

## Goals

- Remove `leafwing-input-manager` from `wc-core` and the workspace.
- Preserve current runtime behavior exactly (same keys, same precedence, same
  egui-keyboard-capture suppression, same idle screensaver-skip semantics).
- Keep a single central binding table that a future rebind UI can mutate.
- Keep consumer and test churn minimal.

## Non-goals (deferred)

- **Rebinding UI and persistence.** Bindings stay a hardcoded default in a
  mutable resource (matching today's `actions.rs` "future settings UI can
  rebind" comment). No settings-panel UI, no serialization. A later change can
  add both without reworking this design.
- Analog/axis actions, gamepad, mouse-button actions, clash strategies. Not used.

## Decisions

- **Approach: action messages (Bevy "B" idiom).** A `PreUpdate` producer reads
  `Res<ButtonInput<KeyCode>>` plus the binding table and emits `ActionInput`
  messages; consumers read them via `MessageReader<ActionInput>`. Chosen over a
  drop-in `ActionState`-resource shim and over per-action run-conditions:
  messages fit all four consumers uniformly, including `nav`'s multi-action
  branch and `idle`'s per-frame state machine (run-conditions cannot express
  either), and they let us centralize the keyboard-capture gate at the producer.
- **Edge-only model.** Every production action query is an edge
  (`just_pressed`/`just_released`); the only continuous `.pressed(...)`/
  `get_pressed()` calls in `src` are on raw Bevy `ButtonInput` (mouse buttons in
  `input/systems.rs`, keyboard activity in `idle.rs`), not on the action layer.
  So messages carrying `Pressed`/`Released` edges are sufficient — no companion
  "currently-held actions" set is needed.

## Design

### Module layout (`crates/wc-core/src/lifecycle/`)

- `actions.rs` (exists): keep the `WaveConductorAction` enum. Drop the
  `Actionlike` and `Reflect` derives (keep `Clone, Copy, Hash, PartialEq, Eq,
  Debug`). The binding table moves to `action_map.rs`.
- `action_map.rs` (new, ~150 lines): `Modifier`, `Binding`, `InputBindings`
  resource + `default_bindings()`, `ActionInput` message + `ActionPhase`, and the
  `emit_action_input` producer system.

### Types

```rust
/// Keyboard modifier; matches either left or right physical key.
pub enum Modifier { Shift, Control, Alt }
impl Modifier {
    fn held(self, keys: &ButtonInput<KeyCode>) -> bool { /* both L/R variants */ }
}

/// One physical binding for an action.
pub enum Binding {
    Key(KeyCode),
    Chord { modifier: Modifier, key: KeyCode },
}

/// Central, mutable binding table. Future rebind UI mutates this resource.
#[derive(Resource)]
pub struct InputBindings(pub Vec<(WaveConductorAction, Binding)>);

pub fn default_bindings() -> InputBindings { /* ports today's default_input_map */ }

#[derive(Message, Clone, Copy, Debug)]
pub struct ActionInput { pub action: WaveConductorAction, pub phase: ActionPhase }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionPhase { Pressed, Released }
```

`default_bindings()` reproduces the current map exactly:

- Digit1..Digit5 → SelectLine/Flame/Dots/Cymatics/Waves
- KeyZ, ArrowLeft → NavigatePrev (OR); KeyX, ArrowRight → NavigateNext (OR)
- Escape → NavigateHome; KeyV → ToggleVolume; F11 → ToggleFullscreen
- Chord{Shift, KeyD} → ToggleDevPanel; Chord{Shift, KeyS} → StartScreensaver

### Producer system

`emit_action_input(keys: Res<ButtonInput<KeyCode>>, bindings: Res<InputBindings>,
writer: MessageWriter<ActionInput>)`, scheduled in `PreUpdate` (where leafwing's
plugin updated its `ActionState`, so `Update` consumers see fresh edges).

- For each `(action, binding)`:
  - Pressed edge: `Binding::Key(k)` → `keys.just_pressed(k)`;
    `Binding::Chord { modifier, key }` → `keys.just_pressed(key) && modifier.held(keys)`.
  - Released edge: `keys.just_released(key)` (releasing the main key ends the
    chord; a modifier release while the main key stays held is intentionally not
    a Released edge — keeps it simple and matches the edge-only consumers).
- De-duplicate per `(action, phase)` within the frame (an action with two
  bindings could fire both the same frame). Use a fixed-size stack bitset over
  the 12 actions × 2 phases — **no per-frame allocation** (AGENTS.md hot-path
  rule; this system runs every frame for the life of the session).
- Gated with `.run_if(crate::settings::input_capture::egui_not_capturing_keyboard)`.
  This is the single, central keyboard-capture gate, replacing the per-consumer
  `.run_if` scattering.

### Plugin wiring (`lifecycle/mod.rs`)

Replace:

```rust
.add_plugins(InputManagerPlugin::<actions::WaveConductorAction>::default())
.insert_resource(actions::default_input_map())
.init_resource::<ActionState<actions::WaveConductorAction>>()
```

with:

```rust
.add_message::<action_map::ActionInput>()
.insert_resource(action_map::default_bindings())
.add_systems(PreUpdate,
    action_map::emit_action_input
        .run_if(crate::settings::input_capture::egui_not_capturing_keyboard))
```

Remove `use leafwing_input_manager::prelude::*;` from `mod.rs` and update the
data-flow doc comment (step 2) to reference the in-house producer.

### Consumer migration

All consumers switch from `Res<ActionState<…>>` to `MessageReader<ActionInput>`
and drop whatever egui-keyboard-capture gating they currently apply — the
explicit `.run_if(egui_not_capturing_keyboard)` on `nav`, the
post-`update_egui_input_capture` ordering on `panel_dev`, and any equivalent on
the volume toggle (the implementation should locate each). The producer now
carries the single gate; consumers read every frame, and `MessageReader` cursors
ensure each message is consumed once per reader.

- `lifecycle/nav.rs::handle_navigation_actions` — collect this frame's `Pressed`
  actions; apply the first that maps to a transition, preserving the current
  else-if precedence (SelectLine, SelectFlame, …, NavigateHome, NavigateNext,
  NavigatePrev). Handle `ToggleFullscreen` on its `Pressed`. Drop the run_if on
  this system in `mod.rs`.
- `audio/nav.rs::handle_volume_toggle` — fire on `ActionInput { ToggleVolume,
  Pressed }`.
- `settings/panel_dev.rs::handle_dev_panel_toggle` — fire on `ActionInput {
  ToggleDevPanel, Pressed }`. (Its previous chaining-after-`update_egui_input_capture`
  ordering nuance is subsumed by the producer gate.)
- `lifecycle/idle.rs::skip_to_screensaver` — `just_pressed(StartScreensaver)`
  becomes "an `ActionInput { StartScreensaver, Pressed }` arrived this frame."
  The system keeps running every frame (only the producer is gated, so the armed
  state never freezes — preserves the original intent of keeping this system off
  the egui run_if). `keyboard_active` keeps reading raw `ButtonInput<KeyCode>`
  (`get_pressed`/`get_just_released`) unchanged.

### Capture-gating equivalence

Moving the gate to the producer is behavior-equivalent to today: `nav`,
`audio` volume, and dev-panel toggle were each egui-gated, and `StartScreensaver`
self-gated on `!capturing`. With the producer gated, no action messages are
emitted while an egui text field has keyboard focus, so all consumers are
suppressed uniformly. The documented 1–2 frame egui-mirror lag tolerance (see
`settings/input_capture.rs`) is preserved: the producer in `PreUpdate` reads the
same mirrored `EguiKeyboardCaptured` state the consumers read today.

### Testing

- `tests/lifecycle.rs`, `tests/audio.rs`: keep the physical `KeyCode::press(world)`
  / `release(world)` simulation. Only the test-app setup changes (swap the
  leafwing plugin registration for `add_message::<ActionInput>()` +
  `insert_resource(default_bindings())` + the `PreUpdate` producer). The
  `press_key` helper is unchanged.
- `tests/settings_plugin.rs`: already drives `ShiftLeft.press` + `KeyD.press`
  physically; update only the resource/registration setup.
- `settings/panel_dev.rs` unit test: replace direct `ActionState::press()`
  mutation with driving `ButtonInput<KeyCode>` (Shift + D) through the producer,
  matching the `settings_plugin.rs` pattern.
- New `action_map.rs` unit tests: Key binding emits `Pressed` on `just_pressed`;
  Chord emits only when the modifier is held; an action with two bindings
  de-dups to one message per phase per frame; `Released` phase on key release;
  `default_bindings()` binds all 12 actions (port the existing
  `default_input_map_contains_all_actions` test).

### Removal and dependency impact

- Delete `leafwing-input-manager` from `crates/wc-core/Cargo.toml` and
  `workspace.dependencies` in the root `Cargo.toml`. (`wc-sketches`'s unused
  declaration was already removed.)
- Remove all leafwing API usage: `InputManagerPlugin`, `InputMap`, `ActionState`,
  `Actionlike`, `ButtonlikeChord`, `ModifierKey`.
- This clears the `wc-core` half of the Bevy-0.19 blocker. The remaining blocker
  is `bevy_console` (no 0.19 release); dropping or replacing it is tracked
  separately.

## Suggested implementation order

1. Add `action_map.rs` (types, `default_bindings()`, producer, unit tests).
2. Strip the `Actionlike`/`Reflect` derives from `actions.rs`; move/retire
   `default_input_map()` in favor of `default_bindings()`.
3. Rewire `lifecycle/mod.rs`.
4. Port consumers one at a time: `nav`, `audio/nav`, `panel_dev`, `idle`.
5. Update the four test files.
6. Remove the dependency from both manifests.
7. Verify: `cargo fmt --check`, `cargo clippy --all-targets --all-features
   --workspace -D warnings`, `cargo nextest run --workspace --all-features`
   (+ `cargo test --doc`), `cargo xtask check-secrets`.

## Risks

- **nav precedence:** the message loop must preserve the current first-match
  precedence; a naive "last message wins" would change which sketch is selected
  when two select keys land in one frame (rare, but covered by a test).
- **Message double-buffering vs. skipped frames:** consumers must read every
  frame (ungated). Because the gate lives at the producer, no consumer is
  skipped, so each edge is consumed exactly once — no stale replay. (Guard
  against accidentally leaving a `.run_if` on a consumer.)
- **idle armed state:** `skip_to_screensaver` must remain ungated and run every
  frame; only the producer is gated. Verified by the existing idle tests.
