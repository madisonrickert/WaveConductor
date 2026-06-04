//! Continuous Leap **heartbeat monitor** — the instrument for reproducing the
//! Ultraleap service wedge under GPU contention (`leap-deep-idle-state`).
//!
//! [`leap_recovery_probe`] answers "is it wedged *right now*" in one shot; this
//! runs until you stop it (or one hour elapses), printing a frames-per-second
//! line each second and **loudly flagging a wedge** — a sustained drop to zero
//! frames — with the elapsed timestamp. Start it, then crank up GPU load in
//! another process and watch for the `WEDGE` line and *when* it appears relative
//! to the load you applied.
//!
//! It mirrors the planned in-app detector (a frame heartbeat that flips to
//! `TrackingFlow::NotStreaming` after ~1 s of silence), so it doubles as a
//! prototype for that logic.
//!
//! Run (no hand required — a healthy service streams ~90 empty frames/s):
//!
//! ```text
//! cargo run -p wc-core --example leap_heartbeat_monitor --features hand-tracking-gestures
//! ```
#![cfg(feature = "hand-tracking-gestures")]
#![allow(clippy::print_stdout, clippy::expect_used)]

use std::thread::sleep;
use std::time::{Duration, Instant};

use bevy::prelude::Messages;
use wc_core::input::provider::HandTrackingProvider;
use wc_core::input::providers::leap_native::LeaprsProvider;
use wc_core::input::state::HandTrackingFrame;

/// Consecutive zero-frame seconds before we declare a wedge. Matches the
/// in-app `STALE_FRAME_THRESHOLD` intent (~1 s of silence on a service we
/// believe is streaming is unambiguous), with one second of debounce on top to
/// avoid flagging a single hiccup.
const WEDGE_AFTER_ZERO_SECS: u32 = 2;

/// Safety cap (seconds) so a forgotten background run doesn't linger forever.
const MAX_RUN_SECS: u64 = 60 * 60;

fn main() {
    println!(
        "Leap heartbeat monitor — frames/s, flags a wedge after {WEDGE_AFTER_ZERO_SECS}s of \
         zero frames. Ctrl-C to stop.\n"
    );

    let mut provider = LeaprsProvider::default();
    // Unfocused CLI: request BackgroundFrames so a healthy service isn't falsely
    // read as silent (see leap_recovery_probe for the rationale).
    provider.request_background = true;
    provider
        .start()
        .expect("LeaprsProvider::start failed — is a Leap connected?");
    let mut msgs = Messages::<HandTrackingFrame>::default();

    let max_run = Duration::from_secs(MAX_RUN_SECS);
    let started = Instant::now();
    let mut second = Instant::now();
    let mut frames_this_second = 0usize;
    let mut zero_streak = 0u32;
    let mut wedged = false; // edge-trigger so we print WEDGE / RECOVERED once each

    while started.elapsed() < max_run {
        provider.poll(Duration::ZERO, &mut msgs);
        frames_this_second += msgs.drain().count();

        if second.elapsed() >= Duration::from_secs(1) {
            let t = started.elapsed().as_secs();
            if frames_this_second == 0 {
                zero_streak += 1;
                if zero_streak >= WEDGE_AFTER_ZERO_SECS && !wedged {
                    wedged = true;
                    println!(
                        "[t={t:>4}s] ⚠⚠ WEDGE — 0 frames for {zero_streak}s. Note what GPU load \
                         was applied. (macOS recovery: physical USB replug.)"
                    );
                } else if !wedged {
                    println!("[t={t:>4}s] 0 frames/s (zero streak {zero_streak}s)…");
                }
            } else {
                if wedged {
                    println!(
                        "[t={t:>4}s] ✅ RECOVERED — {frames_this_second} frames/s after a wedge."
                    );
                }
                wedged = false;
                zero_streak = 0;
                println!("[t={t:>4}s] {frames_this_second} frames/s OK");
            }
            frames_this_second = 0;
            second = Instant::now();
        }
        sleep(Duration::from_millis(4));
    }

    provider.stop();
    println!("\nReached {MAX_RUN_SECS}s run cap — stopping.");
}
