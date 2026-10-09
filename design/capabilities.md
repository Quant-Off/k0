# Capabilities, untyped memory and retype

[한국어](capabilities_KR.md)

This note describes K0's capability model as it exists in version 0.1.0: the root CNode, how free memory becomes untyped capabilities at boot, and how user space turns untyped memory into kernel objects with `RETYPE`. The code is in `crates/k0-cap/src/lib.rs`, with the system call layer in `kernel/src/main.rs`.

Read the "Known gaps" section before relying on any of this for isolation. In 0.1.0 every task shares the same capabilities.

## What a capability is in K0

A capability is an entry in a slot of the root CNode. User space names a capability by its slot number, and the kernel resolves the number in the CNode on every system call. There is no other way to name a kernel resource. System calls that change memory or another task always take a capability for the object they change. Only calls about the caller itself, such as `YIELD` and `EXIT`, act on the calling task without one.

The root CNode has 32 slots. Slot 0 always holds the null capability, so a zeroed argument never names anything.

| Kind | Created | Grants |
| --- | --- | --- |
| `RootTcb` | at boot | nothing yet; marks the running root task, which cannot be reconfigured |
| `AddrSpace` | at boot | the right to map into the root task's address space and to bind TCBs to it |
| `Console` | at boot | the right to write bytes to the debug console |
| `Untyped` | at boot | a range of physical memory that can be retyped |
| `Frame` | by `RETYPE` | one granule of memory that can be mapped into user space |
| `PageTable` | by `RETYPE` | one page table that can be installed in an address space |
| `Tcb` | by `RETYPE` | a task that can be configured and resumed |
| `Endpoint` | by `RETYPE` | a rendezvous point for synchronous IPC |

Capabilities carry no rights bits. Holding a slot means holding the whole authority of the object.

## Filling the root CNode at boot

`bootstrap` fills the CNode in a fixed order: the null capability, `RootTcb`, `AddrSpace`, `Console`, and then untyped capabilities.

The untyped capabilities cover every byte of RAM reported by the device tree, minus every reserved range. The kernel passes these reserved ranges:

- the kernel image,
- the DTB,
- the 2 MiB boot frame window, which holds the root task's image, stack and page tables, and
- every firmware reserved range from the DTB, both the memory reservation block and the children of `/reserved-memory`.

Reserved ranges may overlap one another and may arrive in any order. For each memory region, a cursor walks forward through the reserved ranges in address order and emits the gaps between them, so overlapping reservations simply merge.

Memory regions themselves must not overlap. If two regions overlapped, the same physical frame would sit in two untyped capabilities and could be retyped into two different objects. `bootstrap` rejects such a map, and the kernel stops booting. Running out of slots also stops the boot.

## The bootinfo page

Just before the handoff, the kernel writes a description of the CNode into the bootinfo page, which is mapped read-only for the root task. The page holds a header (layout version, granule size, number of slots) followed by one entry per slot with its kind. Only untyped entries reveal a physical base and size, because the root task needs them to plan its memory. For every other kind, the base and size are zero, so kernel object addresses are not exposed.

The page is a snapshot. Capabilities created later by `RETYPE` do not appear in it. Their slot numbers come back as the return value of `RETYPE`.

## Retype

`RETYPE` takes an untyped slot and an object type and creates one new object of exactly one granule.

Each untyped capability has a watermark: how much of it has been used, counted from its base. A retype rounds the watermark up to the granule, takes the next granule as the new object, and moves the watermark past it. The watermark only ever grows, so two objects carved from the same untyped capability never overlap, and memory that has been retyped is never handed out again.

The order of the checks is the important part.

1. The source slot must exist and hold an untyped capability.
2. The CNode must have a free slot.
3. The rounded watermark plus one granule must fit inside the untyped range, with every addition checked for overflow.
4. Only then does the kernel prepare the memory. The callback in `sys_retype` maps the granule into the kernel alias (see [memory.md](memory.md)) and zeroes it.
5. Only if the preparation succeeds does the watermark move and the new capability go into the next free slot.

If any step fails, nothing has changed. The watermark, the slot count and the kernel table pool are exactly as before, and the caller gets an error code.

Zeroing is a security step and a design choice at the same time. It keeps the previous contents of the memory from leaking into the new object. It also means a zeroed frame already is a valid initial object: a zeroed TCB is in the `Inactive` state, and a zeroed endpoint has empty queues. No object needs any further initialization.

## Using the new capabilities

| System call | Capabilities it requires |
| --- | --- |
| `MAP` | a `Frame` or `PageTable`, and an `AddrSpace` naming the target |
| `TCB_CONFIGURE` | a `Tcb` in the `Inactive` state, and an `AddrSpace` |
| `TCB_RESUME` | a `Tcb` in the `Stopped` state |
| `SEND`, `RECV`, `CALL`, `REPLY_RECV` | an `Endpoint` |
| `DEBUG_PUTC` | `Console` |

A slot number beyond the filled part of the CNode returns `BAD_SLOT`. A filled slot of the wrong kind returns `BAD_CAP`, or `NOT_UNTYPED` for `RETYPE`. The null slot 0 counts as a slot of the wrong kind.

## How it is tested

- Host unit tests in `crates/k0-cap/src/tests.rs` check that bootstrap produces exactly "memory minus reserved" for thousands of random layouts, that bad and overlapping regions are rejected, that the watermark is aligned and monotonic, that a failed preparation leaves no trace, that bad sources are rejected before any preparation runs, and that random sequences of retypes never produce overlapping objects.
- The root task self-tests retype every object type, check that a retyped frame reads as zero, and check the rejection paths for the null slot and for bad object types.

## Known gaps

These are the reasons K0 0.1.0 provides no isolation between tasks.

- **One CNode for everyone.** Every task resolves slot numbers in the same root CNode, so every task holds every capability. This is advisory GHSA-vmpv-qc49-3p6r in [SECURITY.md](../SECURITY.md).
- **No derivation.** There is no capability derivation tree, no `MINT` to make a copy with fewer rights or a badge, and no `revoke`.
- **No transfer.** Capabilities cannot be sent over IPC.
- **No reuse.** Retyped memory is never returned to its untyped capability.

The intended direction, recorded in the README, is per-task CSpaces and `AddrSpace` retype, then the derivation tree with `MINT` and `revoke`, then capability transfer over IPC.
