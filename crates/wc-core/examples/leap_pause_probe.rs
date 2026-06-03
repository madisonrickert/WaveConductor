//! Hardware spike for `leap-idle-pause` (Phase 1): measure the Ultraleap service
//! resume latency `L` — the time from `set_pause(false)` to the first frame that
//! carries a hand — and prompt the operator to read paused-vs-running daemon CPU.
//!
//! Run with a Leap controller connected and your hand held over the sensor:
//!
//! ```text
//! cargo run -p wc-core --example leap_pause_probe --features hand-tracking-gestures
//! ```
//!
//! This is a throwaway measurement harness, not production code: it drives the
//! real `LeaprsProvider` (best fidelity) directly, without a Bevy `App`.
#![cfg(feature = "hand-tracking-gestures")]
#![allow(clippy::print_stdout, clippy::expect_used)]

use std::thread::sleep;
use std::time::{Duration, Instant};

use bevy::prelude::Messages;
use wc_core::input::provider::HandTrackingProvider;
use wc_core::input::providers::leap_native::LeaprsProvider;
use wc_core::input::state::HandTrackingFrame;

/// Poll once and return any frames produced since the last poll.
fn poll_frames(
    provider: &mut LeaprsProvider,
    msgs: &mut Messages<HandTrackingFrame>,
) -> Vec<HandTrackingFrame> {
    provider.poll(Duration::ZERO, msgs);
    msgs.drain().collect()
}

/// Keep the connection serviced for `dur`, polling every 5 ms. Returns how many
/// hand-bearing frames were seen (0 once the service is paused and quiesced).
fn pump(
    provider: &mut LeaprsProvider,
    msgs: &mut Messages<HandTrackingFrame>,
    dur: Duration,
) -> u32 {
    let start = Instant::now();
    let mut hands = 0u32;
    while start.elapsed() < dur {
        for frame in poll_frames(provider, msgs) {
            if !frame.hands.is_empty() {
                hands += 1;
            }
        }
        sleep(Duration::from_millis(5));
    }
    hands
}

fn main() {
    let mut provider = LeaprsProvider::default();
    provider
        .start()
        .expect("LeaprsProvider::start failed — is a Leap connected?");
    let mut msgs = Messages::<HandTrackingFrame>::default();

    println!("Warming up (handshake + AllowPauseResume). Hold your hand over the sensor…");
    // ~4 s warm-up: lets the service connect and the pause policy arm, and
    // confirms a hand is actually present before we start timing.
    let seen = pump(&mut provider, &mut msgs, Duration::from_secs(4));
    assert!(
        seen > 0,
        "no hand-bearing frames during warm-up — keep your hand over the sensor"
    );
    println!(
        "Warm-up OK ({seen} hand frames). Starting latency measurement — keep your hand still.\n"
    );

    // ── Latency measurement: 20 pause→resume cycles ──────────────────────────
    let mut samples: Vec<Duration> = Vec::with_capacity(20);
    for i in 1..=20 {
        provider.set_paused(true);
        // Let the service fully quiesce, then clear any backlog.
        sleep(Duration::from_millis(400));
        let _ = poll_frames(&mut provider, &mut msgs);

        let t0 = Instant::now();
        provider.set_paused(false);

        // Poll until a hand-bearing frame arrives (or give up after 2 s).
        let mut latency = None;
        let deadline = t0 + Duration::from_secs(2);
        while Instant::now() < deadline {
            if poll_frames(&mut provider, &mut msgs)
                .iter()
                .any(|f| !f.hands.is_empty())
            {
                latency = Some(t0.elapsed());
                break;
            }
            sleep(Duration::from_millis(1));
        }
        match latency {
            Some(l) => {
                println!("  cycle {i:>2}: L = {:>6.1} ms", l.as_secs_f64() * 1000.0);
                samples.push(l);
            }
            None => println!("  cycle {i:>2}: TIMEOUT (>2 s, no hand frame after resume)"),
        }
    }

    // ── Summary + saving estimate ────────────────────────────────────────────
    if !samples.is_empty() {
        samples.sort_unstable();
        let to_ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let median = samples[samples.len() / 2];
        let min = samples[0];
        let max = samples[samples.len() - 1];
        let saving = (1.0 - median.as_secs_f64() / 0.5).clamp(0.0, 1.0) * 100.0;
        println!(
            "\nL: min {:.1} ms / median {:.1} ms / max {:.1} ms (n={})",
            to_ms(min),
            to_ms(median),
            to_ms(max),
            samples.len(),
        );
        println!("Estimated Leap-service CPU saved at P=500 ms ≈ {saving:.0}%");
        println!("Decision gate: median < 250 ms → proceed with duty cycle; ≥ 400 ms → fall back to \"don't pause\".\n");
    }

    // ── CPU windows: operator reads Activity Monitor / `top` ─────────────────
    println!("CPU CHECK — open Activity Monitor (or `top -o cpu`) and watch the Ultraleap service process.");
    println!(">>> SERVICE RUNNING for 30 s — note its %CPU now …");
    let running_hands = pump(&mut provider, &mut msgs, Duration::from_secs(30));
    provider.set_paused(true);
    println!("    (saw {running_hands} hand frames while running)");
    println!(">>> SERVICE PAUSED for 30 s — note its %CPU now (frames should stop) …");
    let paused_hands = pump(&mut provider, &mut msgs, Duration::from_secs(30));
    println!("    (saw {paused_hands} hand frames while paused — expect ~0)");

    provider.set_paused(false);
    provider.stop();
    println!("\nDone. Record: median L, running %CPU, paused %CPU. These set the Phase 4 tuning + the go/no-go.");
}
