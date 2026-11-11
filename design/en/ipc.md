# Synchronous IPC

<div align="center">

English | [한국어](../kr/ipc_KR.md)

</div>

This note describes K0's inter-process communication: endpoints, the four IPC system calls, and the badge that tells a server its clients apart, and the single-use reply right that makes `CALL` safe. The code is in `crates/k0-ipc/src/lib.rs`, with the endpoint queue in `crates/k0-task/src/lib.rs`.

## Design in one paragraph

IPC in K0 is a synchronous rendezvous in the style of seL4. A message is four 64-bit words carried in registers `x2` to `x5`, called MR0 to MR3. When both sides are ready, the kernel copies the four words from the sender's saved context into the receiver's saved context. If one side is not ready, the other waits in the endpoint's queue. The kernel never buffers a message, never allocates memory for IPC, and never reads user memory. That leaves no buffer for user space to fill up, and no user buffer that could change between the kernel's check and its use.

## Endpoints

An endpoint is a kernel object created by `RETYPE` (see [capabilities.md](capabilities.md)). It holds the head and tail of one queue of blocked TCBs, linked through the TCBs' own `next` and `prev` fields. A zeroed endpoint is an empty endpoint.

The queue is doubly linked. A TCB also records which endpoint it waits on (`blocked_on`), so a task can be taken out of the middle of a queue in constant time when it is destroyed. Removal checks that the links are consistent and halts the kernel with `k0: FATAL` if they are not.

At any moment the queue holds either waiting senders or waiting receivers, never both. A sender that finds a receiver waiting delivers at once, and a receiver that finds a sender waiting takes the message at once. Only one kind can therefore pile up. The kernel tells the two kinds apart by the state of the TCB at the head of the queue. Queues are first in, first out.

### Finding the endpoint, and rights

The slot number in `x0` is looked up only in the calling task's own CNode (see [capabilities.md](capabilities.md)). A slot number means nothing outside the CNode of the task that uses it, so a task can reach only the endpoints whose capabilities it holds. The capability must carry the right the call needs: `W` for `SEND` and `CALL`, `R` for `RECV` and `REPLY_RECV`. A capability without that right is refused with `NO_RIGHTS`. Rights can only be removed, and only by `CAP_COPY`, so a server can hand out send-only capabilities to its clients and keep the receive side for itself.

### Badges

An endpoint capability carries a badge, a 64-bit value where 0 means none. The badge is set once, by `CAP_COPY` on an endpoint capability that has no badge yet. Setting a badge on any other capability, or a different badge on an already badged one, fails with `BAD_BADGE`. Copies of a badged capability keep its badge.

When a message is received, the kernel puts the badge of the sender's capability into the receiver's `x1`. The sender never supplies it. The kernel reads it from the capability the sender used, so a client cannot forge another client's badge. A sender that has to wait in the queue keeps the badge of its send in its TCB (`ipc_badge`), so the badge delivered is the one of the call that queued it. On the reply path `x1` is always 0.

## The four calls

All four take an `Endpoint` capability in `x0` and flags in `x1`. The only flag is `NONBLOCK`: if the other side is not ready, the call returns `WOULD_BLOCK` instead of waiting. There are no timeouts, so `NONBLOCK` is the only way not to wait.

**`SEND`** delivers MR0 to MR3. If a receiver is waiting, the kernel copies the message into the receiver's context, makes the receiver ready, and the sender continues. Otherwise the sender waits in the queue as `BlockedSend` until a receiver arrives.

**`RECV`** waits for a message. If a sender is waiting, the kernel copies its message into the receiver's context at once. The receiver gets `x0 = 0`, `x1 = ` the badge of the sender's capability, and the message in `x2` to `x5`. A plain sender is released and made ready. A sender that used `CALL` is not released. It moves on to wait for the reply, as described below.

**`CALL`** sends a message and waits for the reply in a single trap. If a receiver is waiting, the message is delivered and the caller moves straight to `BlockedReply`. Otherwise the caller waits in the queue as `BlockedCall`. Either way, the caller always blocks. Because sending and waiting happen in one step, the reply can never arrive in the gap between them.

**`REPLY_RECV`** is the server loop in one trap. If the calling task holds a reply right, the kernel delivers `x2` to `x5` as the reply to the waiting caller, makes the caller ready, and drops the right. Then it continues exactly like `RECV`. The endpoint is checked first. If the endpoint capability is invalid or lacks `R`, the call fails without using up the reply. The reply is delivered with `x1 = 0`.

## The reply right

When a receiver takes a message from a `CALL`, the kernel records the caller in a field of the receiver's TCB called `reply_to`. That field is the reply right. It is not a capability and does not occupy a slot, so user space can neither forge it, copy it, nor pass it on. Three rules keep it safe.

1. **Single use.** `REPLY_RECV` clears the field the moment it delivers the reply.
2. **At most one per task.** If a task that already holds a reply right receives a new `CALL`, the old caller is woken with `NO_REPLY` before the new right is recorded.
3. **No orphaned callers.** If a task holding a reply right exits, faults or is destroyed, the kernel first wakes the caller with `NO_REPLY`.

The right is linked in both directions: the server's `reply_to` names the caller, and the caller's `reply_from` names the server. That lets either side be destroyed without leaving a pointer behind. A caller therefore always wakes up, either with a reply or with `NO_REPLY`, as long as the system keeps running.

## Destroying tasks and endpoints

A task is torn down when it exits, faults, makes an unknown system call, when its TCB's original capability is removed, or when the CNode or address space bound to it is revoked or deleted. The teardown looks only at the task's state and cuts every kernel pointer to it:

- **Ready:** removed from the ready queue.
- **Blocked in `SEND`, `RECV` or `CALL`:** removed from the queue of the endpoint it waits on, whether it is the head, the middle or the tail.
- **Waiting for a reply (`BlockedReply`):** the reply right is cut in both directions, so the server holds no right to answer it and a late reply cannot wake a dead task.
- **Holding a reply right:** the caller waiting for it wakes with `NO_REPLY`.
- **Inactive, stopped or running:** nothing to unlink.

The state then becomes `Dead` and the saved registers are zeroed. If the task that made the system call is the one that died, the kernel never writes a return value into it and switches to the next ready task.

When the original capability of an endpoint is deleted, every task still queued on it wakes with `REVOKED`, and no TCB points at the endpoint afterwards. Deleting only a copy of the capability does not touch the queue.

## Blocking and deadlock

A task that has to wait is marked with its blocked state, linked into the endpoint queue or held by a reply right, and then the scheduler switches to the next ready task (see [scheduling.md](scheduling.md)). Its return registers are written later, by the task that releases it.

If no task is ready at that point, every task is blocked and nothing can ever wake one, because K0 does not yet have notification objects for interrupts. The kernel prints `k0: all tasks blocked` and parks the core. Stopping the whole system is the fail-secure choice here, but it is also advisory GHSA-q99x-2wrv-358p in [SECURITY.md](../../SECURITY.md): one task can halt the machine by blocking last. This is unchanged by task isolation. A confined task that holds a capability to an endpoint can still do it, and isolation does not cover availability.

## Errors

| Code | Cause |
| --- | --- |
| `BAD_SLOT` | the slot number is 0, beyond the calling task's CNode, or empty there |
| `BAD_CAP` | the slot holds something other than an endpoint |
| `NO_RIGHTS` | the endpoint capability lacks `W` (`SEND`, `CALL`) or `R` (`RECV`, `REPLY_RECV`) |
| `WOULD_BLOCK` | `NONBLOCK` was set and the other side was not ready |
| `NO_REPLY` | the receiver of a `CALL` exited, faulted or took a newer `CALL` without replying, or was destroyed |
| `REVOKED` | the original capability of the endpoint was deleted while the caller was queued on it |

## How it is tested

IPC and teardown are tested by booting. The root task starts a server task and client tasks and checks:

- that `NONBLOCK` returns `WOULD_BLOCK` for each call on an empty endpoint, and that a non-endpoint slot is refused,
- a `SEND` to a waiting receiver and a `SEND` that has to wait for one, with the data checked on arrival,
- `CALL` with a reply, `CALL` queued before the server is ready, the content of each reply, and `x1 = 0` on the reply,
- first in, first out order with two queued senders,
- a server that receives a new `CALL` while it still holds a reply right, so that the first caller gets `NO_REPLY`, and
- a server that exits while a caller waits, so that the caller gets `NO_REPLY`.

The capability tests check that a badge can be set once and is refused a second time, that a copy keeps the badge, that a send-only capability cannot receive and a receive-only one cannot send (`NO_RIGHTS`), and that revoking an endpoint removes every copy while the original stays.

The isolation tests start a confined task whose CNode holds only a send-only, badged endpoint capability. The root task checks that the task receives the badge it was given in `x1` and that slot numbers of the root task mean nothing in the confined CNode (`BAD_SLOT`).

The teardown tests delete tasks in each state and check the queues afterwards:

- a task blocked alone in a queue, and the middle, tail and head of a queue of four, with the survivor's message still arriving,
- ready tasks, an inactive TCB and a stopped TCB whose CNode is revoked,
- a task blocked in `RECV` and a task queued by `CALL`,
- a caller waiting for a reply, so that the server's reply right is withdrawn,
- a server that holds a reply right, so that the caller gets `NO_REPLY`, and
- an endpoint deleted with a receiver queued, and with a sender and a caller queued, so that every waiter gets `REVOKED`.

The boot test script also requires an exact number of `k0: task fault` lines and exactly one `k0: unknown syscall` line, so a task that keeps running after it was destroyed fails the test.

`tools/qemu-boot-test.sh` requires `root: ipc tests pass`.

## Known gaps

- **A queued sender delivers once after revocation.** A sender that is already waiting on an endpoint keeps the badge of the send that queued it. If its capability is revoked while it waits, its message is still delivered once to the next receiver.
- **No capability transfer** over IPC.
- **No notifications and no timeouts.** A blocked task waits until its partner acts.
- **Four words per message.** Larger data needs shared memory, which the holder of a task's `AddrSpace` capability must map into that task's address space, since tasks can now run in separate address spaces.
