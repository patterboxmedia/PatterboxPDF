# Native Linux worker boundary

This internal crate runs one private resource worker on qualified x86_64 Ubuntu
Linux. Native WSL2 execution qualifies the recorded kernel and runtime; it does
not qualify every distribution, architecture or bare-metal host.

`SandboxProfile::new()` requires Landlock ABI 3 or later and pins the exact
Ubuntu loader, libc, libm, libgcc and loader-cache objects. Callers grant only
host-owned staged inputs, pre-created outputs and private work. Leaf grants
authorize the pinned object; replacing its path does not authorize a new
object. Directory grants are recursive: never grant a shared parent directory.

Pass exact verified worker bytes to `PinnedExecutable::from_bytes`, then use
`spawn_pinned`. `spawn` is the convenience form for a host-owned private path.
Both execute a sealed memfd. Staging, digest verification, total protocol/output
size bounds, report checks, prepared-resource receipts and directory cleanup
remain the calling boundary's responsibility.

Before the initial ELF instruction, the launcher applies Landlock, no-new-privs,
an architecture-checked seccomp allowlist, hard resource limits, explicit piped
stdio, closure of unrelated inherited descriptors and an environment containing
only `LANG` and `TMPDIR`. All process/thread creation is denied. Current font,
color and raster workers have no internal threads; host I/O reader threads run
outside this boundary. Future internal threading needs a verified policy change.

`process_memory_bytes` bounds virtual address space. The retained compatibility
field `job_memory_bytes` must be at least that bound and also bounds each
regular output file. One worker comprises the whole job. CPU time rounds up to
whole kernel seconds. A dedicated owner enforces wall time and captured run
cancellation independently of caller polling. Pipes have bounded kernel
capacity; callers still enforce their existing total response-size limits.

`clone3(CLONE_PIDFD)` atomically creates the worker and stable process handle.
The child waits at a startup barrier for ownership. The owner thread remains
alive until reaping, so Linux `PDEATHSIG` follows the correct thread lifetime.
Drop, cancellation and wall expiry kill via pidfd and reap. Forced host death
causes kernel-enforced worker death. Missing primitives fail closed; there is
no unconfined retry or weaker fork fallback.

Initial `execveat` uses sealed descriptor 64, just outside the hard 64-descriptor
limit. Bootstrap copies start at 66; an inherited limit of 73 or more can hold
them, including the usual 1024 limit. An insufficient budget fails explicitly.
The executable closes on exec; later open/dup cannot recreate it, hard limits cannot
be raised and other exec is denied. The policy also denies sockets, namespace
changes, ptrace, io_uring, asynchronous I/O ownership signals and metadata
mutation syscalls that Landlock ABI 3 does not mediate.

From the workspace in native Ubuntu, use a fresh private target:

```sh
CARGO_TARGET_DIR=/tmp/pdf-library-u4-target cargo test --offline --locked --release -p publisher-linux-worker-job -- --nocapture --test-threads=1
CARGO_TARGET_DIR=/tmp/pdf-library-u4-target cargo clippy --offline --locked -p publisher-linux-worker-job --all-targets -- -D warnings
cargo fmt --package publisher-linux-worker-job -- --check
```

The eleven focused tests exercise real kernel denials, executable/object
replacement, incompatible syscall ABIs, memory/CPU/wall limits, blocked stdout,
unrelated descriptor/pipe closure, cancellation, Drop, forced host death and
inherited primitive failures before parsing. They also require timestamp denial
for read-only/ungranted objects and launch under a 1024 descriptor limit.
The owner retains the wall-deadline reason after reaping, allowing preparation
boundaries to preserve their typed timeout errors. Unsupported features fail the
suite; no confinement result is mocked or skipped.

Kernel contracts: [Landlock ABI 3](https://www.kernel.org/doc/html/v6.6/userspace-api/landlock.html)
and [seccomp filters](https://docs.kernel.org/userspace-api/seccomp_filter.html).
