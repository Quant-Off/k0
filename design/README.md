# K0 design notes

[한국어](README_KR.md)

These notes explain how each part of K0 works, why it is built that way, how it is tested, and what is still missing. They describe the code as of version 0.1.0. If a note and the code disagree, the code is what runs. Please open an issue so the note can be fixed.

Read them in this order the first time. It follows the order in which the kernel does things at boot.

| Note | Subject |
| --- | --- |
| [boot.md](boot.md) | From the first instruction to the root task: core selection, the drop to EL1, the jump to the higher half, and the fixed initialization order |
| [memory.md](memory.md) | Kernel page tables, the static table pool and its rollback, W^X, guard pages, and the boot-time translation checks |
| [user-address-space.md](user-address-space.md) | The root task's address space, user permissions, and how `MAP` grows an address space without the kernel allocating tables |
| [capabilities.md](capabilities.md) | The root CNode, untyped memory, retype and its failure guarantees, and why 0.1.0 has no isolation |
| [dtb.md](dtb.md) | The device tree as hostile input: what the parser reads, its limits, and how it is fuzzed |
| [hardening.md](hardening.md) | PAC key derivation, PAN, closing EL0 access to counters, debug and PMU, and the root task integrity check, including what each does not protect yet |
| [exceptions.md](exceptions.md) | The vector table, saving user state, system call dispatch, and fault isolation |
| [scheduling.md](scheduling.md) | TCBs, task creation, the round-robin scheduler, and timer preemption |
| [ipc.md](ipc.md) | Endpoints, `SEND`, `RECV`, `CALL` and `REPLY_RECV`, and the single-use reply right |

Every note ends with a "Known gaps" section. The open security advisories are listed in [SECURITY.md](../SECURITY.md).
