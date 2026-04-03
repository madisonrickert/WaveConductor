# Architecture Decision Log

Decisions made during the sketch class hierarchy refactoring (April 2026).

---

## ADL-001: Domain-based file structure over type-based grouping

**Context:** The `src/common/` directory had become a grab bag — sketch framework, Leap Motion integration, particle physics, audio context, settings, and math utilities all lived together because they were "shared." A `src/common/hooks/` directory grouped `useSketchLifecycle`, `useLeapStatus`, and `useThrottledNavigate` together solely because they were all hooks.

**Decision:** Organize by domain, not by technical role. Each directory corresponds to a feature area: `sketch/`, `settings/`, `audio/`, `leap/`, `particles/`. Hooks, types, and components co-locate with the domain they serve. Only truly generic utilities (`math.ts`, `hooks/useThrottledNavigate`) remain at the top level.

**Trade-offs:**
- (+) A developer working on Leap finds everything in `leap/` — controller, status types, hook, and indicator component.
- (+) Adding a new domain means creating one directory, not scattering files across `common/`, `common/hooks/`, and `components/`.
- (-) Import paths are slightly longer (`@/settings/store` vs `@/common/sketchSettingsStore`), though more descriptive.

**Alternatives rejected:**
- Keeping `common/` with better sub-directories — still hides the "why" behind a generic name.
- Feature folders per sketch (each sketch owning its own copy of shared code) — creates duplication.

---

## ADL-002: Decompose SketchView via custom hooks, not sub-components alone

**Context:** `SketchComponent` (now `SketchView`) was 316 lines with 8 state variables and 8 `useEffect` hooks managing sketch instantiation, settings, volume, keyboard shortcuts, dev panel, mouse idle, Leap status, and screen saver.

**Decision:** Extract state and effects into custom hooks (`useSketchInstance`, `useSketchSettingsManager`, `useVolume`, `useMouseIdle`), and extract JSX into child components (`SketchRenderer`, `SketchOverlay`, `SketchErrorBoundary`). SketchView becomes a ~69-line composition root with zero `useEffect` calls.

**Trade-offs:**
- (+) Each hook is independently testable without rendering the full component tree.
- (+) SketchView reads as a wiring diagram — you can see all concerns at a glance.
- (-) More files to navigate (7 instead of 1), though each is focused and small.

**Alternatives rejected:**
- Extracting only sub-components (not hooks) — reduces JSX but doesn't help the state management tangle. The 8 `useEffect` calls would remain in the parent.
- A single `useSketchView` hook containing all logic — just moves the God Component into a God Hook.

---

## ADL-003: Template Method with overridable `animate()`, not a sealed method with optional hooks

**Context:** All 5 sketches had identical `animate()` boilerplate: check Leap hands, gate simulation on idle, update idle state. Only LineSketch needed extra per-frame work (attractor decay) that runs even when idle.

**Decision:** Make `animate()` concrete in `BaseSketch` with the shared orchestration, and require subclasses to implement `step()` for their simulation. Sketches that need work outside the idle gate (currently only LineSketch) override `animate()` and call `super.animate()`.

**Trade-offs:**
- (+) No optional hooks on the base class for one sketch's needs.
- (+) `super.animate()` is a well-understood OOP convention.
- (-) TypeScript has no `final` — a subclass could override `animate()` and forget to call `super`. The JSDoc makes the contract clear.

**Alternatives rejected:**
- Sealed `animate()` with an `animateAlways()` optional hook — adds a permanent method to the base class that only one sketch uses. Pollutes the API surface.
- Making `animate()` abstract and letting each sketch re-implement the orchestration — the original problem.

---

## ADL-004: `BaseSketch` naming over `Sketch`

**Context:** The abstract base class was named `Sketch`, matching the concrete sketches (`LineSketch`, `DotsSketch`, etc.). This was ambiguous when scanning imports — `Sketch` could be mistaken for a concrete sketch.

**Decision:** Rename to `BaseSketch`. Apply the `Sketch` suffix consistently to all concrete sketches: `LineSketch`, `DotsSketch`, `CymaticsSketch`, `FlameSketch`, `WavesSketch`.

**Trade-offs:**
- (+) `class DotsSketch extends BaseSketch` is immediately clear.
- (+) Consistent suffix across all 5 concrete classes.
- (-) `Base` prefix is more common in C#/Java than TypeScript — the `abstract` keyword already communicates "not instantiable." Accepted because the consistency benefit outweighs the style preference.

---

## ADL-005: Factory method for Leap controller, not a base class property

**Context:** All 5 sketches constructed `LeapHandController` with 4 identical properties (`canvas`, `renderer`, `getConnectionCallback`, `getProtocolVersionCallback`) and 2-3 sketch-specific ones (`renderMode`, `onFrame`, `handMaterial`).

**Decision:** Add `createLeapController()` to `BaseSketch` that pre-fills the shared properties. Subclasses call it in `init()` and assign to the inherited `protected leapHands` field. The base `destroy()` disposes it.

**Trade-offs:**
- (+) Eliminates 4 boilerplate lines per sketch and the `LeapHandController` import.
- (+) Uses `Omit<T, K>` so the compiler enforces what the subclass must provide.
- (-) `leapHands` lives on the base class even though it's a Leap-specific concept. Accepted because all 5 sketches use it and `animate()` checks it.

**Alternatives rejected:**
- Making `leapHands` private and auto-creating it in the base class — each sketch's `onFrame` callback is fundamentally different (attract, drag camera, move wave center, dual-hand assignment). No useful default exists.
- A Leap mixin — TypeScript mixins are awkward and would complicate the type hierarchy.

---

## ADL-006: Pure function for attractor power, not a class or mixin

**Context:** LineSketch and DotsSketch had identical grab-strength-to-attractor-power formulas with different constants (threshold, decay speed, power floor).

**Decision:** Extract `computeLeapAttractorPower()` as a pure function with a `LeapAttractorPowerConfig` parameter object. Each sketch declares its config as a constant.

**Trade-offs:**
- (+) Pure function — testable with zero setup, no lifecycle, no state.
- (+) Config object makes the tunable parameters explicit and named.
- (-) Only 2 of 5 sketches use it — not a universal abstraction.

**Alternatives rejected:**
- An `AttractorSketch` intermediate subclass — only 2 sketches would extend it, and it would force a rigid inheritance hierarchy.
- A method on `Attractor` — the power computation depends on Leap hand data, not just attractor state.

---

## ADL-007: Extract Flame audio as a class, convert chord IIFE to a factory function

**Context:** FlameSketch had a 125-line `initAudio()` method creating an inline audio graph, plus an IIFE that captured mutable state in a closure and returned an object with setters (`setScaleDegree`, `setIsMajor`, etc.) — effectively a class written as a function.

**Decision:** Extract `FlameAudio` as a class in `audio.ts` with three methods matching the three call sites: `configureForName()`, `updateForCamera()`, `updateFromFractalStats()`. Extract the chord IIFE into a `createChord()` factory function that returns the existing `Chord` interface.

**Trade-offs:**
- (+) Follows the pattern of the other 4 sketches (each has its own `audio.ts`).
- (+) FlameSketch drops from ~550 to 383 lines.
- (+) The chord factory is testable independently of the sketch.
- (-) The chord is still a closure-with-methods rather than a class — accepted because the `Chord` interface already exists in `types.ts` and the factory pattern keeps the oscillator wiring localized.

**Alternatives rejected:**
- Converting `Chord` to a full class — would require exposing the `AudioNodeTracker` to the class for oscillator lifecycle management. The factory function is simpler because the tracker is already in scope.
- A generic `SketchAudio` base class — the 5 audio modules are too different (LFO + noise, sample player + oscillator, chord stack + compressor, etc.).

---

## ADL-008: Named component files over index.tsx

**Context:** UI components used the `directory/index.tsx` pattern (e.g., `homeButton/index.tsx`), causing tab ambiguity and search friction in editors.

**Decision:** Rename component files to match their export: `homeButton/HomeButton.tsx`, `screenSaver/ScreenSaver.tsx`, etc. Keep true barrel exports (`audio/index.ts`, `particles/index.ts`, `sketches/index.ts`) as `index.ts` since they are genuine public API surfaces.

**Trade-offs:**
- (+) Tabs and file search show the actual component name.
- (+) Clear distinction between "this file IS the component" and "this file re-exports from siblings."
- (-) Import paths are slightly longer (`@/ui/homeButton/HomeButton` vs `@/ui/homeButton`).

---

## ADL-009: `disposeComposer()` utility over automatic disposal tracking

**Context:** `EffectComposer` disposal requires iterating and disposing each pass before disposing the composer. LineSketch and CymaticsSketch did this correctly; DotsSketch did not (GPU resource leak). Three different `destroy()` implementations existed.

**Decision:** Extract a `disposeComposer()` utility function. Fix the Dots bug by adding the call. Keep disposal explicit — each sketch's `destroy()` calls the functions it needs.

**Trade-offs:**
- (+) Simple, no lifecycle management overhead.
- (+) Fixes a real bug (Dots leaking shader programs and render targets).
- (-) Sketches must still remember to call it — but the pattern is now visible and consistent.

**Alternatives rejected:**
- A `track(resource)` + auto-dispose pattern on the base class — Three.js shares materials and geometries across objects (e.g., `Attractor.geometry` is static). Automatic disposal would destroy shared resources. Explicit is safer.
