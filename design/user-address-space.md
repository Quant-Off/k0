# User address spaces

[한국어](user-address-space_KR.md)

This note describes how K0 builds the root task's address space at boot and how user space extends it afterwards with the `MAP` system call. The code is in `crates/k0-mm/src/user.rs`, `crates/k0-task/src/lib.rs` (`spawn_root`) and `sys_map` in `kernel/src/main.rs`.

## The boot frame window

Before the root task exists, the kernel needs memory for its image, its stack, its page tables and the bootinfo page. `kernel_main` reserves a 2 MiB window of RAM right after the kernel image for this (see [boot.md](boot.md)). `FrameAlloc` hands out frames from the window, one after another, and never frees any. Each frame is zeroed through the kernel alias before it is handed out.

The window is listed as reserved when the root CNode is filled, so none of it becomes untyped memory. The root task can never retype a frame that still holds its own page tables.

## Building the root task's tables

`spawn_root` takes the root task image that has already passed its integrity check (see [hardening.md](hardening.md)) and does the following.

1. It copies the flat image into contiguous frames and synchronizes the instruction cache, because the copy was a data write and EL0 is about to execute it.
2. It creates an empty level 0 table with `UserSpace::new`.
3. It maps every segment from the build-time metadata. Each segment must start inside the image on a granule boundary, end inside the image and have a non-zero size. The entry point must lie inside the image and must not lie in a segment that is not text.
4. It maps a 64 KiB stack just below `0x1000_0000`. The granule below the stack stays unmapped as a guard.
5. It maps the bootinfo page at `0x0F00_0000` read-only.
6. It fills the root TCB's saved context: the program counter at the entry point, the stack pointer at the stack top, and a saved processor state of EL0 with interrupts unmasked.

`UserSpace::map_range` rejects any range that is not granule-aligned, any range that would map the first granule, and any range that reaches past the 48-bit user half. Because the first granule is never mapped, a null pointer dereference in user space always faults.

## User permissions

| Kind | Used for | EL0 | EL1 |
| --- | --- | --- | --- |
| `TextUser` | root task code | read and execute | read, never execute |
| `RoUser` | read-only data, bootinfo, read-only frames | read | read, never execute |
| `RwUser` | data, stacks, read-write frames | read and write | read and write, never execute |

Every user mapping sets `PXN`, so the kernel can never execute user memory. Only `TextUser` leaves `UXN` clear, and `TextUser` is read-only. Only the boot builder can create `TextUser` mappings. The `MAP` system call accepts only read-only or read-write, so after boot no new executable user memory can appear.

## Switching `TTBR0`

`install_user_ttbr0` replaces the identity map with the root task's tables and invalidates the TLB. From that point, a physical address used as a pointer is meaningless to the kernel. The console, the GIC, the DTB and all page tables are already reached through the kernel alias, which is why this switch happens late in `kernel_main`.

Right after the switch, `check_user_wx` checks the result with address translation instructions:

| Check | Expected |
| --- | --- |
| user text readable from EL0 | yes |
| user text writable from EL0 | no |
| top page of the user stack writable from EL0 | yes |
| user stack guard page readable from EL0 | no |
| kernel text readable from EL0 | no |
| bootinfo readable from EL0 | yes |
| bootinfo writable from EL0 | no |
| user text readable from EL1 | no when PAN can be checked, otherwise yes |
| old identity map of kernel text readable | no |

The PAN check needs the `AT S1E1RP` instruction from FEAT_PAN2. On CPUs without it, the check falls back to the page permissions, which do allow EL1 to read user text.

## Growing the address space at run time

After the handoff, the root task adds mappings with `MAP`. The kernel walks the installed tables through the alias, and there is one firm rule: **the run-time walker never allocates a page table**.

- **Mapping a `Frame` capability** with `user_map_frame` creates one leaf entry. If an intermediate table is missing on the way, the call fails with `MISSING_TABLE`. The leaf must be empty, or the call fails with `OVERLAP`.
- **Mapping a `PageTable` capability** with `user_install_table` walks down the path of the given address and links the table into the first empty level it finds. If the path is already complete down to the leaf level, the call fails with `OVERLAP`.

User space therefore pays for its own page tables with its own untyped memory, by retyping `PageTable` objects. The kernel's static table pool is never charged for user mappings.

Further rules in `sys_map`:

- The target address space is always named by an `AddrSpace` capability in `x3`. The kernel never assumes "the current address space".
- Each `Frame` and each `PageTable` capability can be mapped once. A flag in the capability records this, and a second attempt fails with `ALREADY_MAPPED`.
- `Tcb` and `Endpoint` capabilities cannot be mapped at all. Kernel objects are never visible at EL0.
- A corrupted entry found during the walk is treated as a kernel invariant violation. The kernel prints `k0: user page table corrupt` and parks.

## PAN and kernel access to user memory

When the CPU has FEAT_PAN, the kernel runs with PAN set, so any EL1 access through a user virtual address faults. K0 is built so that this never matters today: the kernel touches user frames only through its own alias, and no system call reads or writes a user buffer. A future system call that does need to copy user memory will have to use the unprivileged load and store instructions (`LDTR`, `STTR`).

## How it is tested

- Host unit tests in `crates/k0-mm/src/user/tests.rs` check the W^X properties of every user permission and the validation paths that must reject bad input before touching any table.
- The root task self-tests exercise the run-time path: mapping before a page table exists, mapping into a bad address space, double mapping, mapping at the null address, bad permissions, overlapping mappings, and reading and writing through the new mappings.
- `tools/qemu-boot-test.sh` requires `k0: user w^x checks pass` and `root: retype/map tests pass`.

## Known gaps

- There is exactly one address space, the root task's. `AddrSpace` cannot be retyped yet, so every task runs in it. This is part of advisory GHSA-vmpv-qc49-3p6r in [SECURITY.md](../SECURITY.md).
- Mappings cannot be removed.
- Every frame mapped into user space is also mapped read-write in the kernel alias, because the kernel zeroed it there during retype.
