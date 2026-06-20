# In-house Keyboard Action Manager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `leafwing-input-manager` with a small message-based keyboard action manager in `wc-core`, unblocking the Bevy 0.18 → 0.19 upgrade.

**Architecture:** A `PreUpdate` producer system reads `Res<ButtonInput<KeyCode>>` plus a central `InputBindings` table and emits one `ActionInput { action, phase }` message per action edge each frame. Consumers read them via `MessageReader<ActionInput>`. The egui-keyboard-capture gate moves from each consumer to the single producer. Migration is incremental: the new producer runs alongside leafwing while consumers move over one at a time, then leafwing is removed.

**Tech Stack:** Rust, Bevy 0.18 (Message/`MessageReader`/`MessageWriter` API), `ButtonInput<KeyCode>`.

**Spec:** `docs/superpowers/specs/2026-06-20-in-house-input-manager-design.md`

## Global Constraints

- **Edition / MSRV:** Rust 1.96, edition 2021 (workspace-pinned).
- **Lints are hard errors:** `cargo clippy --all-targets --all-features --workspace -- -D warnings` must pass. No `unwrap`/`expect`/`panic` in non-test code; no `as` numeric casts where `From`/`TryFrom` work; `unsafe_code` is denied.
- **Docs required:** `///` on every public item, `//!` on the new module root. Never delete comments during refactors — update stale ones.
- **Hot-path rule:** the producer runs every frame for the session's life — no per-frame heap allocation.
- **One concept per file**, files under ~300 lines, tests in a `#[cfg(test)] mod tests` footer.
- **Behavior must stay equivalent:** same keys, same nav precedence, same egui-capture suppression, same idle screensaver-skip semantics.
- **Verify gates** (run before claiming done): `cargo fmt --all -- --check`, the clippy line above, `cargo nextest run --workspace --all-features` (+ `cargo test --doc --workspace`), `cargo doc --no-deps --workspace --document-private-items`, `cargo deny check`, `cargo xtask check-secrets`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/wc-core/src/lifecycle/action_map.rs` (create) | `Modifier`, `Binding`, `InputBindings`, `default_bindings()`, `ActionInput`, `ActionPhase`, `emit_action_input` producer + tests |
| `crates/wc-core/src/lifecycle/actions.rs` (modify) | Keep `WaveConductorAction` enum; add `ALL`; (Task 7) drop `Actionlike`/`Reflect`, remove `default_input_map` |
| `crates/wc-core/src/lifecycle/mod.rs` (modify) | Declare `action_map`; wire producer; (Task 7) remove leafwing |
| `crates/wc-core/src/lifecycle/nav.rs` (modify) | Read `MessageReader<ActionInput>` |
| `crates/wc-core/src/audio/nav.rs` (modify) | Read `MessageReader<ActionInput>` |
| `crates/wc-core/src/audio/mod.rs` (modify) | Drop volume-toggle `.run_if` |
| `crates/wc-core/src/settings/panel_dev.rs` (modify) | Read `MessageReader<ActionInput>`; migrate unit test |
| `crates/wc-core/src/settings/mod.rs` (modify) | Drop dev-panel `.run_if` |
| `crates/wc-core/src/lifecycle/idle.rs` (modify) | Read `ActionInput` for `StartScreensaver` |
| `crates/wc-core/tests/{lifecycle,audio,settings_plugin}.rs` (modify) | Swap leafwing injection for `common::input` helpers / producer plumbing |
| `crates/wc-core/Cargo.toml`, `Cargo.toml` (modify) | Remove `leafwing-input-manager` |

---

### Task 1: Add `action_map` types, bindings, and pure edge helpers

**Files:**
- Create: `crates/wc-core/src/lifecycle/action_map.rs`
- Modify: `crates/wc-core/src/lifecycle/actions.rs` (add `WaveConductorAction::ALL`)
- Modify: `crates/wc-core/src/lifecycle/mod.rs` (add `pub mod action_map;`)

**Interfaces:**
- Consumes: `super::actions::WaveConductorAction` (existing enum).
- Produces: `Modifier`, `Binding`, `InputBindings(pub Vec<(WaveConductorAction, Binding)>)`, `default_bindings() -> InputBindings`, `ActionInput { action: WaveConductorAction, phase: ActionPhase }`, `ActionPhase::{Pressed, Released}`, and `WaveConductorAction::ALL: [WaveConductorAction; 12]`.

- [ ] **Step 1: Add `ALL` to the action enum**

In `crates/wc-core/src/lifecycle/actions.rs`, add an impl block directly after the `WaveConductorAction` enum:

```rust
impl WaveConductorAction {
    /// Every action variant, in nav-precedence order. Used by the action-input
    /// producer to iterate actions without per-frame allocation.
    pub const ALL: [WaveConductorAction; 12] = [
        WaveConductorAction::SelectLine,
        WaveConductorAction::SelectFlame,
        WaveConductorAction::SelectDots,
        WaveConductorAction::SelectCymatics,
        WaveConductorAction::SelectWaves,
        WaveConductorAction::NavigateHome,
        WaveConductorAction::NavigateNext,
        WaveConductorAction::NavigatePrev,
        WaveConductorAction::ToggleVolume,
        WaveConductorAction::ToggleDevPanel,
        WaveConductorAction::ToggleFullscreen,
        WaveConductorAction::StartScreensaver,
    ];
}
```

- [ ] **Step 2: Declare the module**

In `crates/wc-core/src/lifecycle/mod.rs`, add to the module declarations block (near `pub mod actions;`):

```rust
pub mod action_map;
```

- [ ] **Step 3: Write `action_map.rs` with types, helpers, bindings, and failing tests**

Create `crates/wc-core/src/lifecycle/action_map.rs`:

```rust
//! In-house keyboard action mapping.
//!
//! Replaces `leafwing-input-manager` for WaveConductor's needs: keyboard
//! button actions with edge detection plus simple modifier chords. A
//! `PreUpdate` producer ([`emit_action_input`]) reads `ButtonInput<KeyCode>`
//! and the [`InputBindings`] table and emits one [`ActionInput`] message per
//! action edge each frame; consumers read them via `MessageReader<ActionInput>`.
//!
//! Rebinding is intentionally out of scope: [`InputBindings`] is a mutable
//! resource seeded by [`default_bindings`] so a future settings UI can edit it,
//! but no UI or persistence exists yet.

use bevy::prelude::*;

use super::actions::WaveConductorAction;

/// A keyboard modifier that matches either its left or right physical key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    /// `ShiftLeft` or `ShiftRight`.
    Shift,
    /// `ControlLeft` or `ControlRight`.
    Control,
    /// `AltLeft` or `AltRight`.
    Alt,
}

impl Modifier {
    /// The two physical [`KeyCode`]s this modifier matches.
    fn keys(self) -> [KeyCode; 2] {
        match self {
            Modifier::Shift => [KeyCode::ShiftLeft, KeyCode::ShiftRight],
            Modifier::Control => [KeyCode::ControlLeft, KeyCode::ControlRight],
            Modifier::Alt => [KeyCode::AltLeft, KeyCode::AltRight],
        }
    }

    /// True when either physical variant is currently held.
    fn held(self, keys: &ButtonInput<KeyCode>) -> bool {
        self.keys().iter().any(|k| keys.pressed(*k))
    }
}

/// One physical binding for a [`WaveConductorAction`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// A single key; fires on that key's press/release edge.
    Key(KeyCode),
    /// A modifier + key chord; the press edge fires when `key` is pressed while
    /// `modifier` is held, and the release edge is `key`'s release.
    Chord {
        /// Modifier that must be held for the chord's press edge.
        modifier: Modifier,
        /// Main key whose edge drives the chord.
        key: KeyCode,
    },
}

impl Binding {
    /// True if this binding produced a *pressed* edge this frame.
    fn pressed(self, keys: &ButtonInput<KeyCode>) -> bool {
        match self {
            Binding::Key(k) => keys.just_pressed(k),
            Binding::Chord { modifier, key } => keys.just_pressed(key) && modifier.held(keys),
        }
    }

    /// True if this binding produced a *released* edge this frame. Releasing the
    /// main key ends the chord; releasing only the modifier is not an edge.
    fn released(self, keys: &ButtonInput<KeyCode>) -> bool {
        let key = match self {
            Binding::Key(k) | Binding::Chord { key: k, .. } => k,
        };
        keys.just_released(key)
    }
}

/// The central, mutable action → key binding table.
///
/// Seeded by [`default_bindings`]. A future rebind UI mutates this resource;
/// there is no persistence yet.
#[derive(Resource, Debug, Clone)]
pub struct InputBindings(pub Vec<(WaveConductorAction, Binding)>);

/// The default keyboard bindings (ports v4's hotkey table).
#[must_use]
pub fn default_bindings() -> InputBindings {
    use Binding::{Chord, Key};
    use WaveConductorAction as A;
    InputBindings(vec![
        (A::SelectLine, Key(KeyCode::Digit1)),
        (A::SelectFlame, Key(KeyCode::Digit2)),
        (A::SelectDots, Key(KeyCode::Digit3)),
        (A::SelectCymatics, Key(KeyCode::Digit4)),
        (A::SelectWaves, Key(KeyCode::Digit5)),
        (A::NavigatePrev, Key(KeyCode::KeyZ)),
        (A::NavigatePrev, Key(KeyCode::ArrowLeft)),
        (A::NavigateNext, Key(KeyCode::KeyX)),
        (A::NavigateNext, Key(KeyCode::ArrowRight)),
        (A::NavigateHome, Key(KeyCode::Escape)),
        (A::ToggleVolume, Key(KeyCode::KeyV)),
        (A::ToggleFullscreen, Key(KeyCode::F11)),
        (
            A::ToggleDevPanel,
            Chord { modifier: Modifier::Shift, key: KeyCode::KeyD },
        ),
        (
            A::StartScreensaver,
            Chord { modifier: Modifier::Shift, key: KeyCode::KeyS },
        ),
    ])
}

/// Edge phase carried by an [`ActionInput`] message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionPhase {
    /// The action transitioned to pressed this frame (`just_pressed`).
    Pressed,
    /// The action transitioned to released this frame (`just_released`).
    Released,
}

/// One action edge emitted by [`emit_action_input`] for the current frame.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionInput {
    /// Which action fired.
    pub action: WaveConductorAction,
    /// Whether it was pressed or released this frame.
    pub phase: ActionPhase,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_with(pressed: &[KeyCode]) -> ButtonInput<KeyCode> {
        let mut keys = ButtonInput::<KeyCode>::default();
        for k in pressed {
            keys.press(*k);
        }
        keys
    }

    #[test]
    fn modifier_matches_either_side() {
        assert!(Modifier::Shift.held(&keys_with(&[KeyCode::ShiftLeft])));
        assert!(Modifier::Shift.held(&keys_with(&[KeyCode::ShiftRight])));
        assert!(!Modifier::Shift.held(&keys_with(&[KeyCode::KeyA])));
    }

    #[test]
    fn key_binding_pressed_on_just_pressed() {
        let keys = keys_with(&[KeyCode::Digit1]);
        assert!(Binding::Key(KeyCode::Digit1).pressed(&keys));
        assert!(!Binding::Key(KeyCode::Digit2).pressed(&keys));
    }

    #[test]
    fn chord_requires_modifier_held() {
        let chord = Binding::Chord { modifier: Modifier::Shift, key: KeyCode::KeyD };
        assert!(chord.pressed(&keys_with(&[KeyCode::ShiftLeft, KeyCode::KeyD])));
        assert!(!chord.pressed(&keys_with(&[KeyCode::KeyD])));
    }

    #[test]
    fn binding_released_on_key_release() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::Digit1);
        keys.release(KeyCode::Digit1);
        assert!(Binding::Key(KeyCode::Digit1).released(&keys));
    }

    #[test]
    fn default_bindings_cover_all_actions() {
        let bindings = default_bindings();
        for action in WaveConductorAction::ALL {
            assert!(
                bindings.0.iter().any(|(a, _)| *a == action),
                "no binding for {action:?}",
            );
        }
    }
}
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p wc-core --lib action_map::`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/lifecycle/action_map.rs crates/wc-core/src/lifecycle/actions.rs crates/wc-core/src/lifecycle/mod.rs
git commit -m "feat(input): add in-house action_map types and bindings"
```

---

### Task 2: Add the producer system and wire it alongside leafwing

**Files:**
- Modify: `crates/wc-core/src/lifecycle/action_map.rs` (add `emit_action_input` + test)
- Modify: `crates/wc-core/src/lifecycle/mod.rs:41-48` (register message, resource, system)

**Interfaces:**
- Consumes: `Res<ButtonInput<KeyCode>>`, `Res<InputBindings>`, `WaveConductorAction::ALL`, `Binding::{pressed,released}`.
- Produces: `pub fn emit_action_input(...)` (a Bevy system) and `Messages<ActionInput>` available to consumers.

- [ ] **Step 1: Write the producer test (App-level)**

Append inside the `tests` module in `action_map.rs`:

```rust
    use crate::settings::input_capture::egui_not_capturing_keyboard;

    #[derive(Resource, Default)]
    struct Captured(Vec<ActionInput>);

    fn capture(mut reader: MessageReader<'_, '_, ActionInput>, mut out: ResMut<'_, Captured>) {
        for ev in reader.read() {
            out.0.push(*ev);
        }
    }

    fn producer_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_plugins(bevy::input::InputPlugin);
        app.add_message::<ActionInput>();
        app.insert_resource(default_bindings());
        app.init_resource::<Captured>();
        app.add_systems(PreUpdate, emit_action_input.run_if(egui_not_capturing_keyboard));
        app.add_systems(Update, capture);
        app
    }

    fn send_key(app: &mut App, key: KeyCode, state: bevy::input::ButtonState) {
        app.world_mut().write_message(bevy::input::keyboard::KeyboardInput {
            key_code: key,
            logical_key: bevy::input::keyboard::Key::Unidentified(
                bevy::input::keyboard::NativeKey::Unidentified,
            ),
            state,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
    }

    #[test]
    fn producer_emits_pressed_for_bound_key() {
        let mut app = producer_app();
        app.update(); // settle
        app.world_mut().resource_mut::<Captured>().0.clear();
        send_key(&mut app, KeyCode::Digit1, bevy::input::ButtonState::Pressed);
        app.update();
        let got = &app.world().resource::<Captured>().0;
        assert_eq!(
            got.as_slice(),
            &[ActionInput { action: WaveConductorAction::SelectLine, phase: ActionPhase::Pressed }],
        );
    }

    #[test]
    fn producer_dedups_multi_binding_action() {
        let mut app = producer_app();
        app.update();
        app.world_mut().resource_mut::<Captured>().0.clear();
        // Z and ArrowLeft both map to NavigatePrev; pressing both the same frame
        // must still yield exactly one Pressed message.
        send_key(&mut app, KeyCode::KeyZ, bevy::input::ButtonState::Pressed);
        send_key(&mut app, KeyCode::ArrowLeft, bevy::input::ButtonState::Pressed);
        app.update();
        let prev: Vec<_> = app
            .world()
            .resource::<Captured>()
            .0
            .iter()
            .filter(|e| e.action == WaveConductorAction::NavigatePrev && e.phase == ActionPhase::Pressed)
            .collect();
        assert_eq!(prev.len(), 1, "multi-binding action must de-dup to one message");
    }

    #[test]
    fn producer_chord_needs_modifier() {
        let mut app = producer_app();
        app.update();
        app.world_mut().resource_mut::<Captured>().0.clear();
        // KeyD alone (no Shift) must NOT emit ToggleDevPanel.
        send_key(&mut app, KeyCode::KeyD, bevy::input::ButtonState::Pressed);
        app.update();
        assert!(
            !app.world().resource::<Captured>().0.iter().any(|e| e.action
                == WaveConductorAction::ToggleDevPanel),
            "chord must not fire without its modifier",
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p wc-core --lib action_map::tests::producer`
Expected: FAIL — `cannot find function emit_action_input`.

- [ ] **Step 3: Implement `emit_action_input`**

Add to `action_map.rs` (after the `ActionInput` struct, before `#[cfg(test)]`):

```rust
/// `PreUpdate` producer: reads `ButtonInput<KeyCode>` and [`InputBindings`] and
/// emits one [`ActionInput`] per action edge this frame.
///
/// Iterates [`WaveConductorAction::ALL`] and OR-s each action's bindings, so an
/// action with multiple bindings (e.g. `Z` and `ArrowLeft` → `NavigatePrev`)
/// yields at most one `Pressed` and one `Released` message per frame, with no
/// per-frame heap allocation.
///
/// Registered with `.run_if(egui_not_capturing_keyboard)` so no action fires
/// while an egui text field holds keyboard focus.
pub fn emit_action_input(
    keys: Res<'_, ButtonInput<KeyCode>>,
    bindings: Res<'_, InputBindings>,
    mut writer: MessageWriter<'_, ActionInput>,
) {
    for action in WaveConductorAction::ALL {
        let mut pressed = false;
        let mut released = false;
        for (bound_action, binding) in &bindings.0 {
            if *bound_action != action {
                continue;
            }
            pressed |= binding.pressed(&keys);
            released |= binding.released(&keys);
        }
        if pressed {
            writer.write(ActionInput { action, phase: ActionPhase::Pressed });
        }
        if released {
            writer.write(ActionInput { action, phase: ActionPhase::Released });
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p wc-core --lib action_map::`
Expected: PASS (8 tests).

- [ ] **Step 5: Wire the producer into `LifecyclePlugin` (alongside leafwing)**

In `crates/wc-core/src/lifecycle/mod.rs`, in `LifecyclePlugin::build`, immediately after the existing leafwing block (the `init_resource::<ActionState<…>>()` line), add:

```rust
            // In-house action input (replaces leafwing; see action_map). Runs
            // alongside leafwing during migration.
            .add_message::<action_map::ActionInput>()
            .insert_resource(action_map::default_bindings())
            .add_systems(
                PreUpdate,
                action_map::emit_action_input
                    .run_if(crate::settings::input_capture::egui_not_capturing_keyboard),
            )
```

- [ ] **Step 6: Verify the workspace compiles and tests pass**

Run: `cargo test -p wc-core --lib action_map:: && cargo check -p wc-core`
Expected: PASS / clean.

- [ ] **Step 7: Commit**

```bash
git add crates/wc-core/src/lifecycle/action_map.rs crates/wc-core/src/lifecycle/mod.rs
git commit -m "feat(input): add emit_action_input producer, wire alongside leafwing"
```

---

### Task 3: Migrate `nav.rs` to `MessageReader<ActionInput>`

**Files:**
- Modify: `crates/wc-core/src/lifecycle/nav.rs`
- Modify: `crates/wc-core/src/lifecycle/mod.rs:68-69` (drop nav `.run_if`)
- Modify: `crates/wc-core/tests/lifecycle.rs` (replace local `press_key`, drop leafwing import)

**Interfaces:**
- Consumes: `action_map::{ActionInput, ActionPhase}`, `WaveConductorAction`.
- Produces: `handle_navigation_actions` now reads `MessageReader<ActionInput>`; no signature consumed by others.

- [ ] **Step 1: Rewrite `handle_navigation_actions`**

Replace the body of `crates/wc-core/src/lifecycle/nav.rs` (keep the module doc comment at top; replace the `use` lines and the function):

```rust
use bevy::prelude::*;
use bevy::window::WindowMode;

use super::action_map::{ActionInput, ActionPhase};
use super::actions::WaveConductorAction;
use super::state::AppState;

/// Reads `MessageReader<ActionInput>` and translates `Pressed` edges into
/// navigation transitions and window-level effects (fullscreen toggle).
///
/// Drains all of this frame's `Pressed` edges first, then applies a single
/// transition by fixed precedence (sketch-select, Home, Next, Prev) so two
/// select keys landing the same frame resolve deterministically — matching the
/// previous else-if ordering.
pub fn handle_navigation_actions(
    mut actions: MessageReader<'_, '_, ActionInput>,
    current: Res<'_, State<AppState>>,
    mut next: ResMut<'_, NextState<AppState>>,
    mut windows: Query<'_, '_, &mut Window>,
) {
    use WaveConductorAction as A;

    let mut pressed_select: Option<AppState> = None;
    let mut home = false;
    let mut go_next = false;
    let mut go_prev = false;
    let mut fullscreen = false;

    for input in actions.read() {
        if input.phase != ActionPhase::Pressed {
            continue;
        }
        match input.action {
            A::SelectLine => pressed_select = pressed_select.or(Some(AppState::Line)),
            A::SelectFlame => pressed_select = pressed_select.or(Some(AppState::Flame)),
            A::SelectDots => pressed_select = pressed_select.or(Some(AppState::Dots)),
            A::SelectCymatics => pressed_select = pressed_select.or(Some(AppState::Cymatics)),
            A::SelectWaves => pressed_select = pressed_select.or(Some(AppState::Waves)),
            A::NavigateHome => home = true,
            A::NavigateNext => go_next = true,
            A::NavigatePrev => go_prev = true,
            A::ToggleFullscreen => fullscreen = true,
            // ToggleVolume → audio::nav; ToggleDevPanel → settings::panel_dev;
            // StartScreensaver → idle::skip_to_screensaver.
            _ => {}
        }
    }

    let transition_to = pressed_select
        .or_else(|| home.then_some(AppState::Home))
        .or_else(|| go_next.then(|| current.get().next_sketch()))
        .or_else(|| go_prev.then(|| current.get().prev_sketch()));

    if let Some(target) = transition_to {
        if *current.get() != target {
            tracing::info!(?target, "navigate");
            next.set(target);
        }
    }

    if fullscreen {
        for mut window in &mut windows {
            window.mode = match window.mode {
                WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                _ => WindowMode::Windowed,
            };
            tracing::info!(mode = ?window.mode, "toggle fullscreen");
        }
    }
}
```

- [ ] **Step 2: Drop the nav `.run_if` in `mod.rs`**

In `crates/wc-core/src/lifecycle/mod.rs`, change the `nav::handle_navigation_actions` registration so it no longer has the `.run_if(...)` (the producer now carries the gate). Replace:

```rust
                    nav::handle_navigation_actions
                        .run_if(crate::settings::input_capture::egui_not_capturing_keyboard),
```

with:

```rust
                    nav::handle_navigation_actions,
```

(Keep the surrounding comment, updating it to note the gate now lives on the producer.)

- [ ] **Step 3: Update the lifecycle test harness**

In `crates/wc-core/tests/lifecycle.rs`: remove `use leafwing_input_manager::prelude::*;` (line 19) and replace the local `press_key` helper (lines 22-34) with a delegate to the existing leafwing-free helper:

```rust
use common::input::press_key as send_press;
use common::input::release_key as send_release;

/// Inject a physical key press, run one update tick (so the PreUpdate producer
/// emits the action and the Update consumers act), then release.
fn press_key(app: &mut App, key: KeyCode) {
    send_press(app, key);
    app.update();
    send_release(app, key);
}
```

Add `mod common;`'s `input` submodule if not already imported (the file already has `mod common;`; ensure `common::input` is accessible — `crates/wc-core/tests/common/input.rs` already exists).

- [ ] **Step 4: Run the lifecycle tests**

Run: `cargo test -p wc-core --test lifecycle`
Expected: PASS (nav/select/home/next/prev tests green via the new producer).

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/lifecycle/nav.rs crates/wc-core/src/lifecycle/mod.rs crates/wc-core/tests/lifecycle.rs
git commit -m "refactor(input): nav reads ActionInput messages"
```

---

### Task 4: Migrate `audio/nav.rs`

**Files:**
- Modify: `crates/wc-core/src/audio/nav.rs`
- Modify: `crates/wc-core/src/audio/mod.rs:94-95` (drop volume `.run_if`)
- Modify: `crates/wc-core/tests/audio.rs` (replace local `press_key`)

**Interfaces:**
- Consumes: `crate::lifecycle::action_map::{ActionInput, ActionPhase}`, `WaveConductorAction`.

- [ ] **Step 1: Rewrite `handle_volume_toggle`**

In `crates/wc-core/src/audio/nav.rs`, replace the `use leafwing_input_manager::prelude::*;` import with the action_map import, and rewrite the function to read messages:

```rust
use super::command::AudioCommand;
use super::ring::AudioCommandSender;
use super::state::AudioState;
use crate::lifecycle::action_map::{ActionInput, ActionPhase};
use crate::lifecycle::actions::WaveConductorAction;

/// `Update` system that translates `ToggleVolume` presses into `SetMuted`.
///
/// Uses `NonSendMut<AudioCommandSender>` because `rtrb::Producer` is not
/// `Sync`; see `ring` module docs.
pub fn handle_volume_toggle(
    mut actions: MessageReader<'_, '_, ActionInput>,
    state: Res<'_, AudioState>,
    mut sender: NonSendMut<'_, AudioCommandSender>,
) {
    let toggled = actions.read().any(|a| {
        a.action == WaveConductorAction::ToggleVolume && a.phase == ActionPhase::Pressed
    });
    if toggled {
        let new_muted = !state.muted;
        if let Err(_dropped) = sender.push(AudioCommand::SetMuted(new_muted)) {
            tracing::warn!("audio command ring full; dropping SetMuted command");
        } else {
            tracing::info!(new_muted, "toggle volume → SetMuted");
        }
    }
}
```

Keep the existing `use bevy::ecs::system::NonSendMut;` and `use bevy::prelude::*;` lines.

- [ ] **Step 2: Drop the volume `.run_if` in `audio/mod.rs`**

In `crates/wc-core/src/audio/mod.rs`, change the `nav::handle_volume_toggle` registration to drop `.run_if(crate::settings::input_capture::egui_not_capturing_keyboard)` (the producer carries the gate). Update the adjacent comment to say so.

- [ ] **Step 3: Update the audio test harness**

In `crates/wc-core/tests/audio.rs`, replace the local `press_key` (lines 106-118) with a producer-aware version using the shared helper:

```rust
fn press_key(app: &mut App, key: KeyCode) {
    common::input::press_key(app, key);
    app.update();
    common::input::release_key(app, key);
    app.update();
}
```

Ensure the test file has `mod common;` (add it if absent — sibling integration tests use it). The test app already adds `LifecyclePlugin` (which now registers the producer), so no other setup change is needed.

- [ ] **Step 4: Run the audio tests**

Run: `cargo test -p wc-core --test audio`
Expected: PASS (`toggle_volume_action_pushes_set_muted_command` green).

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/audio/nav.rs crates/wc-core/src/audio/mod.rs crates/wc-core/tests/audio.rs
git commit -m "refactor(input): volume toggle reads ActionInput messages"
```

---

### Task 5: Migrate `settings/panel_dev.rs` (handler + unit test) and `tests/settings_plugin.rs`

**Files:**
- Modify: `crates/wc-core/src/settings/panel_dev.rs` (handler + `mod tests`)
- Modify: `crates/wc-core/src/settings/mod.rs:85-86` (drop dev-panel `.run_if`)
- Modify: `crates/wc-core/tests/settings_plugin.rs` (replace leafwing plumbing with producer plumbing)

**Interfaces:**
- Consumes: `crate::lifecycle::action_map::{ActionInput, ActionPhase}`, `WaveConductorAction`.

- [ ] **Step 1: Rewrite `handle_dev_panel_toggle`**

In `crates/wc-core/src/settings/panel_dev.rs`, replace `use leafwing_input_manager::prelude::ActionState;` with the action_map import and rewrite the handler:

```rust
use crate::lifecycle::action_map::{ActionInput, ActionPhase};
use crate::lifecycle::actions::WaveConductorAction;

/// Listens for `ToggleDevPanel` presses and flips [`DevPanelVisible`].
/// Scheduled in `Update` by `SettingsPlugin`.
pub fn handle_dev_panel_toggle(
    mut actions: MessageReader<'_, '_, ActionInput>,
    mut visible: ResMut<'_, DevPanelVisible>,
) {
    let toggled = actions.read().any(|a| {
        a.action == WaveConductorAction::ToggleDevPanel && a.phase == ActionPhase::Pressed
    });
    if toggled {
        visible.0 = !visible.0;
        tracing::debug!(visible = visible.0, "dev panel toggled");
    }
}
```

- [ ] **Step 2: Drop the dev-panel `.run_if` in `settings/mod.rs`**

In `crates/wc-core/src/settings/mod.rs`, change the `panel_dev::handle_dev_panel_toggle` registration to drop `.run_if(input_capture::egui_not_capturing_keyboard)`. Update the adjacent comment to note the producer carries the gate.

- [ ] **Step 3: Rewrite the panel_dev unit test**

Replace the `tests` module in `panel_dev.rs` (lines 438-486) with a message-driven version:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_message::<ActionInput>();
        app.init_resource::<DevPanelVisible>();
        app.add_systems(Update, handle_dev_panel_toggle);
        app
    }

    fn fire_toggle(app: &mut App) {
        app.world_mut().write_message(ActionInput {
            action: WaveConductorAction::ToggleDevPanel,
            phase: ActionPhase::Pressed,
        });
        app.update();
    }

    #[test]
    fn toggle_flips_visibility() {
        let mut app = make_app();
        fire_toggle(&mut app);
        assert!(
            app.world().resource::<DevPanelVisible>().0,
            "first press should make panel visible",
        );
        fire_toggle(&mut app);
        assert!(
            !app.world().resource::<DevPanelVisible>().0,
            "second press should hide panel",
        );
    }
}
```

- [ ] **Step 4: Rewrite the settings_plugin test setup**

In `crates/wc-core/tests/settings_plugin.rs`, replace the leafwing plumbing (the `InputManagerPlugin` add, `default_input_map` insert, and `ActionState` init at lines 38-45) with producer plumbing:

```rust
    app.add_message::<wc_core::lifecycle::action_map::ActionInput>();
    app.insert_resource(wc_core::lifecycle::action_map::default_bindings());
    app.add_systems(
        bevy::app::PreUpdate,
        wc_core::lifecycle::action_map::emit_action_input.run_if(
            wc_core::settings::input_capture::egui_not_capturing_keyboard,
        ),
    );
```

The existing physical injection at lines 161-162 (`ShiftLeft.press` + `KeyD.press`) must become the leafwing-free helpers; replace those two lines with:

```rust
    common::input::press_key(&mut app, bevy::prelude::KeyCode::ShiftLeft);
    common::input::press_key(&mut app, bevy::prelude::KeyCode::KeyD);
```

Add `mod common;` to the test file if absent. Remove any now-unused `leafwing_input_manager` imports.

- [ ] **Step 5: Run the affected tests**

Run: `cargo test -p wc-core --lib panel_dev:: && cargo test -p wc-core --test settings_plugin`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/wc-core/src/settings/panel_dev.rs crates/wc-core/src/settings/mod.rs crates/wc-core/tests/settings_plugin.rs
git commit -m "refactor(input): dev-panel toggle reads ActionInput messages"
```

---

### Task 6: Migrate `idle.rs::skip_to_screensaver`

**Files:**
- Modify: `crates/wc-core/src/lifecycle/idle.rs:205-236`

**Interfaces:**
- Consumes: `super::action_map::{ActionInput, ActionPhase}`, `WaveConductorAction`.

- [ ] **Step 1: Rewrite the action read in `skip_to_screensaver`**

In `crates/wc-core/src/lifecycle/idle.rs`, change the system's `actions` parameter from the leafwing `Res<ActionState<…>>` to a `MessageReader<ActionInput>`, and compute `just_pressed` from the messages. Replace the parameter:

```rust
    actions: Res<
        '_,
        leafwing_input_manager::prelude::ActionState<super::actions::WaveConductorAction>,
    >,
```

with:

```rust
    mut actions: MessageReader<'_, '_, super::action_map::ActionInput>,
```

and replace the `just_pressed` computation:

```rust
    let just_pressed =
        !capturing && actions.just_pressed(&super::actions::WaveConductorAction::StartScreensaver);
```

with:

```rust
    // The producer is egui-capture-gated, so `StartScreensaver` never arrives
    // while a text field has focus; the `!capturing` guard is kept for harnesses
    // that run this system without the producer's gate.
    let start_pressed = actions.read().any(|a| {
        a.action == super::action_map::ActionInput::screensaver_action()
            && a.phase == super::action_map::ActionPhase::Pressed
    });
    let just_pressed = !capturing && start_pressed;
```

To avoid importing the full path inline, instead use a direct comparison — replace the `start_pressed` closure body with:

```rust
    use super::action_map::ActionPhase;
    use super::actions::WaveConductorAction;
    let start_pressed = actions
        .read()
        .any(|a| a.action == WaveConductorAction::StartScreensaver && a.phase == ActionPhase::Pressed);
    let just_pressed = !capturing && start_pressed;
```

(Do not add a `screensaver_action()` helper — the direct comparison above is the intended form; the earlier snippet is illustrative only.)

`keyboard_active` (reading raw `keys: Res<ButtonInput<KeyCode>>`) is unchanged. The `keys` parameter stays.

- [ ] **Step 2: Run the idle tests**

Run: `cargo test -p wc-core --test lifecycle && cargo test -p wc-core --lib idle::`
Expected: PASS (screensaver-skip behavior preserved).

- [ ] **Step 3: Commit**

```bash
git add crates/wc-core/src/lifecycle/idle.rs
git commit -m "refactor(input): screensaver-skip reads ActionInput messages"
```

---

### Task 7: Remove leafwing wiring and the action-enum derives

**Files:**
- Modify: `crates/wc-core/src/lifecycle/mod.rs` (remove leafwing plugin/resource/init + import; update doc)
- Modify: `crates/wc-core/src/lifecycle/actions.rs` (drop `Actionlike`/`Reflect` derives + `#[reflect]`; remove `default_input_map`)

**Interfaces:**
- Produces: `WaveConductorAction` now derives only `Clone, Copy, Hash, PartialEq, Eq, Debug`.

- [ ] **Step 1: Remove leafwing from `LifecyclePlugin`**

In `crates/wc-core/src/lifecycle/mod.rs`: delete `use leafwing_input_manager::prelude::*;`, and delete the three leafwing lines in `build`:

```rust
            .add_plugins(InputManagerPlugin::<actions::WaveConductorAction>::default())
            .insert_resource(actions::default_input_map())
            .init_resource::<ActionState<actions::WaveConductorAction>>()
```

Update the data-flow doc comment (step 2 of the `//!` block) to read: "2. `action_map::emit_action_input` reads `ButtonInput<KeyCode>` and emits `ActionInput` messages."

- [ ] **Step 2: Strip derives and remove `default_input_map` in `actions.rs`**

In `crates/wc-core/src/lifecycle/actions.rs`:
- Change the enum derive from `#[derive(Actionlike, Reflect, Clone, Copy, Hash, PartialEq, Eq, Debug)]` + `#[reflect(Hash, PartialEq)]` to:

```rust
#[derive(Clone, Copy, Hash, PartialEq, Eq, Debug)]
```

- Remove `use leafwing_input_manager::prelude::*;` (or the `Actionlike`/`InputMap` imports).
- Delete the `default_input_map()` function and its `#[cfg(test)]` test `default_input_map_contains_all_actions` (coverage now lives in `action_map.rs`'s `default_bindings_cover_all_actions`).

- [ ] **Step 3: Verify the crate compiles with no leafwing references in code**

Run:
```bash
rg -n 'leafwing' crates/wc-core/src && echo "LEAFWING STILL REFERENCED" || echo "clean"
cargo check -p wc-core
```
Expected: "clean", then a clean `cargo check`.

- [ ] **Step 4: Run the full wc-core test suite**

Run: `cargo nextest run -p wc-core --all-features` (or `cargo test -p wc-core --all-features`)
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/src/lifecycle/mod.rs crates/wc-core/src/lifecycle/actions.rs
git commit -m "refactor(input): remove leafwing wiring and action-enum derives"
```

---

### Task 8: Remove the dependency and run full verification

**Files:**
- Modify: `crates/wc-core/Cargo.toml` (remove `leafwing-input-manager`)
- Modify: `Cargo.toml` (remove `leafwing-input-manager` from `[workspace.dependencies]`)

**Interfaces:** none.

- [ ] **Step 1: Remove the dependency declarations**

- In `crates/wc-core/Cargo.toml`, delete the line `leafwing-input-manager = { workspace = true }`.
- In the root `Cargo.toml`, delete the `leafwing-input-manager = "0.20"` line and its comment under `[workspace.dependencies]`.

- [ ] **Step 2: Confirm nothing references leafwing anywhere**

Run:
```bash
rg -n 'leafwing' crates Cargo.toml && echo "STILL REFERENCED" || echo "clean"
```
Expected: "clean".

- [ ] **Step 3: Regenerate the lockfile and confirm removal**

Run:
```bash
cargo update -p leafwing-input-manager 2>&1 | head -3 || true
cargo check --workspace
grep -c 'name = "leafwing-input-manager"' Cargo.lock
```
Expected: `cargo check` clean; grep prints `0`.

- [ ] **Step 4: Run the full CI gate suite**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo nextest run --workspace --all-features
cargo test --doc --workspace
cargo doc --no-deps --workspace --document-private-items
cargo deny check
cargo xtask check-secrets
```
Expected: all PASS (the ~29 pre-existing doc-link warnings are non-fatal).

- [ ] **Step 5: Commit**

```bash
git add crates/wc-core/Cargo.toml Cargo.toml Cargo.lock
git commit -m "build(deps): drop leafwing-input-manager (replaced by in-house action_map)"
```

---

## Self-Review

**Spec coverage:**
- Module layout (`action_map.rs` + `actions.rs` enum) → Tasks 1, 7. ✓
- `Modifier`/`Binding`/`InputBindings`/`default_bindings` → Task 1. ✓
- `ActionInput`/`ActionPhase`/producer + zero-alloc de-dup + capture gate → Tasks 1, 2. ✓
- Plugin wiring swap → Tasks 2 (add) + 7 (remove leafwing). ✓
- Consumer migration (nav, audio, panel_dev, idle) + drop per-consumer run_if → Tasks 3-6. ✓
- Capture-gating moved to producer → Tasks 2-6 (each consumer drops its run_if; producer gated). ✓
- Testing (physical injection retained, panel_dev unit test rewritten, new producer tests) → Tasks 1-6. ✓
- Removal + dep impact + Bevy-0.19 unblock → Tasks 7, 8. ✓

**Placeholder scan:** No TODO/TBD. The one illustrative snippet in Task 6 Step 1 is explicitly flagged as illustrative, with the intended final form given immediately after. ✓

**Type consistency:** `ActionInput { action, phase }`, `ActionPhase::{Pressed, Released}`, `Binding::{Key, Chord{modifier,key}}`, `Modifier::{Shift,Control,Alt}`, `InputBindings(pub Vec<(WaveConductorAction, Binding)>)`, `WaveConductorAction::ALL`, `emit_action_input`, `default_bindings` — used identically across Tasks 1-8. `MessageReader<'_, '_, ActionInput>` / `MessageWriter<'_, ActionInput>` lifetimes consistent. ✓

## Notes for the implementer

- `common::input::{press_key, release_key, tap_key}` already exist in `crates/wc-core/tests/common/input.rs` and inject physical `KeyboardInput` without leafwing. Reuse them; do not reintroduce a leafwing-based injector. Update the stale leafwing-referencing doc comment on `tap_key` if you touch it.
- Build stays green at every task: the producer coexists with leafwing (Tasks 2-6); leafwing is only removed once every consumer has migrated (Tasks 7-8).
- If a consumer accidentally keeps a `.run_if(egui_not_capturing_keyboard)`, the message buffer can be read a frame late after capture toggles — keep the gate solely on the producer.
