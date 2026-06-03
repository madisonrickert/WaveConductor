//! Duty-cycle stress test for `leap-idle-pause` debugging (2026-06-03).
//!
//! The live test showed that pausing/resuming the Ultraleap service every ~0.5 s
//! (165 ms sample window) wedges the device: the `set_pause` control path keeps
//! ack'ing but the device stops producing frames. This harness sweeps the sample
//! **window length** (longest → shortest) to find the minimum window at which the
//! device reliably tracks each cycle WITHOUT wedging. After each config it runs a
//! **recovery probe** (resume + wait up to 3 s for a frame) to tell "wedged"
//! apart from "window merely too short for a frame this cycle".
//!
//! Run with a hand held over the sensor the WHOLE time:
//!   cargo run -p wc-core --example leap_duty_stress --features hand-tracking-gestures
#![cfg(feature = "hand-tracking-gestures")]
#![allow(clippy::print_stdout, clippy::expect_used)]

use std::thread::sleep;
use std::time::{Duration, Instant};

use bevy::prelude::Messages;
use wc_core::input::provider::HandTrackingProvider;
use wc_core::input::providers::leap_native::LeaprsProvider;
use wc_core::input::state::HandTrackingFrame;

fn poll_frames(p: &mut LeaprsProvider, m: &mut Messages<HandTrackingFrame>) -> Vec<HandTrackingFrame> {
    p.poll(Duration::ZERO, m);
    m.drain().collect()
}

/// Resume for `window`, polling tightly; return how many hand-bearing frames arrived.
fn sample(p: &mut LeaprsProvider, m: &mut Messages<HandTrackingFrame>, window: Duration) -> u32 {
    let t0 = Instant::now();
    let mut hands = 0u32;
    while t0.elapsed() < window {
        for f in poll_frames(p, m) {
            if !f.hands.is_empty() {
                hands += 1;
            }
        }
        sleep(Duration::from_millis(2));
    }
    hands
}

/// Probe whether the device is wedged: resume and wait up to 3 s for any
/// hand-bearing frame. Returns true if a frame arrived (device alive).
fn recovery_probe(p: &mut LeaprsProvider, m: &mut Messages<HandTrackingFrame>) -> bool {
    p.set_paused(false);
    let t0 = Instant::now();
    let mut alive = false;
    while t0.elapsed() < Duration::from_secs(3) {
        if poll_frames(p, m).iter().any(|f| !f.hands.is_empty()) {
            alive = true;
            break;
        }
        sleep(Duration::from_millis(2));
    }
    alive
}

/// Run one duty-cycle config for `secs`. Returns (cycles, cycles_with_a_hand_frame,
/// total_frames, wedged).
fn run_config(
    p: &mut LeaprsProvider,
    m: &mut Messages<HandTrackingFrame>,
    window: Duration,
    gap: Duration,
    secs: Duration,
) -> (u32, u32, u32, bool) {
    let start = Instant::now();
    let (mut cycles, mut with_hand, mut total) = (0u32, 0u32, 0u32);
    while start.elapsed() < secs {
        p.set_paused(false);
        let h = sample(p, m, window);
        p.set_paused(true);
        sleep(gap);
        let _ = poll_frames(p, m); // discard backlog accrued during the gap
        cycles += 1;
        total += h;
        if h > 0 {
            with_hand += 1;
        }
    }
    // Discriminator: is the device wedged, or was the window just too short?
    let alive = recovery_probe(p, m);
    p.set_paused(true);
    (cycles, with_hand, total, !alive)
}

fn main() {
    let mut p = LeaprsProvider::default();
    // LeapC only streams to the FOCUSED app unless BackgroundFrames is requested.
    // This CLI never has focus (and the Control Panel may be focused), so without
    // this the harness is starved and reads zero frames — a harness bug, not a
    // device wedge. The real app sets this too.
    p.request_background = true;
    p.start().expect("LeaprsProvider::start failed — is a Leap connected?");
    let mut m = Messages::<HandTrackingFrame>::default();

    println!("Put your hand over the sensor — waiting up to 20s for tracking to come up…");
    let t0 = Instant::now();
    let mut warm = 0u32;
    while t0.elapsed() < Duration::from_secs(20) {
        warm += sample(&mut p, &mut m, Duration::from_millis(200));
        if warm >= 30 {
            break; // hand is solidly tracked; proceed
        }
    }
    assert!(
        warm > 0,
        "no hand frames in 20s — hand not over sensor, or device still wedged (replug)"
    );
    println!("Warm-up OK ({warm} hand frames). Sweeping sample windows (longest → shortest).\n");

    // (window, gap) longest-window-first so a wedge (expected only at the short
    // end) lands last; we stop at the first wedge.
    let configs = [
        (Duration::from_millis(1000), Duration::from_millis(500)),
        (Duration::from_millis(500), Duration::from_millis(500)),
        (Duration::from_millis(300), Duration::from_millis(350)),
        (Duration::from_millis(150), Duration::from_millis(350)), // the known-bad live setting
    ];

    for (window, gap) in configs {
        let wms = window.as_millis();
        let gms = gap.as_millis();
        println!("── config: window={wms}ms gap={gms}ms (period {}ms) for 20s ──", wms + gms);
        // Settle: 2 s fully un-paused so each config starts from a clean tracking state.
        p.set_paused(false);
        let s = Instant::now();
        while s.elapsed() < Duration::from_secs(2) {
            let _ = poll_frames(&mut p, &mut m);
            sleep(Duration::from_millis(5));
        }

        let (cycles, with_hand, total, wedged) =
            run_config(&mut p, &mut m, window, gap, Duration::from_secs(20));
        let pct = if cycles > 0 { with_hand * 100 / cycles } else { 0 };
        let avg = if cycles > 0 { total as f64 / cycles as f64 } else { 0.0 };
        println!(
            "   cycles={cycles}  windows-with-a-hand={with_hand}/{cycles} ({pct}%)  \
             avg frames/window={avg:.1}  ->  {}",
            if wedged { "WEDGED (recovery probe saw no frame in 3s)" } else { "device alive" },
        );
        if wedged {
            println!("\n>>> WEDGED at window={wms}ms. Shorter windows would too. Stopping.");
            println!(">>> REPLUG the Leap before any further testing.");
            break;
        }
        println!();
    }

    p.set_paused(false);
    p.stop();
    println!("\nDone. The smallest window with ~100% windows-with-a-hand AND 'device alive' is the");
    println!("minimum safe sample window; the largest 'WEDGED' one is the failure threshold.");
}
