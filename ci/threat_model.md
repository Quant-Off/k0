# Threat model: K0

K0 is a capability-based microkernel for AArch64, written in Rust `no_std` in the style of seL4 and licensed under PolyForm Noncommercial 1.0.0. The kernel runs at EL1. The only user-space code today is the root task, which runs at EL0 and doubles as the self-test harness. One generic image finds the console, the interrupt controller (GICv2 or GICv3) and the timer in the device tree at boot, and it may be entered at EL1 or at EL2. The 4 KiB granule is the default and the 16 KiB granule is a build option. Only QEMU machines can be exercised in this image.

Read `SECURITY.md` at the repository root first. It describes the isolation model of the released version 0.1.0, the unreleased isolation work on the current `main` branch, and the issues that are already known. This model describes the current `main` branch.

## What the kernel does and where untrusted input enters

Trust boundaries, from least to most trusted:

1. **EL0 code via system calls (`svc`).** Every register a task passes to the kernel is attacker controlled: capability slot indices, virtual addresses, permission bits, sizes, object types, badges and message words. This is the primary attack surface. Entry path: `crates/k0-arch/src/vectors.S` (`__lower_common`) -> `crates/k0-arch/src/usermode.rs` (`el0_sync`) -> `kernel/src/syscall.rs` (`k0_syscall` and the `sys_*` functions). Slot arguments are resolved only in the calling task's own CNode.
2. **EL0 faults.** Data and instruction aborts, undefined instructions, trapped system register and FP/SIMD accesses, `brk` and alignment faults all reach `k0_fault` in `kernel/src/syscall.rs`, which must only kill the faulting task.
3. **Timer interrupts** taken at EL1 and from EL0 (`irq_current`, `irq_lower`), which drive preemption in `crates/k0-sched`.
4. **The device tree blob (DTB)** that the boot chain passes in `x0`. Nothing reads it before the MMU is on. `kernel_main` in `kernel/src/main.rs` maps it read-only (`map_dtb`), then `crates/k0-boot/src/fdt.rs` reads the memory map and `crates/k0-boot/src/platform.rs` finds the console, the interrupt controller and the timer interrupt, both on the shared walker `crates/k0-boot/src/fdt/walk.rs`. The results become the physical memory map (`k0_cap::bootstrap`) and the device mappings, after `Platform::validate` has checked the devices against the memory map (`bring_up_platform` in `kernel/src/main.rs`). The boot chain is trusted to load an authentic kernel image, but the DTB code must still be total: no panic, no out-of-bounds read, no integer overflow on any input, and no device range that overlaps memory handed to user space.
5. **The root task image** is embedded in the kernel at build time together with its SHA-256 (`crates/k0-boot/build.rs`, `verify_root_task`). It is part of the trusted computing base today. The hash detects corruption, not a malicious image; signature verification is a documented TODO.

Out of the kernel's control and therefore trusted: the boot chain (firmware, the boot loader, the hypervisor), the integrity of the kernel image itself, and the hardware.

## Components that matter most and least

Highest value, all in scope:

- `kernel/src/syscall.rs`: system call dispatch, every capability lookup and permission check, `k0_fault`, the exit path that handles a task that died during its own call.
- `kernel/src/object.rs` and `kernel/src/main.rs`: object creation and finalization, `write_bootinfo`, platform bring-up and the choice of the boot frame window.
- `crates/k0-cap`: CNode objects, the derivation list, `Untyped` accounting and `retype`, copy, move, delete and revoke, finalization without recursion, bootstrap of the memory regions.
- `crates/k0-mm`: page table construction, the table pool (`Pool::map_page` and its rollback), W^X enforcement, user mapping validation, the user page table walkers and the address space registry (id and generation per address space, stale mapping records).
- `crates/k0-arch`: exception vectors and user context save/restore (`vectors.S`, `usermode.rs`), hardening (`hardening.rs`: PAC, BTI, PAN, `CPACR_EL1`), the GICv2 and GICv3 drivers and the timer (`irq.rs`), the console drivers and the boot log (`earlycon.rs`), cache and TLB maintenance. The entry code and the EL2 setup are in `kernel/src/arch/aarch64/boot.S`.
- `crates/k0-ipc`: endpoint state machine, send, receive, call, reply, `abort_reply`.
- `crates/k0-sched` and `crates/k0-task`: run queue, `kill_current`, TCB state.
- `crates/k0-boot`: FDT walker and memory parser, platform discovery and validation (`platform.rs`), root task flattening and verification at build time and at boot.
- `crates/k0-abi`: system call numbers and ABI structures shared with user space.

Lower value:

- `userspace/root-task`: the trusted root task and test harness. Bugs here are not kernel vulnerabilities, but it is the right place to write reproducers.
- `vendor/`: third-party crates vendored with pinned checksums. Only `sha2` and its dependencies are linked into the kernel, for the boot-time hash of the fixed root task image. Report issues there only if reachable through that use; generic upstream findings are out of scope.
- `tools/qemu-virt-runner.sh`, `tools/boot-matrix.sh`, `tools/boot-log.py`, `kernel/build.rs`, `crates/k0-boot/build.rs`, `userspace/root-task/build.rs`: developer tooling that runs on the trusted build host.
- Documentation, `.idea/`, `.claude/`.

## How to exercise it

- Build: `cargo build -p k0-kernel --offline` (generic 4 KiB image, dev profile with debug info; `panic = "abort"` in every profile). The 16 KiB image is `cargo build -p k0-kernel --release --no-default-features --features granule-16k --offline`. It boots only on a CPU model with 16 KiB pages: `K0_QEMU_CPU=max tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/release/k0-kernel`.
- Run: `tools/qemu-virt-runner.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel`, which is what `cargo virt` (alias of `cargo run -p k0-kernel`) does. It converts the ELF to a raw image with `llvm-objcopy` and boots `qemu-system-aarch64 -machine virt,gic-version=3 -cpu cortex-a72 -smp 1 -m 512M -nographic`. `K0_QEMU_MACHINE`, `K0_QEMU_CPU`, `K0_QEMU_SMP` and `K0_QEMU_MEM` change the machine, for example `K0_QEMU_MACHINE=virt,gic-version=2` or `K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on` (entry at EL2). `tools/boot-matrix.sh` runs every configuration that K0 calls supported. The kernel never exits: once the root task finishes its self-tests it yields forever and the kernel prints `k0: tick N` once a second. Wrap every run in `timeout`, or use `tools/qemu-boot-test.sh`, which checks the expected log lines in order, requires an exact number of `k0: task fault` lines and exactly one `k0: unknown syscall` line, and stops QEMU once they appear.
- A healthy boot log is in `/src/bootcheck.log`, captured when this image was built. The lines that matter are `k0: w^x checks pass`, `k0: user w^x checks pass`, `root: retype/map tests pass`, `root: child exit test pass`, `root: ipc tests pass`, `root: fault isolation tests pass`, `root: preempt test pass`, `root: isolation tests pass`, `root: violation tests pass`, `root: teardown tests pass`, `root: revocation tests pass` and `root: sched tests pass`.
- Reproducers: the root task (`userspace/root-task/src/main.rs`) is the only EL0 image and already contains the test harness and thin `sys*` wrappers around `svc`. Add a test there; when the reproducer needs its own thread, build a task with its own CNode and address space (`RETYPE`, `TCB_CONFIGURE`, `TCB_RESUME`), which then runs code from the root task image, rebuild and run. `crates/k0-boot/build.rs` rebuilds and re-embeds the root task automatically.
- Host tests: `tools/host-test.sh` runs the unit tests of `k0-abi`, `k0-cap` and `k0-boot` on any host and those of `k0-mm` on AArch64 hosts. `k0-cap` includes a model test over random retype, copy, move, delete and revoke sequences, and `k0-mm` runs the real user page table walkers and the address space registry. The DTB parser has a deterministic mutation fuzzer: `K0_FUZZ_ITERS=5000000 cargo test --release --offline --target "$(rustc -vV | sed -n 's/^host: //p')" -p k0-boot fuzz -- --nocapture`. A failing case prints `K0_FUZZ_CASE=0x...`, which replays it and dumps the input. The fuzzer's DTB builder in `crates/k0-boot/src/fdt/builder.rs` is a convenient way to write a crafted DTB for a reproducer.
- PAC, PAN and RNDR paths: the default `cortex-a72` model has none of them. `tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel -cpu max` boots on a CPU that has all three.
- A boot that the kernel refuses before it has a console prints nothing. Its reason is in the boot log in memory: start QEMU with `K0_QEMU_MONITOR=<socket path>` and read it with `tools/boot-log.py <elf> <load address> <socket path>` (the load address on QEMU `virt` is `0x40080000`).
- Debugging: append `-s -S` to the runner (`tools/qemu-virt-runner.sh <elf> -s -S`) and attach with `gdb-multiarch target/aarch64-unknown-none-softfloat/debug/k0-kernel -ex 'target remote :1234'`. `-d int,guest_errors` prints exception traces. `llvm-objdump -d` (from the pinned toolchain, linked into `/usr/local/bin`) disassembles both images.
- Fault semantics: an EL0 fault prints `k0: task fault <name> (esr .. elr .. far ..), killing task` and kills only that task. A kernel-internal exception goes to `exception_fatal`, which halts the machine on purpose (fail-secure). Reaching that halt from EL0 is a finding.

## How we rate severity

Rate by what EL0 code can do to EL1 or, on the current `main` branch, to another confined task (see "Anything to leave alone").

- **Critical**: any EL0 -> EL1 privilege escalation, and on the current `main` branch any break of task isolation (reading or writing another task's memory, using a slot or capability of another task, a revoked capability or stale mapping that still works). Executing attacker-chosen code at EL1; reading or writing kernel memory or arbitrary physical memory through a system call; obtaining a mapping, capability or object that the caller's slot arguments do not grant (bypassing a capability or permission check); mapping memory that is both writable and executable for EL0, or kernel memory into TTBR0; getting the kernel to restore EL1 state (mode, exception level, interrupt masks) that came from user input.
- **High**: memory-safety bugs in kernel code reachable from EL0, with or without a demonstrated exploit (out-of-bounds index, integer overflow or truncation in size or address arithmetic, stale raw pointers, aliasing `&mut`, unsound `unsafe`); a system call or fault sequence that halts the whole system (kernel panic, `exception_fatal`, an unbounded loop with interrupts masked, or a scheduler with no runnable task outside the known all-tasks-blocked case); missing TLB or cache maintenance that lets a task keep using memory after it was unmapped or retyped; kernel register or memory contents leaking to EL0 (registers not restored or cleared on the return path, uninitialised memory handed out by `retype` or `map`); checks that a race between the timer interrupt and a system call can bypass.
- **Medium**: malformed DTB input that panics, reads out of bounds, produces an unsafe memory map or gets a device mapped over memory that is handed out (the boot chain is trusted, so cap these at Medium unless they give code execution); exhaustion of kernel resources (table pool, untyped memory, TCB or endpoint objects) by a single task that denies service to others without halting the kernel; scheduler starvation; mistakes in `write_bootinfo` beyond the known physical address exposure.
- **Low**: problems that need a misbehaving root task to trigger; `build.rs` issues with trusted source inputs; console formatting and diagnostic wording; hardening that is missing but has no exploitable consequence today.
- A root-cause analysis plus a patch is welcome at every level. A reproducer counts for more than a severity argument.

## Report and patch format

- State the commit hash (`git -C /src rev-parse HEAD`), the platform (QEMU virt unless stated) and the exact system call sequence or root-task patch that triggers the issue. Include the console lines around the failure; the `esr`, `elr` and `far` values are the most useful part.
- Prefer a reproducer as a diff against `userspace/root-task/src/main.rs` that prints a distinctive line on success.
- Patches should be Rust against the current `main`, add no new third-party dependencies, keep fail-secure behaviour (reject or halt rather than guess) and prefer checks that the compiler or `const` assertions can enforce. Every `unsafe` block in this codebase carries a `// SAFETY:` note; keep that. Existing comments are in Korean; English comments in a patch are fine.

## Anything to leave alone

These are known, tracked in `SECURITY.md`, and are not new findings:

- No isolation between tasks in `v0.1.0` and earlier commits: one static root CNode, one address space, no capability derivation tree, no `MINT`, no `revoke`, no capability transfer over IPC (GHSA-vmpv-qc49-3p6r). Findings that only apply to those versions are not new. The current `main` branch (unreleased) has per-task CNodes and address spaces, rights, badges, a derivation list and revocation, so isolation bypasses there ARE in scope. EL0 reaching EL1 memory is always in scope.
- Known limits of isolation on `main`: no fault delivery to a handler, no notifications, no timeouts, no capability transfer over IPC, a single core, and a sender already queued on an endpoint still delivers once after its capability is revoked. A task holding a lot of `Untyped` memory can exhaust the 32 static kernel table pool (`RETYPE` then fails with `KERNEL_RESOURCE`) or the address space registry; this is a known availability limit. New executable code cannot be loaded after boot by design, so a confined task runs code from the root task image.
- A task that blocks on IPC while every other task is blocked halts the system (GHSA-q99x-2wrv-358p). Isolation does not cover availability, so a confined task can trigger this.
- DTB memory regions are not bounded by the physical address width (GHSA-hj3f-38h9-hrpj). The current `main` branch stops the boot on such a region; the advisory itself is still a draft.
- No KASLR: the kernel image always sits at the same virtual address. `write_bootinfo` exposes the physical addresses of `Untyped` regions to the root task.
- A wrong or hostile DTB can stop the boot, or point the console at another device that passes the checks (a 16550 has no identification register). A pinned build (`K0_PIN`) compares values that come from the DTB, so it does not stop a hostile boot chain.
- The DTB pointer in `x0` is mapped read-only and read before any memory map exists. A boot loader that passes the address of a device makes the kernel read device registers as memory.
- A task with a `Console` capability can print lines that look like kernel output (`k0: ...`). Its output is filtered for control bytes but not marked.
- A trap to EL2 after the kernel has dropped to EL1 parks the whole machine by design.
- PAC keys are shared between EL0 and EL1. No `pac-ret` code generation is enabled yet, so there is nothing to forge today.
- FP/SIMD is trapped at EL0 on purpose (`CPACR_EL1 = 0`); a task that touches a SIMD register is killed by design.
- `exception_fatal` halting on a kernel-internal exception is intended fail-secure behaviour. Only an EL0-reachable path into it is a finding.
- The root task is trusted; its image is embedded and hashed, not signed. Kernel image integrity belongs to the boot chain.
- Only QEMU runs in this image. Real boards, KVM and other hypervisors, the 16550 console driver and CPUs with `HCR_EL2.E2H` fixed to 1 have never been booted. Review of those paths is welcome; do not report "untested on hardware".
- Upstream issues in vendored crates that are not reachable from the kernel's SHA-256 use.
