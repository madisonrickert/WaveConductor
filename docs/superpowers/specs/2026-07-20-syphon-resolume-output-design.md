# Syphon + OSC output to Resolume — design

**Status:** Research / design — shovel-ready for a macOS implementation session. No code in this change.
**Date:** 2026-07-20. All web claims below were verified on this date unless stamped otherwise; all crate/API claims were verified against the exact versions pinned in this workspace's `Cargo.lock`.
**Scope:** the "Resolume (Syphon + OSC) fork" from the outdoor-tracking strategy doc (`2026-07-06-outdoor-tracking-strategy-design.md`, § *Modes & external output*, commit `113989c6`), scope-fork **(a)**: *WaveConductor renders; Resolume composites/cues.* WaveConductor exposes a **Syphon server** (video out) and an **OSC bridge** (control/telemetry, both directions). macOS-only at runtime; compiles as a documented no-op facade everywhere else, exactly like `input/obsbot/`.
**Non-goals:** scope-fork (b) (Resolume renders, WaveConductor as tracking engine only); Spout/NDI (Windows/network equivalents — the module boundary is drawn so they can slot in later, but nothing here designs them); publishing the egui settings UI (deliberately excluded from the feed — see below).

---

## Why this exists

For a scripted performance, Resolume Arena handles sequencing, compositing, projector mapping, and cueing far better than an in-app timeline we'd have to build. The macOS-standard way to hand Resolume a live video stream is **Syphon** (zero-copy GPU surface sharing); the standard control channel is **OSC**. The tracking doc already concluded this bridge is "a general capability that could serve all sketches, not just body — build it as its own module if pursued." This doc is that module's design: the exact hook point in our render pipeline, the FFI surface, the synchronization contract, packaging, settings, and a phased plan a macOS session can execute directly.

The plan is deliberately conservative about one thing: **every GPU-side claim below that could not be exercised on this Windows box is listed in "Open questions" with the first thing to check on the Mac.**

---

## Verified facts

### Syphon framework (github.com/Syphon/Syphon-Framework)

- **`SyphonMetalServer` is the current publishing API** (the OpenGL `SyphonOpenGLServer` lives alongside for legacy apps). Verified from the headers on the default branch, 2026-07-20:

  ```objc
  // SyphonMetalServer.h
  - (id)initWithName:(nullable NSString*)name
              device:(id<MTLDevice>)device
             options:(nullable NSDictionary<NSString *, id> *)options;
  - (void)publishFrameTexture:(id<MTLTexture>)textureToPublish
              onCommandBuffer:(id<MTLCommandBuffer>)commandBuffer
                  imageRegion:(NSRect)region
                      flipped:(BOOL)isFlipped;
  - (nullable id<MTLTexture>)newFrameImage;   // client-side helper; "new" ⇒ caller releases
  - (void)stop;
  // properties: device, name (mutable), serverDescription, hasClients
  ```

  Implementation facts from `SyphonMetalServer.m` (default branch, 2026-07-20):
  - The shared cross-process texture is **`MTLPixelFormatBGRA8Unorm`**, IOSurface-backed, usage `RenderTarget | ShaderRead`. BGRA8 is not a recommendation, it *is* the wire format; anything else goes through a conversion render pass.
  - **Fast path:** if the published texture matches pixel format + sample count, is not framebuffer-only, and `flipped:NO`, publishing is a plain `MTLBlitCommandEncoder` copy **encoded onto the command buffer you pass in**. Flipped or mismatched formats fall back to a slower internal render pass — so render right-side-up and hand it BGRA8.
  - **Frame announcement rides your command buffer:** Syphon calls `[commandBuffer addCompletedHandler:…publish…]`, so clients are signaled only after *your* GPU work completes. This is the load-bearing fact for the synchronization design below: whoever commits that command buffer controls when the frame becomes visible.
  - Thread safety: `@synchronized(self)` around shared-texture access; the header documents the class as safe across threads. Client-side `newFrameHandler` "may be invoked on a thread other than that on which the client was created."
  - `SyphonServerOptionIsPrivate` (BOOL) skips discovery; default is public.
- **License:** BSD 3-clause-style, in `License.txt` (there is no `LICENSE` file; that URL 404s). "Copyright 2010 bangnoise (Tom Butterworth) & vade (Anton Marini)." Binary redistribution **must reproduce the notice, conditions and disclaimer in the documentation/materials shipped with the distribution** — see *Licensing* below.
- **Releases:** the latest tagged release is **"Syphon SDK 5", tag `5`, published 2019-03-02** (GitHub API-confirmed) — it **predates the Metal API** (Metal server work landed Jan 2023; latest commit on the default branch 2025-10-06, including a 2025-10-01 "Explicitly set pixel format type to 32BGRA" merge). **Consequence: build `Syphon.framework` from current source with Xcode; no prebuilt release contains `SyphonMetalServer`.** A CMake port PR (#65) was abandoned in 2022 — Xcode is the only supported build. Output is a dynamic `Syphon.framework` with an `@rpath` install name; no static-library target exists.
- **Bare-binary (non-.app) feasibility:** positive evidence — `cansik/syphon-python` publishes from a plain Python process with no app bundle; Syphon SDK 3 notes (2017) cite "improved command-line tool behavior". The run-loop caveat documented in the syphon-python docs applies to the **discovery side** (`SyphonServerDirectory` clients must pump the NSRunLoop), with no such requirement stated for servers. WaveConductor is a winit app whose main thread pumps the NSRunLoop anyway, so we're better-positioned than a true CLI tool. Residual risk (unverified anywhere authoritative): whether server *announcement* has any main-run-loop dependency in edge cases — this is exactly what Phase 0 smokes. Cosmetic: Resolume's source list shows App Name/Icon from the hosting app; a bare binary presents a degraded identity there until we ship an .app bundle.
- Known issues: tracker has **zero** hits for tearing or gamma/colorspace. #74 (2022): clients can crash when servers exit without `stop` — closed invalid, but cheap insurance: call `stop()` on clean shutdown. #93 (2023, closed): empty `SyphonServerDirectory` — the classic client-side no-run-loop symptom; server-irrelevant for us.

### Resolume ingest

- Current Resolume Arena/Avenue: **7.27.1, released 2026-07-17** (resolume.com/download). Syphon input has existed since Resolume 4.1 and in v7 "Syphon and Spout input are always enabled" — publishers appear automatically in the Sources tab (resolume.com/support/en/syphonspout).
- The exact Resolume version that moved macOS rendering to Metal could **not** be pinned from a primary source (forum 403s our fetches) — but it is **moot**: Syphon shares at the **IOSurface** level and is "interoperable between renderers … OpenGL and Metal … out of the box" (syphon.info). A `SyphonMetalServer` publisher works with any Syphon-capable Resolume regardless of its internal renderer.
- **Color:** the shared surface is `BGRA8Unorm` (not `_sRGB`) and carries no colorspace metadata — bytes are sampled as-is. The de-facto contract: **write display-referred, sRGB-encoded bytes**. The design below does that in hardware (sRGB view on a Unorm texture). Failure mode to watch in Phase 3: washed-out output = double gamma; too-dark = missing encode. Note Resolume 7.24 overhauled its color pipeline / added 10-bit output — untested interaction with 8-bit Syphon input, validate by eye.
- **Alpha:** Syphon carries no straight-vs-premultiplied metadata (a standing ambiguity in the ecosystem — e.g. OBS's open request for a toggle on its Syphon input). Convention is **premultiplied**. Our published frame is an opaque composite (alpha = 1 everywhere), which sidesteps the question for v1; flag it if we ever publish transparent layers.
- **Pacing:** pull-model. Clients call `newFrameImage` at their own rate; latest frame wins; publisher and consumer rates are fully decoupled, and no tearing class of bug exists in the tracker (the completed-handler announce means a client never sees a half-written frame). Resolume wart: "black frame flash when retriggering Syphon" was fixed in 7.21.1 (2024) — require ≥ 7.21.1 in the validation matrix.

### wgpu 29.0.3 (the exact version Bevy 0.19.0 pins in our `Cargo.lock`)

Verified **from the vendored registry sources on this machine** (`wgpu-29.0.3`, `wgpu-hal-29.0.3`, `wgpu-core-29.0.3`), not just docs:

- `as_hal` is **guard-returning** since wgpu 26 (the callback style is gone for resources):
  - `wgpu::Texture::as_hal::<A>(&self) -> Option<impl Deref<Target = A::Texture>>` — unsafe (`wgpu-29.0.3/src/api/texture.rs:61`). The guard holds a device-local destruction read-lock; keep guard scopes tight.
  - `wgpu::Device::as_hal::<A>` / `wgpu::Queue::as_hal::<A>` — same shape (`api/device.rs:578`, `api/queue.rs:339`).
  - `wgpu::CommandEncoder::as_hal_mut::<A, F, R>(&mut self, callback)` **kept the callback shape** (`api/command_encoder.rs:277`), with documented rules: don't end the command buffer (wgpu ends it at `finish()`), don't touch the wgpu encoder while recording via hal.
- **Encoder API pinning (decisive for the sync design):** wgpu-core pins each command encoder to *either* the wgpu encoding API *or* the raw-hal API on first use; mixing **panics** — `"Mixing the wgpu encoding API with the raw encoding API is not permitted"` (`wgpu-core-29.0.3/src/command/mod.rs:547–572`; `as_hal_mut` sets `EncodingApi::Raw` via `record_as_hal_mut`, mod.rs:305–326). **Therefore Bevy's shared `RenderContext` command encoder — which records wgpu passes — can never be used with `as_hal_mut`.** A *dedicated* encoder used only via `as_hal_mut` is legal (`Undecided → Raw`), and the callback receives an **opened** hal encoder (wgpu-core `as_hal.rs:342–366` calls `encoder.open()`), i.e. a live Metal command buffer.
- The wgpu-hal Metal backend is built on **objc2** (migrated in wgpu 29.0.0; `wgpu-hal-29.0.3` depends on `objc2 0.6`, `objc2-metal 0.3`, `objc2-foundation 0.3`, `objc2-quartz-core 0.3` — the old `metal` crate is gone from its graph). Raw handles are objc2 types:
  - `wgpu_hal::metal::Texture::raw_handle(&self) -> &ProtocolObject<dyn MTLTexture>` (`metal/mod.rs:667`).
  - `wgpu_hal::metal::Device::raw_device(&self) -> &Retained<ProtocolObject<dyn MTLDevice>>` (`metal/device.rs:396`); also `texture_from_raw` + `wgpu::Device::create_texture_from_hal` for importing IOSurface-backed textures if we ever need the inverse path.
  - `wgpu_hal::metal::CommandEncoder::raw_command_buffer(&self) -> Option<&ProtocolObject<dyn MTLCommandBuffer>>` (`metal/command.rs:153`). Plain accessor — it does **not** close any internally-open blit encoder; only safe on an encoder where no wgpu-API work was ever recorded (which the API-pinning rule enforces anyway).
  - **`Queue` has NO raw accessor at 29.0.3** (only the `queue_from_raw` constructor; `metal/mod.rs:459–480`). wgpu's CHANGELOG restores `Queue::as_raw` in **29.0.4**. At our pin, wgpu's own `MTLCommandQueue` is unreachable — another reason the dedicated-encoder design below never needs it.
  - Threading: hal `Texture`/`Device`/`Queue` are `unsafe impl Send + Sync`; no thread-pinning documented for the guards. `Queue::submit` commits buffers in order; fences are `MTLSharedEvent`s.
- **The swapchain is not capturable.** Bevy 0.19 configures the surface with `usage: TextureUsages::RENDER_ATTACHMENT` only (`bevy_render-0.19.0/src/view/window/mod.rs:415`) — no `COPY_SRC`, no `TEXTURE_BINDING`; and `Queue::present` consumes the `SurfaceTexture` (hal side owns the `MTLDrawable`). We must publish **a texture we own**. This matches every piece of prior art found.

### Prior art

- **`BlueJayLouche/syphon-rs`** (GitHub; crates.io `syphon-core` / `syphon-metal` / `syphon-wgpu` v0.3.0, published 2026-07-09) — objc2 bindings for Syphon, an IOSurface pool, and a wgpu-29 interop layer. **Targets exactly our wgpu major.** MIT, bundles Syphon.framework (BSD) in-tree. Eleven days old, zero stars: **read it as a worked reference, don't depend on it.** Notably its publish path stalls the CPU with `device.poll(wait)` before blitting on a separate `MTLCommandQueue` — our design below avoids both the stall and the second queue.
- **`mark-ik/wgpu-graft`** (MPL-2.0, active, wgpu 28/29) — imports IOSurface-backed Metal textures *into* wgpu via `texture_from_raw`/`create_texture_from_hal`; useful as a Syphon-*client* reference and a second worked example of the hal import path.
- `raycaster-io/syphon-rs` (crates.io v0.1.1, 2026-04) — repo 404s; unauditable, ignore. No nannou or Bevy Syphon integration exists anywhere (negative result, searched 2026-07-20).

### objc2 + rosc

- objc2 family current versions (crates.io, 2026-07-20): `objc2 0.6.4`, `objc2-foundation 0.3.2`, `objc2-metal 0.3.2` — **exactly the generation already in our `Cargo.lock`** via wgpu-hal 29 (and via our own macOS AVFoundation capture deps in `wc-core/Cargo.toml`). Bind Syphon against objc2 0.6 / objc2-metal 0.3 so `ProtocolObject<dyn MTLTexture>` etc. are *the same types* wgpu-hal hands us — a 0.5-generation binding could not exchange types with wgpu 29. (winit still pulls the 0.5/0.2 generation; both generations already coexist in the lock, that duplication is a fait accompli.)
- Current binding syntax is the `#[unsafe(super(NSObject))]` attribute form of `extern_class!` + per-method `#[unsafe(method(selector:))]` in `extern_methods!` — see FFI surface below.
- **rosc 0.11.4** (2025-03-23, MIT/Apache-2.0) is still the standard Rust OSC crate (nannou_osc and async-osc are both stale wrappers *around* it). 0.11 added `encoder::encode_into(&packet, &mut out)` writing into a caller-provided buffer — satisfies the no-hot-path-allocation rule with a reused `Vec` (building the `OscMessage` itself still allocates `String`/`Vec`; see OSC section for how v1 handles that). Transport is BYO `std::net::UdpSocket`.
- Resolume OSC (manual, resolume.com/support/en/osc): input enabled by default on **port 7000** (changeable); output configurable in Preferences (no fixed default — don't hardcode one); `Shortcuts > Edit OSC` maps any incoming address to any control; native scheme looks like `/composition/layers/1/clips/2/connect` (int 1 to trigger), `/composition/layers/1/video/opacity 0.25`.

---

## Architecture — a frame's journey

```text
 Bevy main world                Bevy render world (per frame)                     Resolume (other process)
 ───────────────                ─────────────────────────────────                 ────────────────────────
 sketch systems ──extract──►  Core2d schedule for the main Camera2d
                              Prepass ▸ MainPass ▸ EarlyPostProcess
                              (Line gravity smear etc.) ▸ PostProcess
                              (bloom ▸ tonemapping ▸ upscaling→swapchain)
                                        │
                                        │ ViewTarget::main_texture()
                                        │ = tonemapped, display-referred,
                                        │   Rgba16Float, UI-free
                                        ▼
                              [SyphonCaptureSet — ours, after PostProcess]
                              fullscreen blit pass: sample main texture,
                              write Bgra8UnormSrgb VIEW of our own
                              persistent Bgra8Unorm texture
                              (hardware linear→sRGB encode on write)
                                        │
                              …render graph ends; Bevy submits the frame's
                              command buffers on wgpu's queue…
                                        │
                              [publish system — after render_system/submit]
                              dedicated wgpu CommandEncoder (raw-API-only):
                              as_hal_mut → raw MTLCommandBuffer →
                              SyphonMetalServer publishFrameTexture:
                                 (our texture, that cmd buffer,
                                  full region, flipped:NO)
                              → queue.submit([enc.finish()])
                                        │  same MTLCommandQueue ⇒ executes
                                        │  strictly after the capture blit;
                                        │  Syphon's addCompletedHandler
                                        │  announces the frame when the
                                        ▼  GPU actually finished it
                              IOSurface (zero-copy, cross-process)  ────────►  Sources tab: "WaveConductor"
                                                                              sampled at Resolume's own fps
 main world OSC systems  ◄──────────── UDP 7000 / user port ────────────────►  OSC in/out (cue sketches,
 (rosc encode_into, reused buffers)                                            receive beat/energy telemetry)
```

What is and is not in the feed, by construction:

- **In:** everything the main `Camera2d` renders — the sketch, its post-process chain, bloom, and the operator-selected tonemap (per-sketch `TonemapChoice`, `wc-core/src/render/mod.rs:36–59`; camera default is `Tonemapping::None`/SDR, `waveconductor/src/main.rs:314–318`).
- **Out, deliberately:** the egui settings dock (bevy_egui renders in its own pass against the window, after the cameras — the VJ feed must never show the operator UI).
- **Out, v1 limitation:** the Line hand-mesh overlay (`wc-sketches/src/hand_mesh/mod.rs` spawns a separate HDR `Camera3d` on Core3d whose output composites onto the swapchain — which we cannot read). For the Resolume performance use case (Radiance / body sketches) this doesn't matter: Radiance renders entirely on the main Camera2d (`wc-sketches/src/radiance/render.rs:48`). Full-composite capture is a listed follow-up, not v1.

## Capture pass design

Model: `crates/wc-sketches/src/line/post_process.rs` — the house reference for a render *system* (Bevy 0.19 has no ViewNode graph nodes here; post-processing is systems in `Core2dSystems` sets, `line/post_process.rs:136–139`). `Core2dSystems` is exactly `{Prepass, MainPass, EarlyPostProcess, PostProcess}` chained (`bevy_core_pipeline-0.19.0/src/schedule.rs:86–105`).

- **Position:** a new public set, configured after Bevy's:

  ```rust
  render_app.configure_sets(Core2d, SyphonCaptureSet.after(Core2dSystems::PostProcess));
  render_app.add_systems(Core2d, syphon_capture.in_set(SyphonCaptureSet));
  ```

  After `PostProcess`, `ViewTarget::main_texture()` holds the final tonemapped, display-referred image (upscaling has already blitted it to the swapchain, but the main texture still holds it, in linear-light Rgba16Float — the sRGB encode happens only at the swapchain's sRGB view). Gate on `ExtractedCamera::hdr` exactly as `line_post_process` does (`post_process.rs:315–329` — the `Hdr` marker is not extracted in 0.19).
- **Target:** a persistent texture we own, render-world resource:
  - format `Bgra8Unorm` (Syphon's wire format ⇒ blit fast path), `view_formats: &[Bgra8UnormSrgb]`, usage `RENDER_ATTACHMENT | COPY_SRC`.
  - The pass renders through the **sRGB view**, so the hardware performs the linear→sRGB encode on write and the underlying Unorm bytes are display-ready — the exact contract Resolume expects. `texture.format()` still reports `Bgra8Unorm`, so Syphon's format-match fast path holds.
  - Size = the camera's target size; **reallocate only on resize** (bounded by construction). The capture shader is a trivial fullscreen-triangle sample-and-write (`assets/shaders/output/syphon_capture.wgsl` — per house rules, no inline WGSL).
- **Caching discipline** (AGENTS.md render-world rules, all mirrored from `line/post_process.rs`):
  - Bind groups: the two-slot `[Option<(TextureViewId, BindGroup)>; 2]` cache validated against the ping-pong source view's own id (`post_process.rs:321, 356–407`). Never key on window size.
  - The uniform-free pipeline is even simpler than Line's (no params buffer needed; the only binding is texture+sampler).
  - **Removal companion:** the enable flag arrives in the render world via `ExtractResourcePlugin`, which does not propagate removals — ship `remove_syphon_params_if_absent` exactly like `remove_line_post_params_if_absent` (`post_process.rs:282–290`). The GPU texture + server handle live in a render-world resource torn down by an explicit disable system (mechanism 2 in AGENTS.md's three-way lifetime taxonomy), and `SyphonMetalServer.stop()` is called on teardown and on app exit.
- **Idle:** when the feature is enabled the publish keeps running in every `SketchActivity` (a VJ feed that dies when the attract screensaver starts is broken — same sanctioned-exception reasoning as the resize listeners). When the setting is **off**, both systems early-return on an absent resource: true zero work.
- **FPS cap:** the capture+publish pair optionally skips frames against a wall-clock accumulator (settings below) so a 120 Hz ProMotion window doesn't publish 120 fps at a 60 Hz Resolume composition for no benefit.

## Publish + synchronization design

**Chosen: Option A — dedicated raw-API wgpu command encoder, submitted on wgpu's own queue.**

```rust
// Render-world system, ordered after Bevy's render_system (i.e. after the frame's
// main submission). Simplified; error paths elided.
fn syphon_publish(state: Option<ResMut<SyphonPublishState>>, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let Some(mut state) = state else { return };            // feature off / not macOS
    let mut encoder = device.wgpu_device()
        .create_command_encoder(&CommandEncoderDescriptor { label: Some("syphon_publish") });
    // SAFETY: this encoder records via the raw hal API only (EncodingApi::Raw);
    // it never touches the wgpu encoding API, so the wgpu-core pinning rule is satisfied.
    unsafe {
        encoder.as_hal_mut::<wgpu::hal::api::Metal, _, _>(|hal_enc| {
            let hal_enc = hal_enc.expect("Metal backend");   // documented invariant on macOS
            let cmd_buf = hal_enc.raw_command_buffer().expect("opened by as_hal_mut");
            let tex_guard = state.texture.as_hal::<wgpu::hal::api::Metal>().expect("Metal");
            objc2::rc::autoreleasepool(|_| {
                state.server.publish_frame_texture(
                    tex_guard.raw_handle(), cmd_buf, state.region, /* flipped: */ false);
            });
            // guards drop here — before finish/submit, keeping destruction-lock scopes tight
        });
    }
    queue.submit([encoder.finish()]);
}
```

Why this is correct, point by point:

1. **API pinning:** the encoder is used exclusively through `as_hal_mut`, so it pins to `EncodingApi::Raw` — no panic (`wgpu-core command/mod.rs:562–572`). Bevy's shared `RenderContext` encoder is *never* touched (it is pinned `Wgpu`; touching it with `as_hal_mut` panics — this is the single sharpest edge in the whole design and the reason the publish is not encoded inline in the capture system).
2. **Ordering:** the capture blit was submitted inside Bevy's frame submission on wgpu's internal queue; our publish buffer is submitted on the *same* `wgpu::Queue` afterwards. Metal executes command buffers on one `MTLCommandQueue` in commit order ⇒ the Syphon copy always reads a fully-written capture texture. No `MTLEvent`, no second queue, no `device.poll(wait)` CPU stall (the prior-art crate's weakness), no cross-queue hazard.
3. **Announcement timing:** Syphon's `addCompletedHandler` is attached inside `publishFrameTexture` (verified in `SyphonMetalServer.m`), i.e. before wgpu commits at `submit` — legal (handlers must be added before commit) and it means Resolume is notified exactly when the GPU finishes our copy. End-to-end latency: same frame, ~zero.
4. **Scheduling:** the system runs in the `Render` schedule ordered after Bevy's graph-runner/submit system, in the render app. (Exact anchor to pin during Phase 1: the system Bevy 0.19 exposes for the graph run — `render_system` in `bevy_render::renderer` — or `RenderSystems::Cleanup` if ordering against it directly proves awkward.)
5. **Allocation audit of the steady-state path:** one `CommandEncoder` per publish (wgpu-idiomatic; Bevy itself creates encoders every frame), the `NSRect`/region is a stored plain struct, the server + NSString name are created once at enable, bind groups are cached (capture side). The autoreleasepool bounds any ObjC temporaries. No per-frame Rust heap allocation.

**Failure modes, named:**

| Failure | Cause | Mitigation |
| --- | --- | --- |
| Panic: "Mixing the wgpu encoding API…" | any wgpu-API call on the publish encoder (even a debug marker) | encoder is created, `as_hal_mut`'d, finished — three lines, reviewed as an invariant |
| Deadlock/stall on `Texture::as_hal` guard | holding the guard across `submit` or across a texture `destroy` | guard scopes end inside the callback, before `finish()` |
| Publish reads stale/torn frame | publishing on a queue other than wgpu's | design uses wgpu's queue only; the raw `MTLCommandQueue` isn't even reachable at 29.0.3 |
| Frame announced before GPU done | committing the buffer without Syphon's handler attached | `publishFrameTexture` is called strictly before `submit` |
| Slow conversion path inside Syphon | non-BGRA8 texture, or `flipped:YES` | we publish `Bgra8Unorm` + `flipped:NO`; wgpu/Metal are both top-left origin (verify visually in Phase 3; the flip flag exists for GL-origin publishers) |
| Resource leak on disable | render-world resources are not entity-owned | explicit disable system drops texture/server + calls `stop()`; `remove_*_if_absent` companion for the extracted settings |

**Fallback (Option B), only if A hits an unknown:** create our own `MTLCommandQueue` from `hal::Device::raw_device()`, gate the publish on `wgpu::Queue::on_submitted_work_done` (fires when the prior submit's GPU work completes; callback is `Send + 'static`, Syphon is documented thread-safe). Costs up to a frame of latency and a callback hop; requires no wgpu version change. Do **not** copy prior art's `poll(wait)`.

## FFI surface (objc2)

New macOS-only module; realistic signatures (selector names verified against the upstream headers 2026-07-20; compile-verify on the Mac — the `NSRect`/`CGRect` re-export location in the 0.3 generation is the one thing to double-check):

```rust
// wc-core/src/output/syphon/platform/macos.rs  (sketch)
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{extern_class, extern_methods};
use objc2_core_foundation::CGRect;              // NSRect == CGRect on 64-bit
use objc2_foundation::{NSDictionary, NSObject, NSString};
use objc2_metal::{MTLCommandBuffer, MTLDevice, MTLTexture};

extern_class!(
    /// `SyphonMetalServer : SyphonServerBase : NSObject`. We bind straight to
    /// NSObject; SyphonServerBase's surface isn't needed.
    #[unsafe(super(NSObject))]
    #[name = "SyphonMetalServer"]
    pub struct SyphonMetalServer;
);

extern_methods!(
    impl SyphonMetalServer {
        #[unsafe(method(initWithName:device:options:))]
        pub unsafe fn init_with_name_device_options(
            this: Allocated<Self>,
            name: Option<&NSString>,
            device: &ProtocolObject<dyn MTLDevice>,
            options: Option<&NSDictionary<NSString, AnyObject>>,
        ) -> Retained<Self>;

        #[unsafe(method(publishFrameTexture:onCommandBuffer:imageRegion:flipped:))]
        pub unsafe fn publish_frame_texture(
            &self,
            texture: &ProtocolObject<dyn MTLTexture>,
            command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
            image_region: CGRect,
            flipped: bool,
        );

        #[unsafe(method(stop))]
        pub unsafe fn stop(&self);
    }
);
```

The `MTLDevice` handed to `init` **must be wgpu's own** — obtained once at enable via `device.as_hal::<Metal>() → raw_device()` — so the published texture and the server share a device.

**Linking.** `build.rs` (macOS + `syphon-output` feature only):

```rust
println!("cargo:rustc-link-search=framework={manifest_relative_vendor_dir}");
println!("cargo:rustc-link-lib=framework=Syphon");
println!("cargo:rustc-link-arg=-Wl,-rpath,{runtime_framework_dir}");
```

with the framework **vendored** at `vendor/syphon/Syphon.framework` (built once from source with Xcode on the Mac, committed like `vendor/libdev` — the OBSBOT SDK precedent; ~1 MB; `check-secrets` already exempts `vendor/`). This keeps CI's macOS `--all-features` job linking without network access, and no developer-machine path ever appears in the tree (the search path is derived from `CARGO_MANIFEST_DIR` at build time). The rpath baked for dev runs points at the vendored dir; the eventual .app/DMG switches to `@executable_path/../Frameworks` with the framework copied into the bundle (standard dyld practice), which also upgrades the App Name/Icon Resolume displays.

This repo already planned "native via objc2" for IOKit UVC control (tracking doc §A1), and `wc-core` already carries target-gated `objc2`/`objc2-foundation` optional deps for AVFoundation capture (`wc-core/Cargo.toml`) — Syphon adds `objc2-metal` + `objc2-core-foundation` to that existing pattern, plus a direct `wgpu` dependency (version-pinned to the workspace lock; zero new supply chain, it's already in-tree) for the `wgpu::hal` types.

## Module layout, feature gating, CI safety

Model: `input/obsbot/platform/` (`platform/mod.rs:1–31` — real backend on one OS, stub everywhere else, both exporting the same names) and `lifecycle/thermal/platform/`.

```text
crates/wc-core/src/output/
├── mod.rs              // module docs: the external-output layer (Syphon now; Spout/NDI later)
├── syphon/
│   ├── mod.rs          // plugin, settings struct, main-world systems, data-flow docs
│   ├── capture.rs      // render-world: SyphonCaptureSet blit pass (portable wgpu code)
│   ├── publish.rs      // render-world: the raw-encoder publish system (macOS-real, stub no-op elsewhere)
│   └── platform/
│       ├── mod.rs      // cfg(target_os = "macos") ⇒ macos, else stub; same exported names
│       ├── macos.rs    // objc2 SyphonMetalServer bindings + safe wrapper (init/publish/stop)
│       └── stub.rs     // SyphonServerHandle::new() -> None; compile-checked by --all-features everywhere
└── osc/                // separate module, separate feature — see below
```

- **Feature `syphon-output` on `wc-core`, NOT in `default`.** The `waveconductor` binary enables it from a macOS target table (exactly how `thermal-sensor-macos` is wired in `waveconductor/Cargo.toml`). CI `--all-features` on Linux/Windows compiles the stub (nothing links); on macOS it links the vendored framework. Never anywhere near `bevy/dynamic_linking` (alias-only, per AGENTS.md).
- The capture pass in `capture.rs` is portable wgpu and compiles on every platform (it just has no consumer when the platform handle is `None` — the enable system refuses to insert the render-world state, so idle cost off-macOS is literally zero).

## Settings surface

`SketchSettings`-derive struct following `ObsbotSettings` (`input/obsbot/mod.rs:176–194`; registered via `register_sketch_settings` + `register_dock_section`, mod.rs:339–354):

```rust
#[derive(SketchSettings, Resource, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[reflect(Resource, Default)]
#[settings(storage_key = "video_output")]
pub struct VideoOutputSettings {
    /// Publish the rendered frame as a Syphon server (macOS; no-op elsewhere).
    #[setting(default = false, ty = Boolean, category = User, section = "Video Output",
              label = "Syphon output (Resolume/VJ feed)")]
    pub syphon_enabled: bool,

    /// Publish rate cap. 0 = every rendered frame.
    #[setting(default = 60.0_f32, min = 0.0, max = 120.0, step = 1.0, unit = "fps",
              category = User, section = "Video Output", label = "Publish rate cap")]
    pub fps_cap: f32,
}
```

- **Server name**: `"WaveConductor"` constant in v1. The reflected panel drives booleans/sliders/enums; free-text fields are unproven there, and a VJ has no reason to rename the server mid-show. If needed later it becomes a TOML-persisted field edited in the settings file (`settings/persistence.rs` machinery), not a UI control.
- A custom dock section under the toggle (the `section.rs` render-only pattern, `input/obsbot/section.rs:27`) shows live status: "publishing 1920×1080 @ 60 — 1 client" (`hasClients`) / "Syphon requires macOS" on other platforms — the operator's on-site instrument for "why is Resolume not seeing it."
- Enable/disable applies live: an `apply_video_output_settings` main-world system inserts/removes the main-world marker resource; extraction + the removal companion propagate it to the render world.

## OSC companion (`osc-bridge` feature — separate from `syphon-output`)

Independent feature, independent module (`output/osc/`), usable without video (e.g. scope-fork (b) later) and on any OS. `rosc 0.11.4`, plain `UdpSocket` (nonblocking send; a 1536-byte recv buffer drained by a `PreUpdate` listener system — the cheap-message-drain pattern the OBSBOT worker uses, and `ProviderId::WebSocket` is the existing external-I/O precedent per the tracking doc).

**v1 address space** — deliberately minimal, `/wc` namespace, all values normalized so Resolume-side mapping is a straight `Shortcuts > Edit OSC` drag:

| Direction | Address | Args | Semantics |
| --- | --- | --- | --- |
| out | `/wc/status/sketch` | `s` | current sketch name; sent on change + every 2 s keepalive |
| out | `/wc/audio/energy` | `f` 0..1 | smoothed audio drive (the room-calibrated envelope Radiance already computes) |
| out | `/wc/audio/beat` | `f` 0..1 | beat impulse; strength as float (Resolume maps it to a trigger/param) |
| out | `/wc/body/count` | `i` | tracked-body count |
| out | `/wc/body/1/centroid` | `ff` 0..1 | primary body centroid, normalized (multi-body indices reserved) |
| in | `/wc/sketch` | `s` | switch sketch by name (drives the existing sketch-cycle machinery) |
| in | `/wc/attract` | `i` 0/1 | force/exit attract mode |
| in | `/wc/setting/<storage_key>/<field>` | `f` | clamped settings nudge through the existing reflected-settings write path |

- **Telemetry rate:** 30 Hz cap for continuous lanes, event-driven for beats/sketch — OSC is UDP; nothing here needs reliability, latest-wins.
- **Allocation:** pre-render the fixed address strings once; reuse one `Vec<u8>` via `encode_into` per send. The per-message `OscMessage { args: Vec }` construction allocates a few small vecs on the **main-world** system at 30 Hz — acceptable off the render/audio paths, but pre-built skeletons are a cheap follow-up if profiling cares.
- Ports: send target host:port and listen port live in the same `VideoOutputSettings`-style struct (defaults: send `127.0.0.1:7000` — Resolume's default input; listen `7001`). Resolume's outgoing port is user-configured; the runbook (Phase 3 deliverable) documents pointing it at us.
- v1 sends into the generic `/wc` namespace and lets Resolume map. Directly addressing Resolume's native scheme (`/composition/layers/N/clips/M/connect`) from WaveConductor is possible but is show-file-specific — that's operator config, not app code.

## Licensing / attribution

- **Syphon.framework: BSD 3-clause-style** (`License.txt`, Butterworth & Marini). Vendoring the framework and shipping it in a DMG are both redistribution ⇒ (a) commit upstream `License.txt` alongside the vendored framework (`vendor/syphon/License.txt`), (b) reproduce the full notice in our shipped credits/acknowledgements (same surface that will carry the OBSBOT SDK note; the vendored-SDK license caveat memory entry applies here too), (c) no "Syphon Project" endorsement implications in marketing copy. Register in `deny.toml` only if a Rust crate enters the graph — the framework itself is outside cargo's view; the objc2/rosc crates are MIT / MIT-or-Apache, already-allowed license classes.
- rosc: MIT OR Apache-2.0 — standard.

## Phased implementation plan (macOS session)

**Phase 0 — spike: prove the pipe (½ day).**
Build `Syphon.framework` from current source with Xcode. Write a throwaway 30-line Obj-C or Swift CLI (not in-repo, or under a gitignored `spikes/`) that creates a `SyphonMetalServer` on the system default `MTLDevice`, publishes a solid-color BGRA8 texture at 60 Hz from a plain process (no app bundle), with **no run loop pumped**.
*Accept:* the "Simple Client" example app (or Resolume) sees the server and the color; note whether announcement required a pumped run loop (the one server-side unknown). Vendor the built framework + `License.txt` under `vendor/syphon/`.

**Phase 1 — static publish from WaveConductor (1 day).**
`output/syphon/` skeleton: feature, platform facade, objc2 bindings, build.rs linking, settings toggle. Publish a **fixed test-pattern texture** (owned `Bgra8Unorm`, cleared to a gradient once at enable) via the Option-A raw-encoder path each frame.
*Accept:* toggling the setting starts/stops a "WaveConductor" server visible in Resolume showing the pattern; toggling off tears down cleanly (server disappears, no `Box::leak`-class residue — verify with the removal-companion checklist); `cargo clippy --all-targets --all-features` and the full gate pass on the Mac; `--all-features` still compiles on Linux/Windows CI (stub).

**Phase 2 — live frames (1–1.5 days).**
The `SyphonCaptureSet` blit pass off `ViewTarget::main_texture()` with the two-slot bind-group cache; resize-driven reallocation; fps cap; the dock status section.
*Accept:* Resolume shows live Radiance/Line/Dots output matching the window (minus UI and hand-mesh overlay, as designed); resolution follows a window resize; `cargo xtask capture` scenarios unaffected; a 30-minute run with Activity Monitor + a Metal frame capture shows flat RSS **and flat VRAM/IOSurface counts** across enable/disable cycles and sketch transitions (the soak harness is blind to GPU memory — this manual watch is the gate).

**Phase 3 — Resolume validation matrix + OSC v1 (1 day).**
Resolume Arena ≥ 7.21.1 on the deployment Mac: color check (gray ramp + saturated test frame vs the app window — catching double/missing gamma), orientation (`flipped:NO` correct?), pacing (app at 60/120 Hz vs composition at 30/60 — no stutter/tear), retrigger (clip re-connect — no black flash on ≥ 7.21.1), server lifecycle (app relaunch while Resolume holds the source). Then the `osc-bridge` v1 lanes and a Resolume mapping smoke (map `/wc/audio/beat` to a clip trigger; cue `/wc/sketch` from Resolume's shortcuts).
*Accept:* a written validation table in this doc's follow-up (per-check pass/fail + Resolume version); a one-page operator runbook (`docs/runbooks/resolume-output.md`) covering ports, source naming, and the color check.

Total: ~4 days of macOS sessions, each phase independently landable behind the off-by-default feature.

## Open questions / risks (ranked; first thing to check on the Mac)

1. **[blocking-unknown] Server announcement without a pumped run loop / from a winit app.** Evidence says servers don't need the run loop (only directory *clients* do), and winit pumps the main NSRunLoop anyway — but no authoritative statement exists. *Phase 0 exists to answer exactly this.*
2. **[design-risk] The Option-A raw-encoder path is source-verified but not execution-verified.** The `EncodingApi` pinning, opened-encoder guarantee, and `raw_command_buffer` liveness were all read from wgpu 29.0.3 sources on this box, but nobody has run this exact pattern (prior art used the separate-queue+stall pattern). *First Mac check: Phase 1's static publish with Metal API validation enabled (`MTL_DEBUG_LAYER=1`).* Fallback Option B is specified and requires no redesign.
3. **[verify-by-eye] Color/gamma contract.** sRGB-view hardware encode should be exactly right; Resolume 7.24+'s new color pipeline is the wildcard. Phase 3's gray-ramp check is the arbiter; the fix, if wrong, is one flag (drop the sRGB view / encode in the capture shader).
4. **[verify-by-eye] Orientation.** `flipped:NO` is almost certainly correct (both wgpu and Metal are top-left origin; the flag exists for GL publishers) — one glance in Phase 3.
5. **[scoped-out gap] Hand-mesh overlay + any second camera are absent from the feed** (swapchain is `RENDER_ATTACHMENT`-only, `bevy_render 0.19 view/window/mod.rs:415`). Fine for the body-sketch performance path. The clean fix if ever needed: retarget cameras to an owned offscreen `RenderTarget` and present via a final blit — a real refactor, listed as future work, or override the per-target `OutputColorAttachment` via `ViewTargetAttachments` (`bevy_render view/mod.rs:708–711`) plus a present blit, which is what Bevy's own screenshot path does transiently.
6. **[version note] `wgpu::Queue::as_raw` returns in 29.0.4** (semver-patch). The design doesn't need it; if Option B is ever taken, prefer bumping the patch over holding a second device queue by other means. Watch Bevy 0.19.x lockfile drift.
7. **[maintenance] Vendored framework staleness.** Syphon development is slow and stable; re-vendor on upstream releases only. The 2025-10 BGRA pinning commit is already in what Phase 0 builds.
8. **[cosmetic] Bare-binary identity in Resolume's source list** (generic App Name/Icon) until the .app/DMG packaging phase.
9. **[deferred] OSC settings-nudge address hardening** — `/wc/setting/...` writes through the reflected-settings path with clamping; the listen socket should bind localhost by default and the runbook should say so (a venue LAN is a hostile network for an unauthenticated UDP control port).
