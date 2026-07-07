# CI cost review (T13)

*Reviewed 2026-07-02 against `.github/workflows/ci.yml` @ the v5-alpha audit-fix batch. Updated 2026-07-07 for the Promote Alpha workflow migration. Scope: AUDIT.md T13 — measure per-job runner cost, cache effectiveness, and triggers; decide whether any PR-level cross-platform step fits the budget (AUDIT §6: "the priority is a cost dive; right-sizing, possibly less, not more").*

## Headline finding: the repo is public, so runner minutes are free

`madisonrickert/WaveConductor` is a **public** repository. GitHub-hosted Actions minutes are **unmetered for public repos** on the standard runners — including the 10×-billed `macos-latest` and 2×-billed `windows-latest` that the cross-platform job uses. The per-minute *billing multipliers* only apply to private repos and metered plans.

So the premise behind AUDIT §6's "the last release's cross-platform CI overran the intended budget" no longer holds as a **dollar** cost: either the repo was private at the time, or "budget" meant wall-clock / queue time rather than money. Either way, on a public repo there is no $ ceiling to right-size against.

That reframes the review: the remaining cost levers are **wall-clock feedback time**, **runner-queue contention**, and **redundancy** — not money.

## Trigger map (who runs when)

| Trigger | Jobs that run |
| --- | --- |
| PR to `main` / `v5-alpha` | fmt, clippy, check-secrets, validate-shaders, deny, test-linux, doc — **Linux only** |
| Push to `v5-alpha` | same set — **Linux only** |
| Push to `main` | the Linux set **+ test-cross-platform** (macOS + Windows) |
| Promote Alpha workflow | full Linux set **+ test-cross-platform** (macOS + Windows) **+ release artifact builds**; publish mode creates the annotated tag and pre-release after success |

PRs and `v5-alpha` pushes are Linux-only for fast feedback. The cross-platform matrix in `ci.yml` is gated to `main` pushes; pre-release portability validation moved into `.github/workflows/release.yml` so a manual promotion run validates the target SHA, builds the three release artifacts, creates the annotated tag, and publishes those exact artifacts. Tags no longer trigger a second full CI or release rebuild.

## Actions taken

**Retired the standalone `cargo audit` job.** It ran `rustsec/audit-check` against the same RustSec advisory DB as `cargo deny check`, but did **not** honor `deny.toml`'s ignore list. As of 2026-07-02 that made it actively harmful: the transitively-forced, not-exploitable `quick-xml` advisory (RUSTSEC-2026-0194) is ignored in `deny.toml`, but the audit job would have failed on it with no equivalent ignore — forcing a *second* advisory-ignore config (`audit.toml`) to be maintained in parallel. `cargo deny check` is now the single advisory gate (advisories + licenses + bans + sources, all honoring `deny.toml`). Net: one fewer job per run, and advisory ignores live in exactly one place.

## Considered and deliberately NOT changed

- **PR-level cross-platform (M7).** Stands as an accepted tradeoff (AUDIT §6 / M7). PRs get fast Linux feedback; macOS/Windows portability regressions are caught on the main-push or Promote Alpha runs before a release. On a public repo this coverage is now *free*, so there's no cost argument to remove it — but there's also no need to add it to PRs: the Linux-only PR gate is fast, and macOS and Windows — both first-class targets (macOS the primary dev box, Windows the recommended deploy OS; see the roadmap's Deployment targets) — are exercised before any alpha tag. If a portability regression ever slips to `main` and stings, revisit then.
- **Tag-triggered CI/release rebuilds.** Removed on 2026-07-07. The old flow encouraged a release dry run, then a tag push, then a duplicate tag CI + release rebuild. Promote Alpha runs validation and release builds once for the target SHA, then publishes those same artifacts.
- **Per-job recompilation.** Five heavy Linux jobs (clippy, test-linux, doc, check-secrets, validate-shaders) each compile independently — `Swatinem/rust-cache` caches across runs of a job but not across jobs within one run. Consolidating them into one serial job would compile once but serialize what is currently parallel, trading faster wall-clock feedback for fewer total core-minutes. On free Linux minutes the parallel layout (faster feedback) is the better trade. Left as-is; documented here so it's a conscious choice, not an oversight.
- **`CARGO_INCREMENTAL: 0`, rust-cache placement, mold on Linux.** Already correct.

## If the repo ever goes private again

The billing multipliers snap back (macOS 10×, Windows 2×). At that point the levers, in order of impact:
1. Restrict `test-cross-platform` to **Promote Alpha only** (drop the main-push trigger) — halves macOS runs across a merge-then-release cycle.
2. Consider consolidating the heavy Linux jobs to compile-once (accepting slower feedback).
3. Reconsider whether both macOS *and* Windows must run every release, or alternate.

Until then, none of these is worth the wall-clock/feedback regression.
