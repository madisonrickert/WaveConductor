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
        app.add_systems(
            PreUpdate,
            emit_action_input
                .run_if(egui_not_capturing_keyboard)
                .after(bevy::input::InputSystems),
        );
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
}
