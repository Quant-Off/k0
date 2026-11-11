# Tasks and scheduling

[한국어](scheduling_KR.md)

This note describes how K0 represents a task, how user space creates, starts and tears down tasks, and how the round-robin scheduler switches between them. The code is in `crates/k0-task/src/lib.rs`, `crates/k0-sched/src/lib.rs`, `teardown` in `crates/k0-ipc/src/lib.rs`, and `sys_tcb_configure`, `sys_tcb_resume`, `kill_current` and `finish` in `kernel/src/syscall.rs`. The note describes the current `main` branch (unreleased).

## The task control block

A task control block (TCB) holds:

- the task's saved user context: 31 general-purpose registers, the stack pointer, the return address, the saved processor state, and the two EL0 thread pointer registers,
- the physical address of the task's top-level page table (`ttbr0_pa`), a copy of the address space root that is valid only while the task's embedded slots are bound,
- the task's state,
- `next` and `prev`, the links to its neighbours in whatever queue the task is in,
- the single-use reply right (`reply_to`) and its reverse link (`reply_from`), described in [ipc.md](ipc.md),
- `blocked_on`, the endpoint the task is queued at, and `ipc_badge`, the badge of the message it is waiting to send, and
- three embedded capability slots. Slot 1 holds the task's `CNode` capability and slot 2 its `AddrSpace` capability. Slot 0 is unused.

The embedded slots are what bind a task to its capability space and its address space. System calls resolve slot numbers only in the CNode named by slot 1 (see [capabilities.md](capabilities.md)), and the task runs in the address space named by slot 2. Both are copies, that is children in the derivation list, of the capabilities given to `TCB_CONFIGURE`, and they are marked as bound to the task. When either one is removed, the task is torn down (see [Teardown](#teardown)).

Every TCB, including the root task's, is an ordinary object at the start of a granule-sized frame. The root task's TCB lives in the 2 MiB boot frame window and is reached through a `Tcb` capability in the root CNode. Every other TCB is a frame that user space retyped from untyped memory. The kernel reaches a TCB only through its own alias, and it is never mapped at EL0. Compile-time assertions keep the TCB within a granule, so that the TCB can be found again by rounding an embedded slot's address down to the granule.

## States

| State | Meaning |
| --- | --- |
| `Inactive` | just retyped, not configured |
| `Stopped` | configured, not yet started |
| `Ready` | in the ready queue |
| `Running` | the current task |
| `BlockedSend`, `BlockedRecv`, `BlockedCall` | waiting in an endpoint queue |
| `BlockedReply` | waiting for the answer to a `CALL` |
| `Dead` | exited, killed, or torn down |

`Inactive` has the value 0. A freshly retyped frame is all zeroes, so it already is a valid `Inactive` TCB with empty queue links, empty embedded slots and no reply right. Retype needs no constructor.

## Creating a task

**`TCB_CONFIGURE`** takes six arguments: the `Tcb` slot, an entry address, a stack top, an `AddrSpace` slot, a `CNode` slot and one argument value that the new task receives in `x0`. All slots are looked up in the caller's own CNode. The kernel checks that:

- the entry address is 4-byte aligned, above the first granule and inside the 48-bit user range,
- the stack top is 16-byte aligned, above the first granule and inside the user range,
- the capabilities are of the right kinds (`Tcb`, `AddrSpace`, `CNode`), and
- the TCB is in the `Inactive` state, so each TCB can be configured only once and a running task can never be reconfigured. The state is read through a raw pointer before any mutable reference to the TCB is formed.

The two bindings are written together. Both capabilities are validated first, and then copies are written into slots 1 and 2 in one step, so a TCB that is bound to only one of the two cannot exist. If any check fails, both embedded slots stay empty and the TCB stays `Inactive`.

The kernel then zeroes the saved context and sets the entry address, the stack pointer, `x0` and `ttbr0_pa`. It sets the saved processor state itself, to EL0 with interrupts unmasked. User space never supplies processor state, so it cannot ask to return to EL1 or to run with interrupts masked. The thread pointer registers start at zero, so the new task does not inherit the creator's values. The TCB moves to `Stopped`.

**`TCB_RESUME`** accepts only a `Stopped` TCB and puts it in the ready queue. A TCB that is already queued, running, blocked or dead is refused, so the same TCB can never sit in a queue twice.

## The ready queue

The scheduler keeps three values: the current task, and the head and tail of the ready queue. Each is the kernel address of a TCB, with 0 meaning none. The queue is linked through the TCBs' own `next` and `prev` fields, so it needs no memory of its own. Its length is bounded by the number of TCBs, which user space paid for with untyped memory. The scheduler cannot become a way to exhaust kernel memory.

The queue is doubly linked so that a task can be removed from anywhere in it in constant time. That matters because a task can be torn down while it is ready, and the kernel must not walk the queue to find it. Removal checks that the links agree with the queue's head and tail. If they do not, the queue is corrupt and the kernel halts with `k0: FATAL ready queue corrupt`.

The same `next` and `prev` fields also link a blocked task into an endpoint queue, which has the same structure. The state machine keeps these uses apart: a task is either ready, or blocked in one endpoint, or running, never two at once.

## Switching tasks

When an exception arrives from EL0, the vector code saves all user registers into the context of the current TCB before any Rust code runs (see [exceptions.md](exceptions.md)). On the way back, it restores registers from whatever context the global `__current_context` pointer names. A task switch is therefore just two steps:

1. Point `__current_context` at the next task's saved context.
2. If the next task uses a different top-level page table, load it into `TTBR0_EL1` and invalidate the whole TLB. K0 does not use address space identifiers, so every switch between tasks in different address spaces flushes the TLB. Switching between tasks in the same address space does not touch `TTBR0_EL1`.

Tasks can run in different address spaces, so `TTBR0_EL1` really changes between them. If an address space is destroyed while it is installed, the kernel first switches `TTBR0_EL1` to an empty table.

Three operations use this switch.

| Operation | Used by | What happens to the current task |
| --- | --- | --- |
| `rotate` | `YIELD`, timer preemption | goes to the tail of the ready queue, unless nothing else is ready, in which case it simply continues |
| `block_and_switch` | IPC, and the system call exit path when the current task died | stays out of the ready queue. The caller has already put it in an endpoint queue or behind a reply right, or it is `Dead`. |
| `handoff` | boot, once | the root task becomes the first `Running` task |

## Preemption

The timer fires once per second. When it fires while EL0 is running, the IRQ path acknowledges it, re-arms the timer, prints `k0: tick N` and calls `k0_preempt`, which calls `rotate`. A task that never yields therefore loses the CPU after at most one second.

The kernel itself is never preempted. Exception entry masks interrupts, and the kernel keeps them masked while it handles a system call or fault. A timer interrupt that arrives at EL1 is possible only during boot, before the handoff, and it only counts the tick.

## Teardown

Teardown makes a task `Dead` and removes every kernel pointer that refers to it. It runs when:

- the task calls `EXIT`,
- the task takes an EL0 fault,
- the task makes an unknown system call,
- the original capability of its TCB is deleted (by `CAP_DELETE`, or by `CAP_REVOKE` on a capability it descends from, such as the untyped it was retyped from), or
- a capability bound to the task is removed, that is its CNode or address space capability was deleted or revoked, or the original object behind it was finalized.

`teardown` decides by the task's state only, never by whether a pointer happens to be set:

| State | What teardown does |
| --- | --- |
| `Dead` | nothing, so it is safe to call twice |
| `Inactive`, `Stopped` | nothing to unlink |
| `Ready` | removes the task from the ready queue |
| `BlockedSend`, `BlockedRecv`, `BlockedCall` | removes the task from the endpoint queue named by `blocked_on` |
| `BlockedReply` | clears the reply right in the task that holds it (`reply_from`) |
| `Running` | nothing is unlinked. See below. |

For every state it then withdraws any reply right the task itself holds, so that the caller waiting for that reply wakes with `NO_REPLY` and does not stay blocked forever. Finally it sets the state to `Dead`, clears the queue links, `blocked_on`, `reply_from`, `ipc_badge` and `ttbr0_pa`, and, unless the task is `Running`, zeroes its saved registers. A queue or reply link that does not match what the state promises is a kernel invariant violation, and the kernel halts with `k0: FATAL ...` (see [exceptions.md](exceptions.md)).

The running task is the one case teardown does not finish. The system call handler holds a mutable reference to the running task's saved context, so teardown only marks the task `Dead`. The system call exit path, `finish`, runs after the handler and after all capability cleanup, including the finalization of everything that was removed. If the current task is `Dead`, `finish` writes no return value into its context, zeroes the context, and switches to the next ready task. A task can therefore delete its own CNode, its own address space or its own TCB, or revoke the untyped it was carved from, without ever resuming afterwards.

A dead task's frame is not freed by teardown. It stays a `Dead` TCB, and `TCB_CONFIGURE` and `TCB_RESUME` refuse it, so a dead task is never restarted. The memory is reusable once the TCB's original capability is deleted, or the untyped it came from is revoked, and the untyped is then retyped, which zeroes the frame again. A task that merely exited keeps its two bound capabilities until then.

## When there is nothing left to run

- If the current task died during a kernel entry and no task is ready, `finish` prints `k0: all tasks exited` and parks the core.
- If the current task blocks in IPC and no task is ready, `block_and_switch` reports it, and the kernel prints `k0: all tasks blocked` and parks. No other event can wake a task today, because notification objects do not exist yet. This is advisory GHSA-q99x-2wrv-358p in [SECURITY.md](../SECURITY.md). Task isolation does not change this: a confined task that is the last one to block can still park the whole system. Isolation protects confidentiality and integrity between tasks, not availability.

Parking is a `wfe` loop inside the kernel, which runs with interrupts masked.

## How it is tested

The scheduler and the system call paths read system registers, so they are tested by booting. The queue and capability logic that does not touch hardware (`k0-cap`, `k0-mm`) is tested on the host, see [capabilities.md](capabilities.md). The root task self-tests:

- create a task that counts to a target and exits, and wait for it,
- refuse to resume an unconfigured task, refuse to resume a task twice, and refuse to resume a dead task,
- start a task that spins forever without yielding, then confirm that the root task still gets the CPU back through timer preemption,
- confirm that thread pointer registers do not leak between tasks,
- build confined tasks that have their own CNode and their own address space, and check that the root task's slot numbers mean nothing there, and
- delete a task in each state (inactive, stopped, ready, blocked in send, receive and call, waiting for a reply, holding a reply right, and at the head, middle and tail of an endpoint queue), delete endpoints with tasks queued, kill a task by revoking its CNode or deleting its address space, and have a task delete its own TCB and revoke the untyped it was carved from.

`tools/qemu-boot-test.sh` requires the markers `root: child exit test pass`, `root: preempt test pass`, `root: isolation tests pass`, `root: violation tests pass`, `root: teardown tests pass`, `root: revocation tests pass` and `root: sched tests pass`, followed by at least one more timer tick. It fails if the log contains `k0: all tasks`, `k0: FATAL` or `k0: PANIC`, so a premature "all tasks exited" fails the run. It also counts task fault lines exactly (see [exceptions.md](exceptions.md)), which catches a task that keeps running after its teardown.

## Known gaps

- Scheduling is plain round robin with a fixed one-second slice. There are no priorities and no budgets.
- There is no idle state. When the root task has finished its self-tests it keeps calling `YIELD`, so the CPU does not sleep. When no task can run, the kernel parks instead of waiting for an event.
- If every task is blocked, the kernel parks the core, and a confined task can cause this by blocking last (GHSA-q99x-2wrv-358p). Task isolation does not cover availability.
- There are no timeouts. A blocked task wakes only when its IPC partner acts or when the endpoint is deleted.
- One core only.
- A dead TCB is not reclaimed by teardown. Its frame becomes reusable only when its capability is deleted or its untyped is revoked and retyped.
- There are no hardware address space identifiers, so every switch between address spaces flushes the whole TLB.
- A task that dies cannot be restarted. A new task needs a new TCB.
- The 16 KiB (Apple) build compiles and passes host tests, but it has not been booted.
