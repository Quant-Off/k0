# Security Policy

K0 is a pre-release research microkernel. Its capability model is only partially implemented, and the gaps described below are security relevant. Read this document before running any code on K0 that you do not fully trust.

## Supported versions

| Version | Status | Notes |
| --- | --- | --- |
| 0.1.0 (tag `v0.1.0`) and every earlier commit on `main` | Pre-release | No task isolation. See "Isolation model" below. |

There is no fixed version yet. Every commit up to and including the `v0.1.0` tag is affected by the advisories listed in "Known advisories". The version number follows SemVer: while the major version is 0, any minor release may break the system call ABI, and the first release that ships per-task capability spaces will be `0.2.0`.

## Isolation model

K0 currently provides **no isolation between tasks**. This is a known design gap, not an incidental bug, and it holds for every task the kernel can run today.

- There is exactly one capability space, the static root CNode. Every system call resolves its slot arguments in that CNode regardless of which task made the call.
- There is exactly one address space, the root task's TTBR0 root. `TCB_CONFIGURE` can only bind a TCB to that address space.
- There is no capability derivation tree, no `MINT`, no `revoke`, and no capability transfer over IPC.

As a consequence, a task created with `TCB_CONFIGURE` and `TCB_RESUME` is a **thread of the root task**, not a confined process. It holds every capability the root task holds, it can read and write all of the root task's memory, it can retype any `Untyped` capability, it can configure and resume every other TCB, and it can send to or receive from every `Endpoint`.

Until per-task CSpaces, `AddrSpace` retype, and the capability derivation tree land, you must treat every task as fully trusted. Do not run untrusted or third-party code as a K0 task. There is no workaround at the kernel level.

## Known advisories

The following advisories are tracked in this repository's GitHub Security Advisories. All of them affect version 0.1.0 and earlier, and none has a fixed version yet.

| Advisory | Severity | Summary |
| --- | --- | --- |
| GHSA-vmpv-qc49-3p6r | High | All tasks share the root CNode and address space, so any spawned task holds full root authority. |
| GHSA-q99x-2wrv-358p | Medium | A task that blocks on IPC while every other task is blocked halts the whole system. |
| GHSA-hj3f-38h9-hrpj | Low | DTB memory regions are not bounded by the physical address width. |

## Reporting a vulnerability

Please do not open a public issue for a security problem.

- Preferred: open a draft security advisory at <https://github.com/Quant-Off/k0/security/advisories/new>.
- Alternative: email the maintainer at <qtfelix@qu4nt.space>. Include the affected commit hash, the platform (QEMU virt or Apple Silicon), and a reproduction if you have one.

Reports about the isolation gaps described above are already known and do not need to be filed again. Reports about anything else, including boot-time memory map validation, exception handling, scheduler policy, and IPC state machines, are welcome.

## Scope

In scope: everything under `kernel/`, `crates/`, and `userspace/root-task/`.

Out of scope: the boot chain before the kernel entry point (m1n1, QEMU firmware), and the integrity of the kernel image itself. Kernel image signing is the boot chain's responsibility.
