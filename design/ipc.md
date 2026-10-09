# Synchronous IPC

[한국어](ipc_KR.md)

This note describes K0's inter-process communication: endpoints, the four IPC system calls, and the single-use reply right that makes `CALL` safe. The code is in `crates/k0-ipc/src/lib.rs`, with the endpoint queue in `crates/k0-task/src/lib.rs`.

## Design in one paragraph

IPC in K0 is a synchronous rendezvous in the style of seL4. A message is four 64-bit words carried in registers `x2` to `x5`, called MR0 to MR3. When both sides are ready, the kernel copies the four words from the sender's saved context into the receiver's saved context. If one side is not ready, the other waits in the endpoint's queue. The kernel never buffers a message, never allocates memory for IPC, and never reads user memory. That leaves no buffer for user space to fill up, and no user buffer that could change between the kernel's check and its use.

## Endpoints

An endpoint is a kernel object created by `RETYPE` (see [capabilities.md](capabilities.md)). It holds the head and tail of one queue of blocked TCBs, linked through the TCBs' own `next` field. A zeroed endpoint is an empty endpoint.

At any moment the queue holds either waiting senders or waiting receivers, never both. A sender that finds a receiver waiting delivers at once, and a receiver that finds a sender waiting takes the message at once. Only one kind can therefore pile up. The kernel tells the two kinds apart by the state of the TCB at the head of the queue. Queues are first in, first out.

## The four calls

All four take an `Endpoint` capability in `x0` and flags in `x1`. The only flag is `NONBLOCK`: if the other side is not ready, the call returns `WOULD_BLOCK` instead of waiting. There are no timeouts, so `NONBLOCK` is the only way not to wait.

**`SEND`** delivers MR0 to MR3. If a receiver is waiting, the kernel copies the message into the receiver's context, makes the receiver ready, and the sender continues. Otherwise the sender waits in the queue as `BlockedSend` until a receiver arrives.

**`RECV`** waits for a message. If a sender is waiting, the kernel copies its message into the receiver's context at once. The receiver gets `x0 = 0`, `x1 = 0` (reserved for a badge) and the message in `x2` to `x5`. A plain sender is released and made ready. A sender that used `CALL` is not released. It moves on to wait for the reply, as described below.

**`CALL`** sends a message and waits for the reply in a single trap. If a receiver is waiting, the message is delivered and the caller moves straight to `BlockedReply`. Otherwise the caller waits in the queue as `BlockedCall`. Either way, the caller always blocks. Because sending and waiting happen in one step, the reply can never arrive in the gap between them.

**`REPLY_RECV`** is the server loop in one trap. If the calling task holds a reply right, the kernel delivers `x2` to `x5` as the reply to the waiting caller, makes the caller ready, and drops the right. Then it continues exactly like `RECV`. The endpoint is checked first. If the endpoint capability is invalid, the call fails without using up the reply.

## The reply right

When a receiver takes a message from a `CALL`, the kernel records the caller in a field of the receiver's TCB called `reply_to`. That field is the reply right. It is not a capability and does not occupy a slot, so user space can neither forge it, copy it, nor pass it on. Three rules keep it safe.

1. **Single use.** `REPLY_RECV` clears the field the moment it delivers the reply.
2. **At most one per task.** If a task that already holds a reply right receives a new `CALL`, the old caller is woken with `NO_REPLY` before the new right is recorded.
3. **No orphaned callers.** If a task holding a reply right exits or faults, `kill_current` first wakes the caller with `NO_REPLY`.

A caller therefore always wakes up, either with a reply or with `NO_REPLY`, as long as the system keeps running.

## Blocking and deadlock

A task that has to wait is marked with its blocked state, linked into the endpoint queue or held by a reply right, and then the scheduler switches to the next ready task (see [scheduling.md](scheduling.md)). Its return registers are written later, by the task that releases it.

If no task is ready at that point, every task is blocked and nothing can ever wake one, because K0 does not yet have notification objects for interrupts. The kernel prints `k0: all tasks blocked` and parks the core. Stopping the whole system is the fail-secure choice here, but it is also advisory GHSA-q99x-2wrv-358p in [SECURITY.md](../SECURITY.md): one task can halt the machine by blocking last.

## Errors

| Code | Cause |
| --- | --- |
| `BAD_SLOT` | the endpoint slot number is beyond the filled part of the CNode |
| `BAD_CAP` | the slot holds something other than an endpoint |
| `WOULD_BLOCK` | `NONBLOCK` was set and the other side was not ready |
| `NO_REPLY` | the receiver of a `CALL` exited, faulted or took a newer `CALL` without replying |

## How it is tested

IPC is tested by booting. The root task starts a server task and client tasks and checks:

- that `NONBLOCK` returns `WOULD_BLOCK` for each call on an empty endpoint, and that a non-endpoint slot is refused,
- a `SEND` to a waiting receiver and a `SEND` that has to wait for one, with the data checked on arrival,
- `CALL` with a reply, `CALL` queued before the server is ready, and the content of each reply,
- first in, first out order with two queued senders,
- a server that receives a new `CALL` while it still holds a reply right, so that the first caller gets `NO_REPLY`, and
- a server that exits while a caller waits, so that the caller gets `NO_REPLY`.

`tools/qemu-boot-test.sh` requires `root: ipc tests pass`.

## Known gaps

- **No badges.** `x1` is always 0 on receipt, so a server cannot tell its clients apart.
- **No access control between tasks.** Every task shares the root CNode, so any task can send to or receive from any endpoint.
- **No capability transfer** over IPC.
- **No notifications and no timeouts.** A blocked task waits until its partner acts.
- **Four words per message.** Larger data needs shared memory, and with one shared address space today, every task already shares all memory.
