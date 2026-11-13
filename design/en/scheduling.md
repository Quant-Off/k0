# Tasks and scheduling

<div align="center">

English | [한국어](../kr/scheduling_KR.md)

</div>

This note describes the following:

- how K0 represents a task
- how user space creates, starts and tears down tasks
- how the round-robin scheduler switches between tasks and decides when a timer tick cuts a task

The code is in the following places:

- `crates/k0-task/src/lib.rs`
- `crates/k0-sched/src/lib.rs`
- `teardown` in `crates/k0-ipc/src/lib.rs`
- `sys_tcb_configure`, `sys_tcb_resume`, `kill_current` and `finish` in `kernel/src/syscall.rs`

The note describes the current `main` branch (unreleased).

## The task control block

A **task control block** (TCB) holds the following:

- **Saved user context**: 31 general-purpose registers, the stack pointer, the return address, the saved processor state, and the two EL0 thread pointer registers.
- **Top-level page table address** (`ttbr0_pa`): a copy of the address space root that is valid only while the task's embedded slots are bound.
- **State**: the current state of the task.
- **Queue links**: `next` and `prev`, the links to its neighbours in whatever queue the task is in.
- **Reply right**: the single-use reply right (`reply_to`) and its reverse link (`reply_from`), described in [ipc.md](ipc.md).
- **IPC wait information**: `blocked_on`, the endpoint the task is queued at, and `ipc_badge`, the badge of the message it is waiting to send.
- **Timeout fields**: `deadline`, the tick at which the task's IPC wait ends, and `tnext` and `tprev`, the links of the timed waiter list (see [Timeouts](#timeouts)).
- **Three embedded capability slots**: slot 1 holds the task's `CNode` capability and slot 2 its `AddrSpace` capability. Slot 0 is unused.

The **embedded slots** are what bind a task to its capability space and its address space. System calls resolve slot numbers only in the CNode named by slot 1 (see [capabilities.md](capabilities.md)), and the task runs in the address space named by slot 2. Both are copies, that is children in the derivation list, of the capabilities given to `TCB_CONFIGURE`, and they are marked as bound to the task. When either one is removed, the task is torn down (see [Teardown](#teardown)).

Every TCB, including the root task's, is an ordinary object at the start of a granule-sized frame. The root task's TCB lives in the 2 MiB boot frame window and is reached through a `Tcb` capability in the root CNode. Every other TCB is a frame that user space retyped from untyped memory.

The kernel reaches a TCB only through its own alias, and it is never mapped at EL0. Compile-time assertions keep the TCB within a granule, so that the TCB can be found again by rounding an embedded slot's address down to the granule.

## States

The following figure shows the task states and the transitions between them. The meaning of each state is in the table after it.

<p align="center">
  <img src="../../assets/scheduling-states.svg" alt="Inactive becomes Stopped through TCB_CONFIGURE and Ready through TCB_RESUME, round robin moves a task between Running and Ready, a task blocks in IPC and becomes Ready when its partner releases it or its deadline passes, and teardown makes it Dead from any state">
</p>

| State | Meaning |
| --- | --- |
| `Inactive` | just retyped, not configured |
| `Stopped` | configured, not yet started |
| `Ready` | in the ready queue |
| `Running` | the current task |
| `BlockedSend`, `BlockedRecv`, `BlockedCall` | waiting in an endpoint queue |
| `BlockedReply` | waiting for the answer to a `CALL` |
| `BlockedConsole` | waiting for console input in `CONSOLE_READ` (only in an image built with `console-input`). The task is in no queue. One kernel waiter slot points at it, and only one task can wait at a time. |
| `Dead` | exited, killed, or torn down |

A task in any of the blocked states can also become `Ready` when its deadline passes. A `BlockedConsole` task also becomes `Ready` when a byte arrives or when its capability is deleted, revoked or moved (for deadlines see [Timeouts](#timeouts)).

`Inactive` has the value 0. A freshly retyped frame is all zeroes, so it already is a valid `Inactive` TCB with empty queue links, empty embedded slots and no reply right. Retype needs no constructor.

## Creating a task

`TCB_CONFIGURE` takes six arguments: the `Tcb` slot, an entry address, a stack top, an `AddrSpace` slot, a `CNode` slot and one argument value that the new task receives in `x0`. All slots are looked up in the caller's own CNode. The kernel checks that:

- the entry address is 4-byte aligned, above the first granule and inside the 48-bit user range,
- the stack top is 16-byte aligned, above the first granule and inside the user range,
- the capabilities are of the right kinds (`Tcb`, `AddrSpace`, `CNode`), and
- the TCB is in the `Inactive` state, so each TCB can be configured only once and a running task can never be reconfigured. The state is read through a raw pointer before any mutable reference to the TCB is formed.

> [!IMPORTANT]
> The two bindings are written together. Both capabilities are validated first, and then copies are written into slots 1 and 2 in one step, so a TCB that is bound to only one of the two cannot exist.

If any check fails, both embedded slots stay empty and the TCB stays `Inactive`.

The kernel then zeroes the saved context and sets the entry address, the stack pointer, `x0` and `ttbr0_pa`. It sets the saved processor state itself, to EL0 with interrupts unmasked. User space never supplies processor state, so it cannot ask to return to EL1 or to run with interrupts masked. The thread pointer registers start at zero, so the new task does not inherit the creator's values. The TCB moves to `Stopped`.

`TCB_RESUME` accepts only a `Stopped` TCB and puts it in the ready queue. A TCB that is already queued, running, blocked or dead is refused, so the same TCB can never sit in a queue twice.

## The ready queue

The scheduler keeps three values: the current task, and the head and tail of the ready queue. Each is the kernel address of a TCB, with 0 meaning none. The queue is linked through the TCBs' own `next` and `prev` fields, so it needs no memory of its own. Its length is bounded by the number of TCBs, which user space paid for with untyped memory. The scheduler cannot become a way to exhaust kernel memory.

The queue is doubly linked so that a task can be removed from anywhere in it in constant time. That matters because a task can be torn down while it is ready, and the kernel must not walk the queue to find it. Removal checks that the links agree with the queue's head and tail. If they do not, the queue is corrupt and the kernel halts with `k0: FATAL ready queue corrupt`.

The same `next` and `prev` fields also link a blocked task into an endpoint queue, which has the same structure. The state machine keeps these uses apart. A task is either ready, or blocked in one endpoint, or running, never two at once.

## Switching tasks

When an exception arrives from EL0, the vector code saves all user registers into the context of the current TCB before any Rust code runs (see [exceptions.md](exceptions.md)). On the way back, it restores registers from whatever context the global `__current_context` pointer names. A task switch is therefore just two steps:

1. Point `__current_context` at the next task's saved context.
2. If the next task uses a different top-level page table, load it into `TTBR0_EL1` and invalidate the whole TLB. K0 does not use address space identifiers, so every switch between tasks in different address spaces flushes the TLB. Switching between tasks in the same address space does not touch `TTBR0_EL1`.

Tasks can run in different address spaces, so `TTBR0_EL1` really changes between them. If an address space is destroyed while it is installed, the kernel first switches `TTBR0_EL1` to an empty table.

Five operations use this switch, as shown in the following table. Each one passes the entry kind of the next task: `Tick` if the task got the CPU by a tick's own decision, `Between` if it got it between two ticks. The next section explains why the kernel keeps this.

| Operation | Used by | What happens to the current task | Entry kind of the next task |
| --- | --- | --- | --- |
| `rotate` | `YIELD` | goes to the tail of the ready queue, unless nothing else is ready, in which case it simply continues | `Between` |
| `on_tick` | timer preemption | if it got the CPU between two ticks and has met no tick yet, it continues and the exemption is used up. Otherwise it goes to the tail of the ready queue, unless nothing else is ready | `Tick` |
| `block_and_switch` | IPC, `CONSOLE_READ`, and the system call exit path when the current task died | stays out of the ready queue. The caller has already put it in an endpoint queue, behind a reply right or in the console waiter slot, or it is `Dead`. | `Between` |
| `resume_from_idle` | the idle loop | (the current task is blocked or dead) | `Tick` if a tick passed while idle, otherwise `Between` |
| `handoff` | boot, once | the root task becomes the first `Running` task | `Between` |

## Preemption

The timer fires once per second. When it fires while EL0 is running, the IRQ path acknowledges it, re-arms the timer, prints `k0: tick N` and calls `k0_preempt`. That function wakes the console and timeout waiters that are due and then calls `k0_sched::on_tick`, which decides whether this tick cuts the current task (see [The slice rule](#the-slice-rule)). The `k0: tick N` line goes to the console in an image built with a console driver, and only the first 4 tick lines are logged. A task that never yields or blocks therefore **loses the CPU after at most two seconds**, and after one second if it got the CPU at a tick. Once a task has read console input, the tick lines stop after one line `k0: tick lines off (console input in use)`, but the tick, the preemption and the timeouts go on.

> [!IMPORTANT]
> The kernel itself is never preempted. Exception entry masks interrupts, and the kernel keeps them masked while it handles a system call or fault.

A tick still cuts a task only when another task is ready. A timer interrupt that arrives at EL1 is possible only during boot, before the handoff, or while the kernel idles, and it only counts the tick. `on_tick` is called exactly once for every tick taken from EL0. In an image with `console-input`, an arriving input byte does not preempt: the woken reader goes to the end of the ready queue like any other woken task. There are no priorities, so input latency is up to one tick per ready task, plus one more tick when the running task got the CPU between two ticks.

## The slice rule

The timer is armed only in the tick handler, never when the kernel switches tasks. A task that gets the CPU between two ticks therefore receives only the rest of a period. Before this rule, the next tick moved it to the end of the queue however short a time it had run. A task could call `YIELD`, which needs no capability, shortly before each tick. The task behind it then got only the sliver after the yield, and the yielding task was back at the head of the queue for the next full period. In a QEMU run with two CPU-bound tasks, the other task fell from about 52 % of the CPU to between 7.9 % and 11.1 % when the attacker yielded at 90 % of the period. The yield point in that run was set from a loop count that the root task had measured for the attacker. No attacker that finds the tick phase by itself was built.

The rule is in the crate `k0-slice`, which only answers the question "does this tick cut the current task". The scheduler keeps one mark next to the current task and writes it only where the current task changes:

- A task that gets the CPU **between two ticks** (`Entry::Between`) is not cut by the first tick it meets. That tick uses up the exemption, and the task runs on to the tick after it. This covers a previous task that called `YIELD`, blocked (IPC or `CONSOLE_READ`), exited or was killed, and a kernel that was idle and woke for something that was not a tick, such as console input.
- A task that gets the CPU **by a tick's own decision** (`Entry::Tick`) starts at the beginning of a period and is cut by the next tick, as before. That includes a task the idle kernel switches to after a tick woke it, for example an expired timeout. If a tick and console input arrive in the same wake-up, `Entry::after_idle` counts it as a tick.
- The root task at boot counts as entering between two ticks, because the timer already runs during boot.
- Waking a task never grants the exemption. A woken task goes to the end of the ready queue as before, and console input still does not preempt.
- `YIELD` with nothing else ready changes nothing.

`Slice::entered` assigns the mark from the entry kind alone and never reads the previous value, so a mark left by the previous task cannot survive a switch. Blocking and exiting grant the exemption as well as `YIELD`. If only `YIELD` did, two tasks that start timed waits just before each tick would starve the task between them (the model shows this case).

What follows from the rule:

- A task that was switched in is not cut by the timer before it has had one full tick period, unless it gives up the CPU itself. Before a forced preemption it runs at least one period and at most two.
- With N ready tasks, a task that never yields or blocks keeps at least 1/N of the CPU in the long run, whatever the other tasks do with `YIELD`, blocking and timed waits. This was checked by search in a model (see [How it is tested](#how-it-is-tested)). It is **not a proof**, and it is **not a budget**: nothing caps what a task may use, and N grows with every task that is created.
- It costs latency. A task that becomes ready with k tasks to run before it (the running one included) used to start within k tick periods and now starts within k + 1. In the model, a caller and a server that exchange calls and replies behind one CPU-bound task need 4 ticks per round trip instead of 2. With no CPU-bound task nothing changes.

> [!NOTE]
> The numbers in this section (one to two periods per run, k + 1 periods of waiting) describe this version of the scheduler. They are not an interface. The tick is still one second and timeouts are still counted in ticks.

## Timeouts

The tick is the unit of every IPC timeout, as described in [ipc.md](ipc.md). The scheduler keeps the waiters that have a deadline in a timed list (`arm_timeout`, `disarm_timeout`, `expired`, `has_timed` in `k0-sched`). The list is unsorted, and a lower bound of the earliest deadline lets most ticks skip it.

A task leaves a blocked state only through `k0_sched::enqueue` (wake-up) or `k0_ipc::teardown` (death), and both remove it from the timed list. On every timer tick, `k0_ipc::expire_timeouts` wakes the waiters whose deadline has passed. It runs from `k0_preempt` when a task was running, and from the idle loop when none was. A timeout of n ticks ends at the (n+1)-th tick after the call.

> [!NOTE]
> A timeout says when a task becomes ready, not when it runs. The woken task joins the tail of the ready queue like any other.

## Teardown

Teardown makes a task `Dead` and removes every kernel pointer that refers to it. It runs in the following cases:

- the task calls `EXIT`
- the task takes an EL0 fault
- the task makes an unknown system call
- the original capability of its TCB is deleted (by `CAP_DELETE`, or by `CAP_REVOKE` on a capability it descends from, such as the untyped it was retyped from)
- a capability bound to the task is removed, that is its CNode or address space capability was deleted or revoked, or the original object behind it was finalized

> [!IMPORTANT]
> `teardown` decides by the task's state only, never by whether a pointer happens to be set.

The following table shows what teardown does in each state.

| State | What teardown does |
| --- | --- |
| `Dead` | nothing, so it is safe to call twice |
| `Inactive`, `Stopped` | nothing to unlink |
| `Ready` | removes the task from the ready queue |
| `BlockedSend`, `BlockedRecv`, `BlockedCall` | removes the task from the endpoint queue named by `blocked_on`, and from the timed list if it has a deadline |
| `BlockedReply` | clears the reply right in the task that holds it (`reply_from`), and removes the task from the timed list if it has a deadline |
| `BlockedConsole` | clears the kernel's console waiter slot and removes the task from the timed list if it has a deadline |
| `Running` | nothing is unlinked. See below. |

For every state it then withdraws any reply right the task itself holds, so that the caller waiting for that reply wakes with `NO_REPLY` and does not stay blocked forever. Finally it sets the state to `Dead`, clears the queue links, `blocked_on`, `reply_from`, `ipc_badge` and `ttbr0_pa`, and, unless the task is `Running`, zeroes its saved registers. A queue or reply link that does not match what the state promises is a kernel invariant violation, and the kernel halts with `k0: FATAL ...` (see [exceptions.md](exceptions.md)).

The running task is the one case teardown does not finish. The system call handler holds a mutable reference to the running task's saved context, so teardown only marks the task `Dead`. The system call exit path, `finish`, runs after the handler and after all capability cleanup, including the finalization of everything that was removed. If the current task is `Dead`, `finish` writes no return value into its context, zeroes the context, and switches to the next ready task. A task can therefore delete its own CNode, its own address space or its own TCB, or revoke the untyped it was carved from, without ever resuming afterwards.

A dead task's frame is not freed by teardown. It stays a `Dead` TCB, and `TCB_CONFIGURE` and `TCB_RESUME` refuse it, so a dead task is never restarted. The memory is reusable once the TCB's original capability is deleted, or the untyped it came from is revoked, and the untyped is then retyped, which zeroes the frame again. A task that merely exited keeps its two bound capabilities until then.

## When there is nothing left to run

The kernel no longer parks the core when no task can run. If the current task blocked or died and no task is ready, `idle_until_ready` in `kernel/src/syscall.rs` takes over:

1. It waits for the next interrupt (`k0_arch::irq::wait_for_interrupt`: `wfi` with IRQs masked, then a short unmask so that the EL1 vector takes the interrupt).
2. It wakes every IPC waiter whose deadline has passed.
3. It switches to a ready task if there is one (`resume_from_idle`, which tells the slice rule whether a tick passed while idle), and otherwise waits again.

Timer ticks keep being counted and handled while the kernel idles. The idle loop prints one of two lines, each at most once per boot:

- `k0: idle (waiting for a timeout)`: at least one waiter has a deadline.
- `k0: all tasks stopped (none ready, no timeout pending)`: no waiter has a deadline and no task waits for console input. Nothing can wake a task in that state today, because there are no notification objects and no device interrupts for tasks other than console input, so the machine stays idle.

When the only thing left to wait for is console input (an image with `console-input`), the kernel prints nothing: that is the normal state of a terminal image. The idle loop also looks for arrived input after each interrupt and wakes the reader.

> [!WARNING]
> Availability is still not protected. A task that waits without a timeout can wait forever if its partner never acts, and a confined task that is the last one to block can still leave the system with nothing to run (advisory GHSA-q99x-2wrv-358p in [SECURITY.md](../../SECURITY.md), updated at release time). Isolation protects confidentiality and integrity between tasks, not availability.

An SError taken from EL0 still stops the whole machine, and so does a `k0_arch::fatal` or a kernel panic.

## How it is tested

The scheduler and the system call paths read system registers, so they are tested by booting. The queue and capability logic that does not touch hardware (`k0-cap`, `k0-mm`) is tested on the host, see [capabilities.md](capabilities.md). The root task self-tests:

- create a task that counts to a target and exits, and wait for it,
- refuse to resume an unconfigured task, refuse to resume a task twice, and refuse to resume a dead task,
- run the slice test (`userspace/root-task/src/timeslice.rs`), which shows that a task which never yields is still preempted by the timer and that a task which got the CPU by a yield or by a block is not cut at its first tick,
- confirm that thread pointer registers do not leak between tasks,
- build confined tasks that have their own CNode and their own address space, and check that the root task's slot numbers mean nothing there, and
- delete a task in each state (inactive, stopped, ready, blocked in send, receive and call, waiting for a reply, holding a reply right, and at the head, middle and tail of an endpoint queue), delete endpoints with tasks queued, kill a task by revoking its CNode or deleting its address space, and have a task delete its own TCB and revoke the untyped it was carved from.

`tools/qemu-boot-test.sh` requires the markers `root: child exit test pass`, `root: slice test yield pass`, `root: slice test block pass`, `root: preempt test pass`, `root: isolation tests pass`, `root: violation tests pass`, `root: teardown tests pass`, `root: revocation tests pass` and `root: sched tests pass`, followed by at least one more timer tick. The markers also include `k0: idle (waiting for a timeout)` and `root: timeout tests pass`, which come from the timeout tests (see [ipc.md](ipc.md)). It fails if the log contains a line that starts with `k0: all tasks`, `k0: FATAL` or `k0: PANIC`, so a run in which the kernel ends up with nothing to run and no timeout pending fails. It also counts task fault lines exactly (see [exceptions.md](exceptions.md)), which catches a task that keeps running after its teardown.

The slice test replaces the old test with a busy child and a counter. EL0 has no clock, so it judges by order of execution, not by time. It starts four tasks: one that hands the CPU over (by `YIELD` in the first run, by blocking in the second), one that then spins for ever, one that leaves a mark the first time it runs, and a helper. The root task goes into a timed wait of one tick. If the spinning task is cut at its first tick, the marker task runs and the spinner sees the mark before the root task wakes. If it is not cut, the root task wakes first. The test logs `root: slice test yield pass`, `root: slice test block pass` and then `root: preempt test pass`, and on failure `root: FAIL slice: a task that got the cpu by a yield was cut at its first tick` (or `by a block`). Each run takes about 2 ticks, so the two runs add about 4 seconds to every boot test where the old test took about 1.

The rule itself is tested on the host in the crate `k0-slice` (9 tests, run by `tools/host-test.sh`). The tests run the rule on a model of the scheduler with a first in first out ready queue, ticks, timed waits, input wake-ups at arbitrary times, exits and the idle case:

- A check written separately from the rule agrees with every tick decision over 4,000 random schedules, in both orders in which a system call and a tick can fall on the same instant, and no preempted run was shorter than one period.
- The old rule and nine wrong ways of connecting the rule to the scheduler are each caught by that check.
- The share of a CPU-bound task holds against one yielding task at every percent of the period, against up to three cooperating tasks over a grid of yields and timed waits, and against 3,000 sets of random tasks.
- The two measured cases are reproduced (10 % and 55 % at a 90 % yield, and with three tasks 5 % for the task behind the yielder under the old rule).

This is a search in a bounded model (up to 6 tasks), **not a proof**. Each of nine deliberate breaks of the rule fails at least one host test. With the kernel connection broken on purpose, the boot test fails for the old rule, for `YIELD` passing the wrong entry kind and for blocking passing the wrong entry kind. The boot test would not see the tick's own switch passing `Between`, the idle loop reporting the wrong thing about a tick, or the wrong entry kind at `handoff`. Those mistakes change how long a task runs before it is cut, not its share, and the host model covers the rule for these cases but not the kernel's use of it.

With the change in QEMU, an attacker that yields at 90 % of the period left the other task 53.8 % to 54.9 % of the CPU (three runs). With three tasks no task was below 29.7 %. With an attacker that starts a timed wait just before the tick instead of yielding, the other task had 56.5 %. Everything ran in QEMU (TCG and HVF), nothing ran on hardware, and the yield point was set for the attacker, so what an attacker needs to find it by itself was not measured.

## Known gaps

> [!NOTE]
> Priorities and CPU budgets do not exist yet. There is one core only.

- **Scheduling**: round robin with a one-second tick and the slice rule. There is no priority and no CPU budget: a CPU-bound task uses its whole turn and nothing caps it. A task that holds untyped memory can still create many tasks and dilute everybody's share.
- **Timing channels**: they are open. A task still changes when, and now also for how long, another task runs by choosing when to yield or block. This has not been analysed. Closing it would need fixed time partitions, which K0 does not have.
- **Latency**: there is no real-time or latency promise. The tick is one second and the kernel is not preemptible.
- **Slice rule checks**: the host model covers the rule, not the kernel's use of it (see [How it is tested](#how-it-is-tested)), and the measurements ran only in QEMU with an attacker whose yield point was set for it.
- **Timeouts**: they say when a task becomes ready, not when it runs. They are opt-in per call, so a task that waits without one can wait forever.
- **Idle state**: it exists, but while the root task keeps calling `YIELD` after its self-tests, the CPU does not sleep. When no task is ready and no waiter has a deadline, the machine stays idle and nothing can wake a task.
- **Availability**: a task that blocks without a timeout, including a confined one, can stay blocked for good if its partner never acts (GHSA-q99x-2wrv-358p). The kernel is not preemptible, so a long system call such as `CAP_REVOKE` on an untyped with many objects delays every other task and every timeout. Task isolation does not cover availability.
- **Blocked tasks**: a blocked task wakes when its IPC partner acts, when its deadline passes, or when the endpoint is deleted.
- **Dead TCB**: it is not reclaimed by teardown. Its frame becomes reusable only when its capability is deleted or its untyped is revoked and retyped.
- **Address space identifiers**: there are no hardware address space identifiers, so every switch between address spaces flushes the whole TLB.
- **Dead task**: it cannot be restarted. A new task needs a new TCB.
