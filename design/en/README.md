# K0 design notes

<div align="center">

English | [한국어](../kr/README.md)

</div>

These notes explain how each part of K0 works, why it is built that way, how it is tested, and what is still missing. They describe the code of the current `main` branch (unreleased, crate version 0.1.0), which includes task isolation. The released `v0.1.0` tag does not. If a note and the code disagree, the code is what runs. Please open an issue so the note can be fixed.

Read them in this order the first time. It follows the order in which the kernel does things at boot.

| Note | Subject |
| --- | --- |
| [boot.md](boot.md) | From the first instruction to the root task: core selection, the drop to EL1, the jump to the higher half, and the fixed initialization order |
| [memory.md](memory.md) | Kernel page tables, the static table pool and its rollback, W^X, guard pages, and the boot-time translation checks |
| [user-address-space.md](user-address-space.md) | `AddrSpace` objects and their registry, user permissions, `MAP` and `UNMAP` without the kernel allocating tables, and how mappings are cleaned up when capabilities are removed |
| [capabilities.md](capabilities.md) | Per-task CNodes, rights and badges, the derivation list, untyped memory, retype and its failure guarantees, revoke and delete, and what task isolation does and does not cover |
| [dtb.md](dtb.md) | The device tree as hostile input: what the parser reads, its limits, and how it is fuzzed |
| [uefi.md](uefi.md) | The UEFI loader: why it sits outside the kernel, its flow, which memory it refuses to call RAM, the device tree rewrite, the self-check before the jump, what QEMU cannot test, and what has been booted |
| [hardening.md](hardening.md) | PAC key derivation, PAN, closing EL0 access to counters, debug and PMU, and the root task integrity check, including what each does not protect yet |
| [exceptions.md](exceptions.md) | The vector table, saving user state, system call dispatch, and fault isolation |
| [scheduling.md](scheduling.md) | TCBs and their bound CNode and address space, task creation and teardown, the round-robin scheduler, and timer preemption |
| [ipc.md](ipc.md) | Endpoints and badges, `SEND`, `RECV`, `CALL` and `REPLY_RECV`, the single-use reply right, and what happens to waiting tasks when an endpoint is deleted |

Every note ends with a "Known gaps" section. The open security advisories are listed in [SECURITY.md](../../SECURITY.md).
