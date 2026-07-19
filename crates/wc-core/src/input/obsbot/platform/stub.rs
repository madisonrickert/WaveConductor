//! No-op OBSBOT backend for platforms without the libdev link (everything
//! but Windows). Exists so the `obsbot-camera-control` feature — which CI's
//! `--all-features` switches on for every runner — builds and tests green
//! without a C++ toolchain, the vendored `libdev` binary, or an OBSBOT
//! plugged in. See `platform/mod.rs` for the facade contract.

use super::{ObsbotStatus, WorkerCommand};

/// Facade twin of `windows::WorkerHandle`. Never constructed —
/// [`spawn_worker`] always returns `None` — but the type must exist so
/// `ObsbotControl` and the systems in `obsbot::mod` compile unchanged.
pub struct WorkerHandle {
    /// Uninhabitable: guarantees this stub is never instantiated.
    never: std::convert::Infallible,
}

impl WorkerHandle {
    /// Facade method; unreachable because the stub handle cannot exist.
    pub fn send(&self, _cmd: WorkerCommand) -> bool {
        match self.never {}
    }

    /// Facade method; unreachable because the stub handle cannot exist.
    pub fn try_recv_status(&self) -> Option<ObsbotStatus> {
        match self.never {}
    }
}

/// No real backend on this platform: report "no worker" so
/// `ObsbotControl.status` stays [`ObsbotStatus::NoDevice`] forever.
pub fn spawn_worker(_take_control: bool) -> Option<WorkerHandle> {
    None
}
