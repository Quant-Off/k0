# Security Policy

K0 is a pre-release research microkernel. The released version has no task isolation, the unreleased `main` branch has it but with the limits described below, and those gaps are security relevant. Read this document before running any code on K0 that you do not fully trust.

## Supported versions

| Version | Status | Notes |
| --- | --- | --- |
| 0.1.0 (tag `v0.1.0`) and every earlier commit on `main` | Pre-release | No task isolation. See "Isolation model" below. |
| Current `main` branch | Unreleased | Task isolation with known limits. See "Isolation model" below. |

There is no fixed version yet. Every commit up to and including the `v0.1.0` tag is affected by the advisories listed in "Known advisories". The version number follows SemVer: while the major version is 0, any minor release may break the system call ABI, and the first release that ships per-task capability spaces will be `0.2.0`.

## Isolation model

This section describes two different states of the code. Do not mix them up.

### Released tag `v0.1.0` and every earlier commit

These provide **no isolation between tasks**. This is a known design gap, not an incidental bug, and it holds for every task those versions can run.

- There is exactly one capability space, the static root CNode. Every system call resolves its slot arguments in that CNode regardless of which task made the call.
- There is exactly one address space, the root task's TTBR0 root. `TCB_CONFIGURE` can only bind a TCB to that address space.
- There is no capability derivation tree, no `MINT`, no `revoke`, and no capability transfer over IPC.

A task created with `TCB_CONFIGURE` and `TCB_RESUME` in those versions is a **thread of the root task**, not a confined process. It holds every capability the root task holds, it can read and write all of the root task's memory, it can retype any `Untyped` capability, it can configure and resume every other TCB, and it can send to or receive from every `Endpoint`. Treat every task as fully trusted there. Do not run untrusted or third-party code as a K0 task. There is no workaround at the kernel level.

### The current `main` branch (unreleased)

The current `main` branch contains task isolation. No release or tag contains it yet, and its system call ABI is incompatible with `v0.1.0` (bootinfo layout version 2).

What a confined task is and can do:

- Every task is bound to a capability space, a CNode object, when it is configured. Slot numbers are resolved only in the calling task's own CNode, so a slot number of one task means nothing in another.
- Every task runs in the address space named by an `AddrSpace` capability. A task reaches exactly the memory that the holder of its `AddrSpace` capability mapped into that address space.
- Both bindings are held as capabilities inside the task's TCB. If either capability is revoked or deleted, the task is torn down.
- Isolation holds between tasks bound to different CNodes and address spaces. Tasks that share them, as the root task's helper threads do, are threads of one another. Whoever holds a task's TCB, CNode or AddrSpace capability controls that task.
- Capabilities carry rights (R, W, X). Rights can only be removed by `CAP_COPY`, never added. `Endpoint` capabilities can carry a badge, set once by `CAP_COPY`, which the receiver sees. Retyped frames never gain X, and X exists only on the root task's text frames, so no physical frame is writable in one place and executable in another.
- Capabilities form a derivation list. `CAP_REVOKE` removes everything derived from a capability, `CAP_DELETE` also removes the slot, an object is finalized when its original capability is removed, and the underlying `Untyped` memory can then be retyped again. Retyped objects are zeroed before a capability to them exists.
- A fault, an exit, or an unknown system call in a confined task terminates only that task.

What is still NOT provided on `main`:

- **Availability.** Isolation does not cover it. If every task is blocked the kernel still parks the core (GHSA-q99x-2wrv-358p), and a confined task can cause this by being the last task to block.
- **Kernel resource exhaustion.** A task holding a lot of `Untyped` memory can exhaust the static pool of 32 kernel page tables used for alias mappings, after which `RETYPE` fails with `KERNEL_RESOURCE` for every task, or exhaust the static address space registry.
- No fault delivery to a handler task, no notifications, no timeouts, and no capability transfer over IPC.
- A sender already queued on an endpoint still delivers once after its capability is revoked.
- One core only.
- New executable code cannot be loaded after boot, so a confined task runs code from the root task image.
- The 16 KiB (Apple Silicon) build is compiled and host-tested only. It has never been booted.
- The root task is still trusted and holds the initial `Untyped` capabilities, and `write_bootinfo` still exposes their physical addresses to it.

Even on `main`, treat running untrusted code as experimental. The isolation is exercised by the host tests and the boot self-tests, and the availability limits above still apply. Isolation bypasses on `main` are welcome reports.

## Known advisories

The following advisories are tracked as GitHub Security Advisories in this repository. The first two were published on 2026-11-07. GHSA-hj3f-38h9-hrpj is still an unpublished draft, so its identifier does not resolve publicly yet, and this table is its public record until it is published. All of them affect version 0.1.0 and earlier, and none has a fixed version yet.

| Advisory | Severity | Summary |
| --- | --- | --- |
| [GHSA-vmpv-qc49-3p6r](https://github.com/Quant-Off/k0/security/advisories/GHSA-vmpv-qc49-3p6r) | High | All tasks share the root CNode and address space, so any spawned task holds full root authority. |
| [GHSA-q99x-2wrv-358p](https://github.com/Quant-Off/k0/security/advisories/GHSA-q99x-2wrv-358p) | Medium | A task that blocks on IPC while every other task is blocked halts the whole system. |
| GHSA-hj3f-38h9-hrpj | Low | DTB memory regions are not bounded by the physical address width. |

The current `main` branch contains changes that address the subject of some of these advisories. No release contains them yet, and advisory status is updated only at release.

## Reporting a vulnerability

Please do not open a public issue for a security problem.

- Preferred: open a draft security advisory at <https://github.com/Quant-Off/k0/security/advisories/new>.
- Alternative: email the maintainer at <qtfelix@qu4nt.space>. Include the affected commit hash, the platform (QEMU virt or Apple Silicon), and a reproduction if you have one.

Isolation gaps in `v0.1.0` and earlier commits, and the availability and resource limits listed under "What is still NOT provided on `main`", are already known and do not need to be filed again. Isolation bypasses on the current `main` branch are welcome, for example a task reaching another task's memory or capabilities, a revoked capability that keeps working, or a stale mapping that survives unmap, revoke or address space destruction. Reports about anything else, including boot-time memory map validation, exception handling, scheduler policy, and IPC state machines, are welcome.

## Scope

In scope: everything under `kernel/`, `crates/`, and `userspace/root-task/`.

Out of scope: the boot chain before the kernel entry point (m1n1, QEMU firmware), and the integrity of the kernel image itself. Kernel image signing is the boot chain's responsibility.

The kernel checks the embedded root task image against a SHA-256 hash at every boot. This is a corruption check, not a signature check. The reference hash lives in the same kernel image as the root task, so anyone who can modify the kernel image can change both. Reports that rely on modifying the kernel image are therefore out of scope.
