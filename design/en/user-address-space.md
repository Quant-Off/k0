# User address spaces

<div align="center">

English | [한국어](../kr/user-address-space_KR.md)

</div>

This note describes how K0 builds the root task's address space at boot, how user space creates further address spaces with `RETYPE`, and how it fills them with `MAP` and `UNMAP`. The code is in `crates/k0-mm/src/user.rs`, `crates/k0-task/src/lib.rs` (`spawn_root`), `sys_retype`, `sys_map` and `sys_unmap` in `kernel/src/syscall.rs`, and the cleanup on capability removal in `kernel/src/object.rs`. This describes the current `main` branch (unreleased).

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
| `TextUser` | root task code, and frames mapped with `MAP` `RX` | read and execute | read, never execute |
| `RoUser` | read-only data, bootinfo, read-only frames | read | read, never execute |
| `RwUser` | data, stacks, read-write frames | read and write | read and write, never execute |

Every user mapping sets `PXN`, so the kernel can never execute user memory. Only `TextUser` leaves `UXN` clear, and `TextUser` is read-only. `TextUser` mappings come from two places: the boot builder, and `MAP` with permission `RX`, which needs a `Frame` capability that carries the `X` right (see below). No frame can be writable in one place and executable in another, so after boot no new executable code can be loaded.

## Switching `TTBR0`

`install_user_ttbr0` replaces the identity map with the root task's tables and invalidates the TLB. After the handoff the scheduler calls it again whenever the next task's address space root differs from `current_user_root()`. From that point, a physical address used as a pointer is meaningless to the kernel. The console, the GIC, the DTB and all page tables are already reached through the kernel alias, which is why this switch happens late in `kernel_main`.

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

## Address spaces as objects

The root task's address space is no longer the only one. `RETYPE` with `ADDR_SPACE` turns an untyped granule into an `AddrSpace` object: one zeroed level 0 table. The granule is zeroed through the kernel alias before the capability exists. A task is bound to an address space by `TCB_CONFIGURE`, which takes an `AddrSpace` capability and stores a copy of it in the TCB (see [capabilities.md](capabilities.md)). Tasks in different address spaces cannot see each other's memory, and each task resolves slot numbers only in its own CNode. Isolation here means confidentiality and integrity between tasks. It does not cover availability.

### The registry

A static registry in `k0-mm` (`AddrSpaces`, `MAX_ADDR_SPACES` entries, index 0 never used) gives every live address space an id (`asid`) and a generation. The root task's address space is registered in `kernel_main`, the others by `sys_retype`. Free entries form a singly linked list inside the array, so registering and releasing take no search. Releasing bumps the generation. An entry whose generation can no longer be bumped is retired and never handed out again. If the registry is full, `RETYPE` of an `AddrSpace` fails.

A `Frame` or `PageTable` capability remembers where it is mapped as a record of `(asid, generation, va, level)`. It never stores a pointer. If the address space is destroyed first, the record no longer resolves, and the kernel does not walk a root table whose memory may already be something else.

## Growing an address space at run time

User space maps into an address space with `MAP`. The kernel walks the tables through the alias, and there is one firm rule: **the run-time walker never allocates a page table**.

- **Mapping a `Frame` capability** with `user_map_frame` creates one leaf entry. If an intermediate table is missing on the way, the call fails with `MISSING_TABLE`. The leaf must be empty, or the call fails with `OVERLAP`.
- **Mapping a `PageTable` capability** with `user_install_table` walks down the path of the given address and links the table into the first empty level it finds. If the path is already complete down to the leaf level, the call fails with `OVERLAP`.

User space therefore pays for its own page tables with its own untyped memory, by retyping `PageTable` objects. The kernel's static table pool is never charged for user mappings.

Further rules in `sys_map`:

- The target address space is always named by an `AddrSpace` capability in `x3`, looked up in the caller's own CNode. The kernel never assumes "the current address space", and it never uses the installed `TTBR0`.
- Each `Frame` and each `PageTable` capability can be mapped once. The mapping record in the capability tracks this, and a second attempt fails with `ALREADY_MAPPED`. After `UNMAP` the capability can be mapped again.
- Any capability kind other than `Frame` and `PageTable` fails with `BAD_CAP`. Kernel objects are never visible at EL0.
- A misaligned address, the null granule or an address beyond the 48-bit user half fails with `BAD_VA`.
- A corrupted entry found during the walk is treated as a kernel invariant violation. The kernel prints `k0: FATAL user page table corrupt` and halts.

### Permissions and rights

The permission argument of a `Frame` mapping must be covered by the rights of the capability:

| Permission | Rights needed | Resulting mapping |
| --- | --- | --- |
| `RO` | `R` | `RoUser` |
| `RW` | `R` and `W` | `RwUser` |
| `RX` | `R` and `X` | `TextUser` |

An unknown permission fails with `BAD_PERM`, missing rights with `NO_RIGHTS`. Rights can only be removed, and only by `CAP_COPY`, so a capability can never gain more than its source has. A frame made by `RETYPE` gets `R` and `W` and can never get `X`. The `X` right exists only on the `Frame` capabilities of the root task's own text frames, which the kernel hands over at boot without `W`.

This gives W^X per physical frame instead of per mapping. A frame that can be written has no capability that allows `RX`, and a frame that can be executed has no capability that allows `RW`. No physical frame is writable in one place and executable in another, in any address space. The consequence is stated plainly: **new executable code cannot be loaded after boot.** A confined task runs code from the root task's image text, mapped `RX` into its own address space.

### Unmapping

`UNMAP` takes a `Frame` or `PageTable` capability that is currently mapped (otherwise `BAD_STATE`).

- A `Frame` clears its own leaf. The kernel clears the leaf only if the record still resolves to a live address space and the entry still points at this very frame. A stale record, or a different frame that took the place, leaves the table untouched.
- A `PageTable` is cut from its parent entry, again only if the entry still points at this table. After that the table is **always zeroed**, even when the record was stale. A table with an empty record is therefore always all zero, and a detached table cannot carry old mappings to another place or another address space. If the table was reachable from a destroyed address space, it is unreachable anyway and zeroing it is harmless.

`CAP_DELETE` and `CAP_REVOKE` run the same removal on every `Frame` and `PageTable` capability they delete: the mapping is removed with the same checks. The table of a deleted `PageTable` is not zeroed at that point, because its object is gone. `RETYPE` zeroes the granule before any reuse.

Capabilities for frames below a detached table keep their records. Those records are harmless: a later `UNMAP` finds that the path is gone or points elsewhere and clears nothing.

### TLB policy

K0 uses no hardware ASIDs. Switching `TTBR0` invalidates the whole TLB, and so does every unmap, every table detach and every address space destruction (`flush_user_tlb`). The TLB therefore only ever holds translations of the address space that is installed. `MAP` only turns an invalid entry into a valid one and needs no invalidation.

### Destroying an address space

Removing the original `AddrSpace` capability (delete or revoke of its source) finalizes the object. The kernel releases the registry entry, which bumps the generation, and if the address space is installed in `TTBR0` it moves `TTBR0` to an empty root table from the kernel pool and flushes the TLB. Every task bound to the address space loses its bound capability and is torn down (see [capabilities.md](capabilities.md)): it leaves its queues, its reply links are cut and it becomes dead. If registry bookkeeping is inconsistent, the kernel prints `k0: FATAL address space registry corrupt` and halts.

## PAN and kernel access to user memory

When the CPU has FEAT_PAN, the kernel runs with PAN set, so any EL1 access through a user virtual address faults. K0 is built so that this never matters today: the kernel touches user frames only through its own alias, and no system call reads or writes a user buffer. Entry points, stack pointers and arguments reach the kernel as register values only. Because tasks now run in different address spaces, a kernel access through a user virtual address would also be meaningless, as it would resolve in whichever space is installed. A future system call that does need to copy user memory will have to use the unprivileged load and store instructions (`LDTR`, `STTR`).

## How it is tested

- Host unit tests in `crates/k0-mm/src/user/tests.rs` check the W^X properties of every user permission and the validation paths that must reject bad input before touching any table. They also run the real walkers on host memory: top-down table installation (`tables_install_top_down_and_frames_need_the_full_path`), unmap that clears only the leaf of the very frame (`unmap_clears_only_the_leaf_that_maps_this_very_frame`), uninstall that cuts exactly one link (`uninstall_cuts_exactly_one_link_and_a_zeroed_table_carries_nothing`), a model test of random map and unmap sequences (`random_map_and_unmap_agree_with_a_model`), and full coverage of the 48-bit range. The registry has tests for distinct non-zero ids, rejected bad ids, stale generations (`a_stale_generation_never_resolves_again`), random register and release sequences and retirement of an exhausted generation. `tools/host-test.sh` runs `k0-mm` for both granules on AArch64 hosts only.
- The root task self-tests exercise the run-time path: mapping before a page table exists, double mapping, mapping at the null address, bad permissions, overlapping mappings, executable mapping of a retyped frame (`NO_RIGHTS`), `RX` of a text frame and `RW` of it (`NO_RIGHTS`), rights that never grow through copies, unmap and remap, mapping that follows a moved capability, a detached table that does not carry its old leaf to a new place, stale frame records after the table was detached, and reading and writing through the new mappings.
- The isolation self-tests build confined tasks with their own CNode and address space (only image text and read-only data mapped) and check that a read of a root-only address faults at exactly that address, that writes to a read-only page, execution from a stack page and reads of a kernel address fault, and that a task dies when its address space is deleted.
- `tools/qemu-boot-test.sh` requires, among others, `k0: user w^x checks pass`, `root: retype/map tests pass`, `root: isolation tests pass`, `root: teardown tests pass` and `root: revocation tests pass`, and an exact count of `k0: task fault` lines.

## Known gaps

- Isolation does not cover availability. If every task is blocked, the kernel parks the core (see [SECURITY.md](../../SECURITY.md)), and a confined task can cause this by blocking last.
- The registry holds at most `MAX_ADDR_SPACES` address spaces. The kernel alias of every retyped object uses a static pool of kernel page tables. A task holding a lot of untyped memory can exhaust that pool, and then `RETYPE` fails with `KERNEL_RESOURCE` for everyone.
- No new executable code can be loaded after boot, by design (per-frame W^X). A confined task can only run code from the root task image.
- There are no hardware ASIDs, so every `TTBR0` switch and every unmap flushes the whole TLB. This costs performance, not safety.
- Every frame mapped into user space is also mapped read-write in the kernel alias, because the kernel zeroed it there during retype.
- The kernel copies no user buffers. A future system call that does must use `LDTR` and `STTR` and resolve the buffer in the caller's address space.
- One core only, and no notifications, fault delivery to a handler task or capability transfer over IPC.
