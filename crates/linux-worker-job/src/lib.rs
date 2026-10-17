//! Narrow native Linux boundary for private resource workers.
//!
//! Qualified on `x86_64` Linux with Landlock ABI >= 3, seccomp, `clone3` pidfds,
//! sealed memfds and `close_range`. Setup errors never retry outside confinement. The
//! caller owns staging, hash verification, protocol/output bounds and cleanup.
//! Grants name pinned filesystem objects, not paths resolved again at launch.
#![cfg(target_os = "linux")]

#[cfg(target_arch = "x86_64")]
mod policy;
#[cfg(target_arch = "x86_64")]
mod process;
#[cfg(target_arch = "x86_64")]
pub use process::{
    JobLimits, PinnedExecutable, SandboxProfile, SandboxedProcess, WorkerCancellationScope,
};

// Other architectures deliberately have no launch implementation. Consumers
// must keep their unsupported-platform branch rather than launch unconfined.
