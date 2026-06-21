//! Tracks whether the egui UI is capturing input (pointer or keyboard) this
//! frame.
//!
//! Sketches consume pointer input directly via Bevy's
//! `ButtonInput<MouseButton>` and `Touches` resources, and the app's global
//! hotkeys run through the in-house `action_map` (`ActionInput` messages
//! emitted by `crate::lifecycle::action_map::emit_action_input`). Without
//! coordination, input that lands inside an egui panel ALSO fires those
//! handlers:
//!
//! - a click inside the Settings panel both moves the egui slider AND fires
//!   the sketch's click handler (for Line, tweaking a slider also spawned an
//!   attractor at the slider's screen position);
//! - typing a digit into a dev-panel text field both edits the field AND
//!   fires the `SelectLine`/`SelectFlame`/… sketch-switch hotkeys, yanking
//!   the operator out of the sketch she is tuning.
//!
//! This module exposes two thin boolean wrappers over `bevy_egui`'s
//! `EguiWantsInput` state:
//!
//! - [`EguiPointerCaptured`] — `wants_any_pointer_input()`. Sketches read it
//!   and suppress their press-edge handling when `true`.
//! - [`EguiKeyboardCaptured`] — `wants_any_keyboard_input()` (a focused text
//!   field, or an open popup). The single `PreUpdate` producer,
//!   `crate::lifecycle::action_map::emit_action_input`, carries
//!   `.run_if(egui_not_capturing_keyboard)` — when egui owns the keyboard,
//!   no `ActionInput` messages are emitted and every keyboard-action consumer
//!   (nav, volume toggle, dev-panel toggle, screensaver-skip) is suppressed
//!   uniformly without any per-consumer gating.
//!
//! [`update_egui_input_capture`] copies both values out of `bevy_egui`'s
//! resource each frame. It uses `Option<Res<EguiWantsInput>>` so test
//! harnesses running without `EguiPlugin` (e.g., the `MinimalPlugins`-based
//! `core_plugin_builds_without_panicking` test) don't crash on a missing
//! resource — when `bevy_egui` isn't loaded, both wrappers stay `false` and
//! every gated system runs normally.
//!
//! ## Scheduling
//!
//! `bevy_egui` populates `EguiWantsInput` in `PostUpdate` (via
//! `EguiPostUpdateSet::ProcessOutput::write_egui_wants_input_system`), so
//! [`update_egui_input_capture`] reads a value written the previous frame
//! when it runs in `Update`. The `emit_action_input` producer runs in
//! `PreUpdate` — one schedule slot earlier than `Update` — and reads the
//! `EguiKeyboardCaptured` mirror that `update_egui_input_capture` wrote
//! during the prior frame's `Update`. All consumers therefore share the same
//! 1–2 frame mirror-lag tolerance: focus is acquired well over two frames
//! before any character is typed into a field, and on focus release the worst
//! case is hotkeys staying suppressed for one extra ~16 ms frame. (The
//! pointer mirror has the same property; the panel doesn't move between
//! frames and the cursor must already be over it before the user can click.)
//!
//! ## What consumers do with it
//!
//! Sketches gate press-edge handlers (e.g., "mouse just pressed → spawn
//! attractor") on `!EguiPointerCaptured.0`. Release events and continuous
//! position updates still fire regardless, so an attractor that was activated
//! outside the panel and then dragged over it still releases cleanly.
//! Keyboard-action consumers read only `ActionInput` messages, which the
//! producer never emits while a text field has focus — there is no app
//! hotkey that should still fire then (and `just_pressed` edges belong to the
//! frame they occur in, so a skipped frame cannot replay them later).

use bevy::prelude::*;

/// Mirror of `bevy_egui::EguiWantsInput::wants_any_pointer_input()`. `true`
/// when the egui UI wants the pointer (hovering over a panel, dragging a
/// widget, popup open). Sketches gate their press-edge handlers on this.
///
/// Default is `false` so the resource is safe to initialize at plugin build
/// time even before [`update_egui_input_capture`] has run for the first
/// time.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct EguiPointerCaptured(pub bool);

/// Mirror of `bevy_egui::EguiWantsInput::wants_any_keyboard_input()`. `true`
/// while an egui widget has keyboard focus (a dev-panel text field being
/// typed into, an open popup). App hotkey systems are gated on this via
/// [`egui_not_capturing_keyboard`] so typing "2" into a tuning field cannot
/// switch sketches.
///
/// Default is `false` (same plugin-build-time rationale as
/// [`EguiPointerCaptured`]).
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct EguiKeyboardCaptured(pub bool);

/// Reflect `bevy_egui`'s input-capture state into [`EguiPointerCaptured`]
/// and [`EguiKeyboardCaptured`].
///
/// Reads `Option<Res<bevy_egui::input::EguiWantsInput>>` so that test
/// harnesses running without `EguiPlugin` don't crash on a missing resource.
/// When `bevy_egui` isn't initialized, both wrappers reset to `false`.
pub fn update_egui_input_capture(
    egui_wants: Option<Res<'_, bevy_egui::input::EguiWantsInput>>,
    mut pointer: ResMut<'_, EguiPointerCaptured>,
    mut keyboard: ResMut<'_, EguiKeyboardCaptured>,
) {
    pointer.0 = egui_wants
        .as_ref()
        .is_some_and(|w| w.wants_any_pointer_input());
    keyboard.0 = egui_wants.is_some_and(|w| w.wants_any_keyboard_input());
}

/// Run condition: `true` while app hotkeys may fire — i.e. egui does NOT
/// have keyboard focus. Apply with `.run_if(egui_not_capturing_keyboard)` to
/// every system that consumes keyboard-bound
/// [`crate::lifecycle::actions::WaveConductorAction`]s.
///
/// Takes `Option<Res>` so harnesses (and plugin subsets) that never
/// initialize [`EguiKeyboardCaptured`] keep their hotkeys: a missing resource
/// means no egui UI, which cannot be capturing anything.
#[must_use]
pub fn egui_not_capturing_keyboard(captured: Option<Res<'_, EguiKeyboardCaptured>>) -> bool {
    captured.is_none_or(|c| !c.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts how often the gated probe system ran.
    #[derive(Resource, Default)]
    struct ProbeRuns(u32);

    fn probe(mut runs: ResMut<'_, ProbeRuns>) {
        runs.0 += 1;
    }

    fn probe_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<ProbeRuns>();
        app.add_systems(Update, probe.run_if(egui_not_capturing_keyboard));
        app
    }

    #[test]
    fn hotkeys_run_when_capture_resource_is_absent() {
        // Harnesses without SettingsPlugin/EguiPlugin never insert the
        // resource; the gate must fail open (hotkeys keep working).
        let mut app = probe_app();
        app.update();
        assert_eq!(app.world().resource::<ProbeRuns>().0, 1);
    }

    #[test]
    fn hotkeys_are_suppressed_while_egui_captures_the_keyboard() {
        let mut app = probe_app();
        app.insert_resource(EguiKeyboardCaptured(true));
        app.update();
        assert_eq!(
            app.world().resource::<ProbeRuns>().0,
            0,
            "gated system must not run while a text field has focus"
        );

        // Focus released → the gate reopens on the next frame.
        app.insert_resource(EguiKeyboardCaptured(false));
        app.update();
        assert_eq!(app.world().resource::<ProbeRuns>().0, 1);
    }
}
