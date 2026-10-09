# Threat model: K0

K0 is a capability-based microkernel for AArch64, written in Rust `no_std` in the style of seL4 and licensed AGPL-3.0. The kernel runs at EL1. The only user-space code today is the root task, which runs at EL0 and doubles as the self-test harness. Supported platforms are QEMU `virt` (4 KiB granule, the only platform that can be exercised in this image) and Apple Silicon via m1n1 (16 KiB granule, compile-only here).

Read `SECURITY.md` at the repository root first. It describes the isolation model of version 0.1.0 and the issues that are already known.

## What the kernel does and where untrusted input enters

Trust boundaries, from least to most trusted:

1. **EL0 code via system calls (`svc`).** Every register a task passes to the kernel is attacker controlled: capability slot indices, virtual addresses, permission bits, sizes, object types, badges and message words. This is the primary attack surface. Entry path: `crates/k0-arch/src/vectors.S` (`__lower_common`) -> `crates/k0-arch/src/usermode.rs` (`el0_sync`) -> `kernel/src/main.rs` (`k0_syscall` and the `sys_*` functions).
2. **EL0 faults.** Data and instruction aborts, undefined instructions, trapped system register and FP/SIMD accesses, `brk` and alignment faults all reach `k0_fault` in `kernel/src/main.rs`, which must only kill the faulting task.
3. **Timer interrupts** taken at EL1 and from EL0 (`irq_current`, `irq_lower`), which drive preemption in `crates/k0-sched`.
4. **The device tree blob (DTB)** that the boot chain passes in `x0`. It is parsed before the MMU is on by `crates/k0-boot/src/fdt.rs` and used to build the physical memory map (`check_memory_map` in `kernel/src/main.rs`, `k0_cap::bootstrap`). The boot chain is trusted to load an authentic kernel image, but the DTB parser must still be total: no panic, no out-of-bounds read, no integer overflow on any input.
5. **The root task image** is embedded in the kernel at build time together with its SHA-256 (`crates/k0-boot/build.rs`, `verify_root_task`). It is part of the trusted computing base today. The hash detects corruption, not a malicious image; signature verification is a documented TODO.

Out of the kernel's control and therefore trusted: the boot chain (m1n1, QEMU firmware), the integrity of the kernel image itself, and the hardware.

## Components that matter most and least

Highest value, all in scope:

- `kernel/src/main.rs`: system call dispatch, every capability lookup and permission check, `write_bootinfo`, `k0_fault`, memory map validation.
- `crates/k0-cap`: the root CNode, `Untyped` accounting and `retype`, bootstrap of the memory regions.
- `crates/k0-mm`: page table construction, the table pool (`Pool::map_page` and its rollback), W^X enforcement, user mapping validation.
- `crates/k0-arch`: exception vectors and user context save/restore (`vectors.S`, `usermode.rs`), `boot.S`, hardening (`hardening.rs`: PAC, BTI, PAN, `CPACR_EL1`), paging switch-on, cache and TLB maintenance.
- `crates/k0-ipc`: endpoint state machine, send, receive, call, reply, `abort_reply`.
- `crates/k0-sched` and `crates/k0-task`: run queue, `kill_current`, TCB state.
- `crates/k0-boot`: FDT parser, memory map, root task flattening and verification at build time and at boot.
- `crates/k0-abi`: system call numbers and ABI structures shared with user space.

Lower value:

- `userspace/root-task`: the trusted root task and test harness. Bugs here are not kernel vulnerabilities, but it is the right place to write reproducers.
- `vendor/`: third-party crates vendored with pinned checksums. Only `sha2` and its dependencies are linked into the kernel, for the boot-time hash of the fixed root task image. Report issues there only if reachable through that use; generic upstream findings are out of scope.
- `tools/qemu-virt-runner.sh`, `kernel/build.rs`, `crates/k0-boot/build.rs`, `userspace/root-task/build.rs`: developer tooling that runs on the trusted build host.
- Documentation, `.idea/`, `.claude/`.

## How to exercise it

- Build: `cargo build -p k0-kernel --offline` (QEMU virt, dev profile with debug info; `panic = "abort"` in every profile). The Apple Silicon configuration is `cargo build -p k0-kernel --release --no-default-features --features plat-apple --offline` and only compiles here.
- Run: `tools/qemu-virt-runner.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel`, which is what `cargo virt` (alias of `cargo run -p k0-kernel`) does. It converts the ELF to a raw image with `llvm-objcopy` and boots `qemu-system-aarch64 -machine virt,gic-version=3 -cpu cortex-a72 -smp 1 -m 512M -nographic`. The kernel never exits: once the root task finishes its self-tests it yields forever and the kernel prints `k0: tick N` once a second. Wrap every run in `timeout`, or use `tools/qemu-boot-test.sh`, which checks the expected log lines in order and stops QEMU once they appear.
- A healthy boot log is in `/src/bootcheck.log`, captured when this image was built. The lines that matter are `k0: w^x checks pass`, `k0: user w^x checks pass`, `root: retype/map tests pass`, `root: child exit test pass`, `root: ipc tests pass`, `root: fault isolation tests pass`, `root: preempt test pass` and `root: sched tests pass`.
- Reproducers: the root task (`userspace/root-task/src/main.rs`) is the only EL0 code and already contains the test harness and thin `sys*` wrappers around `svc`. Add a test there, spawning a child task with `TCB_CONFIGURE` and `TCB_RESUME` when the reproducer needs its own thread, rebuild and run. `crates/k0-boot/build.rs` rebuilds and re-embeds the root task automatically.
- Debugging: append `-s -S` to the runner (`tools/qemu-virt-runner.sh <elf> -s -S`) and attach with `gdb-multiarch target/aarch64-unknown-none-softfloat/debug/k0-kernel -ex 'target remote :1234'`. `-d int,guest_errors` prints exception traces. `llvm-objdump -d` (from the pinned toolchain, linked into `/usr/local/bin`) disassembles both images.
- Fault semantics: an EL0 fault prints `k0: task fault <name> (esr .. elr .. far ..), killing task` and kills only that task. A kernel-internal exception goes to `exception_fatal`, which halts the machine on purpose (fail-secure). Reaching that halt from EL0 is a finding.

## How we rate severity

Rate by what EL0 code can do to EL1, not by what one task can do to another (see "Anything to leave alone").

- **Critical**: any EL0 -> EL1 privilege escalation. Executing attacker-chosen code at EL1; reading or writing kernel memory or arbitrary physical memory through a system call; obtaining a mapping, capability or object that the caller's slot arguments do not grant (bypassing a capability or permission check); mapping memory that is both writable and executable for EL0, or kernel memory into TTBR0; getting the kernel to restore EL1 state (mode, exception level, interrupt masks) that came from user input.
- **High**: memory-safety bugs in kernel code reachable from EL0, with or without a demonstrated exploit (out-of-bounds index, integer overflow or truncation in size or address arithmetic, stale raw pointers, aliasing `&mut`, unsound `unsafe`); a system call or fault sequence that halts the whole system (kernel panic, `exception_fatal`, an unbounded loop with interrupts masked, or a scheduler with no runnable task outside the known IPC case); missing TLB or cache maintenance that lets a task keep using memory after it was unmapped or retyped; kernel register or memory contents leaking to EL0 (registers not restored or cleared on the return path, uninitialised memory handed out by `retype` or `map`); checks that a race between the timer interrupt and a system call can bypass.
- **Medium**: malformed DTB input that panics, reads out of bounds or produces an unsafe memory map (the boot chain is trusted, so cap these at Medium unless they give code execution); exhaustion of kernel resources (table pool, untyped memory, TCB or endpoint objects) by a single task that denies service to others without halting the kernel; scheduler starvation; mistakes in `write_bootinfo` beyond the known physical address exposure.
- **Low**: problems that need a misbehaving root task to trigger; `build.rs` issues with trusted source inputs; console formatting and diagnostic wording; hardening that is missing but has no exploitable consequence today.
- A root-cause analysis plus a patch is welcome at every level. A reproducer counts for more than a severity argument.

## Report and patch format

- State the commit hash (`git -C /src rev-parse HEAD`), the platform (QEMU virt unless stated) and the exact system call sequence or root-task patch that triggers the issue. Include the console lines around the failure; the `esr`, `elr` and `far` values are the most useful part.
- Prefer a reproducer as a diff against `userspace/root-task/src/main.rs` that prints a distinctive line on success.
- Patches should be Rust against the current `main`, add no new third-party dependencies, keep fail-secure behaviour (reject or halt rather than guess) and prefer checks that the compiler or `const` assertions can enforce. Every `unsafe` block in this codebase carries a `// SAFETY:` note; keep that. Existing comments are in Korean; English comments in a patch are fine.

## Anything to leave alone

These are known, tracked in `SECURITY.md`, and are not new findings:

- No isolation between tasks: one static root CNode, one address space, no capability derivation tree, no `MINT`, no `revoke`, no capability transfer over IPC (GHSA-vmpv-qc49-3p6r). "Task A reads task B's memory", "task A configures or resumes task B" and "every task holds the Console or Untyped capability" are consequences of this and are not reportable until per-task CSpaces land in 0.2.0. EL0 reaching EL1 memory is a different matter and is always in scope.
- A task that blocks on IPC while every other task is blocked halts the system (GHSA-q99x-2wrv-358p).
- DTB memory regions are not bounded by the physical address width (GHSA-hj3f-38h9-hrpj).
- No KASLR, and `write_bootinfo` exposes the physical addresses of `Untyped` regions to the root task.
- PAC keys are shared between EL0 and EL1. No `pac-ret` code generation is enabled yet, so there is nothing to forge today.
- FP/SIMD is trapped at EL0 on purpose (`CPACR_EL1 = 0`); a task that touches a SIMD register is killed by design.
- `exception_fatal` halting on a kernel-internal exception is intended fail-secure behaviour. Only an EL0-reachable path into it is a finding.
- The root task is trusted; its image is embedded and hashed, not signed. Kernel image integrity belongs to the boot chain.
- The Apple Silicon (`plat-apple`) path cannot run in this image. Compile-only review is welcome; do not report "untested on hardware".
- Upstream issues in vendored crates that are not reachable from the kernel's SHA-256 use.
