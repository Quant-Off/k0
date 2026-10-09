# Tasks and scheduling

[한국어](scheduling_KR.md)

This note describes how K0 represents a task, how user space creates and starts tasks, and how the round-robin scheduler switches between them. The code is in `crates/k0-task/src/lib.rs`, `crates/k0-sched/src/lib.rs`, and `sys_tcb_configure`, `sys_tcb_resume` and `kill_current` in `kernel/src/main.rs`.

## The task control block

A task control block (TCB) holds:

- the task's saved user context: 31 general-purpose registers, the stack pointer, the return address, the saved processor state, and the two EL0 thread pointer registers,
- the physical address of the task's top-level page table,
- the task's state,
- a link to the next TCB in whatever queue the task is in, and
- the single-use reply right, described in [ipc.md](ipc.md).

The root task's TCB is a static in the kernel image. Every other TCB lives at the start of a granule-sized frame that user space retyped from untyped memory (see [capabilities.md](capabilities.md)). The kernel reaches it only through its own alias, and it is never mapped at EL0. A compile-time assertion keeps the TCB smaller than a granule.

## States

| State | Meaning |
| --- | --- |
| `Inactive` | just retyped, not configured |
| `Stopped` | configured, not yet started |
| `Ready` | in the ready queue |
| `Running` | the current task |
| `BlockedSend`, `BlockedRecv`, `BlockedCall` | waiting in an endpoint queue |
| `BlockedReply` | waiting for the answer to a `CALL` |
| `Dead` | exited or killed |

`Inactive` has the value 0. A freshly retyped frame is all zeroes, so it already is a valid `Inactive` TCB with an empty queue link and no reply right. Retype needs no constructor.

## Creating a task

**`TCB_CONFIGURE`** takes a `Tcb` capability, an entry address, a stack top and an `AddrSpace` capability. The kernel checks that:

- the entry address is 4-byte aligned, above the first granule and inside the 48-bit user range,
- the stack top is 16-byte aligned, above the first granule and inside the user range,
- the TCB is in the `Inactive` state, so each TCB can be configured only once and a running task can never be reconfigured, and
- the capability is a retyped `Tcb`. The root task's own `RootTcb` is refused.

The kernel then zeroes the saved context and sets the entry address and stack pointer. It sets the saved processor state itself, to EL0 with interrupts unmasked. User space never supplies processor state, so it cannot ask to return to EL1 or to run with interrupts masked. The thread pointer registers start at zero, so the new task does not inherit the creator's values. The TCB moves to `Stopped`.

**`TCB_RESUME`** accepts only a `Stopped` TCB and puts it in the ready queue. A TCB that is already queued, running, blocked or dead is refused, so the same TCB can never sit in a queue twice.

## The ready queue

The scheduler keeps three values: the current task, and the head and tail of the ready queue. Each is the kernel address of a TCB, with 0 meaning none. The queue is linked through the TCBs' own `next` field, so it needs no memory of its own. Its length is bounded by the number of TCBs, which user space paid for with untyped memory. The scheduler cannot become a way to exhaust kernel memory.

The same `next` field also links a blocked task into an endpoint queue. The state machine keeps these uses apart: a task is either ready, or blocked in one endpoint, or running, never two at once.

## Switching tasks

When an exception arrives from EL0, the vector code saves all user registers into the context of the current TCB before any Rust code runs (see [exceptions.md](exceptions.md)). On the way back, it restores registers from whatever context the global `__current_context` pointer names. A task switch is therefore just two steps:

1. Point `__current_context` at the next task's saved context.
2. If the next task uses a different top-level page table, load it into `TTBR0_EL1` and invalidate the whole TLB. K0 does not use address space identifiers yet.

Four operations use this switch.

| Operation | Used by | What happens to the current task |
| --- | --- | --- |
| `rotate` | `YIELD`, timer preemption | goes to the tail of the ready queue, unless nothing else is ready, in which case it simply continues |
| `block_and_switch` | IPC | stays out of the ready queue. The caller has already put it in an endpoint queue or behind a reply right. |
| `exit_current` | `EXIT`, faults | becomes `Dead` |
| `handoff` | boot, once | the root task becomes the first `Running` task |

## Preemption

The timer fires once per second. When it fires while EL0 is running, the IRQ path acknowledges it, re-arms the timer, prints `k0: tick N` and calls `k0_preempt`, which calls `rotate`. A task that never yields therefore loses the CPU after at most one second.

The kernel itself is never preempted. Exception entry masks interrupts, and the kernel keeps them masked while it handles a system call or fault. A timer interrupt that arrives at EL1 is possible only during boot, before the handoff, and it only counts the tick.

## When there is nothing left to run

- If the current task exits or is killed and no task is ready, `exit_current` reports it, and the kernel prints `k0: all tasks exited` and parks.
- If the current task blocks in IPC and no task is ready, `block_and_switch` reports it, and the kernel prints `k0: all tasks blocked` and parks. No other event can wake a task today, because notification objects do not exist yet. This is advisory GHSA-q99x-2wrv-358p in [SECURITY.md](../SECURITY.md).

Before a task is killed, `kill_current` withdraws any reply right the task holds. The task waiting for that reply is woken with `NO_REPLY` and does not stay blocked forever.

## How it is tested

These paths read system registers, so they are tested by booting, not on the host. The root task self-tests:

- create a task that counts to a target and exits, and wait for it,
- refuse to resume an unconfigured task, refuse to resume a task twice, and refuse to resume a dead task,
- start a task that spins forever without yielding, then confirm that the root task still gets the CPU back through timer preemption, and
- confirm that thread pointer registers do not leak between tasks.

`tools/qemu-boot-test.sh` requires `root: child exit test pass`, `root: preempt test pass` and `root: sched tests pass`, followed by at least one more timer tick.

## Known gaps

- Scheduling is plain round robin with a fixed one-second slice. There are no priorities and no budgets.
- There is no idle state. The root task ends its self-tests in a loop that keeps calling `YIELD`, so the CPU never sleeps.
- There are no timeouts. A blocked task wakes only when its IPC partner acts.
- One core only.
- A dead TCB is never reclaimed, and its memory is never returned to the untyped capability.
- Every task runs in the root task's address space, so in practice `TTBR0_EL1` never changes on a switch.
