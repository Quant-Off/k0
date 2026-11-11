# Exceptions, system calls and fault isolation

[한국어](exceptions_KR.md)

After the handoff, kernel code runs only in response to an exception. This note describes the vector table, how user state is saved and restored, how system calls are dispatched, and how a faulting task is contained. The code is in `crates/k0-arch/src/vectors.S`, `vectors.rs`, `usermode.rs` and `irq.rs`, and in `k0_syscall`, `k0_fault`, `k0_preempt` and `finish` in `kernel/src/syscall.rs`. The note describes the current `main` branch (unreleased).

## Mechanism and policy

`k0-arch` contains the mechanism: saving registers, decoding the exception, acknowledging interrupts. The decisions live in the kernel binary. `k0-arch` declares three external functions, `k0_syscall`, `k0_fault` and `k0_preempt`, and the kernel binary defines them. Every policy that affects a task is therefore in one file, `kernel/src/syscall.rs`. Object finalization after a capability is removed (the `Reaper` callback) is in `kernel/src/object.rs`.

## The vector table

AArch64 has 16 vector entries: four kinds of exception (synchronous, IRQ, FIQ, SError) for each of four origins. K0 gives each entry one of three behaviors.

| Origin | Sync | IRQ | FIQ | SError |
| --- | --- | --- | --- | --- |
| EL1 using `SP_EL0` | fatal | fatal | fatal | fatal |
| EL1 using `SP_EL1` | fatal | handled, returns | handled, returns | fatal |
| EL0, AArch64 | saved, handled, returns | saved, handled, returns | saved, handled, returns | fatal |
| EL0, AArch32 | fatal | fatal | fatal | fatal |

The kernel only ever runs on `SP_EL1`, so the `SP_EL0` row should never fire. K0 runs no 32-bit code. An SError means the hardware reported an asynchronous error, and the kernel does not try to recover from one.

### The fatal path

A fatal entry does not trust the stack pointer, because the fault may have been a stack overflow. It switches to a dedicated 8 KiB exception stack and calls `exception_fatal`, which prints the vector kind, `ESR_EL1` with a decoded exception class, `ELR_EL1`, `FAR_EL1` and `SPSR_EL1`, then parks the core. The output uses only raw string and hexadecimal printing, so it also works for a fault before the jump to the higher half.

### Invariant violations: `k0_arch::fatal`

The fatal vector path above is for exceptions the CPU raises. `k0_arch::fatal` is for the kernel's own checks. When kernel-only data cannot be in the state it is in (a capability slot that does not decode, a broken derivation list link, a corrupt user page table, a corrupt ready, endpoint or reply link), continuing would mean making access decisions from damaged state. `fatal` masks all exceptions, prints `k0: FATAL <what>` and parks the core with a `wfe` loop. It does not switch stacks and prints no registers, because it is called from ordinary kernel code on a valid stack. The boot test fails if `k0: FATAL` appears.

### Interrupts while in the kernel

An IRQ or FIQ taken at EL1 saves the caller-saved registers on the kernel stack, calls the handler and returns. After the handoff this does not happen, because exception entry masks interrupts and the kernel never unmasks them while handling an exception. The kernel itself is never preempted.

## Saving and restoring user state

When an exception arrives from EL0, the entry stub saves `x0` and `x1` on the kernel stack and jumps to `__lower_common` with the handler's address. `__lower_common` loads the global pointer `__current_context`, which always points at the current task's saved context inside its TCB, and stores the whole user state there:

| Offset | Content |
| --- | --- |
| 0 to 240 | `x0` to `x30` |
| 248 | `SP_EL0`, the user stack pointer |
| 256 | `ELR_EL1`, the user program counter |
| 264 | `SPSR_EL1`, the user processor state |
| 272 | `TPIDR_EL0` |
| 280 | `TPIDRRO_EL0` |

The thread pointer registers are saved too, so that one task's thread pointer never shows up in another task. The Rust `Context` struct must match these offsets exactly. Compile-time assertions in `usermode.rs` check every field offset and the total size of 288 bytes, so a mismatch fails the build.

The handler receives a mutable reference to this context and may change it: write return values, advance the program counter, or point `__current_context` at another task. When the handler returns, execution falls through into `__user_restore`, which loads `__current_context` again and restores the user state from whatever context it now names. That is why a task switch is just a pointer change (see [scheduling.md](scheduling.md)). The very first entry to EL0 at handoff uses the same restore path. That entry, `enter_user`, masks all of DAIF before it loads `ELR_EL1` and `SPSR_EL1`. At boot the timer is already running, and an interrupt between those register writes and the `eret` would overwrite both registers with EL1 values. On the normal path through an exception entry the mask is already set.

FP/SIMD registers are not part of the context. They do not need to be, because every FP/SIMD instruction at EL0 traps (see [hardening.md](hardening.md)).

## Synchronous exceptions from EL0

`el0_sync` reads `ESR_EL1` and decides by exception class:

- **`SVC` from AArch64** goes to `k0_syscall`. The CPU has already set the return address to the instruction after `svc`.
- **A trapped `WFI` or `WFE`** is skipped. The return address moves past the instruction and the task continues.
- **Everything else** is a fault: aborts, undefined instructions, trapped system register accesses, FP/SIMD use, alignment faults, `brk` and the rest. It goes to `k0_fault` together with `FAR_EL1`.

## System calls

The system call number is in `x8`. Arguments are in `x0` to `x5`, and the result comes back in `x0`. IPC receive calls also return data in `x1` to `x5`. Errors are negative values (see `k0_abi::err`). Every slot argument is a slot number in the calling task's own CNode and means nothing in any other task's CNode (see [capabilities.md](capabilities.md)). The source of truth for numbers and arguments is `crates/k0-abi/src/lib.rs`.

| Number | Call | Description |
| --- | --- | --- |
| 0 | `DEBUG_PUTC` | write one filtered byte to the console, which needs a `Console` capability |
| 1 | `YIELD` | go to the back of the ready queue |
| 2 | `EXIT` | terminate the calling task |
| 3 | `RETYPE` | create a kernel object from untyped memory, into a destination slot the caller names |
| 4 | `MAP` | map a frame or install a page table in the address space named by an `AddrSpace` capability |
| 5 | `TCB_CONFIGURE` | set a new task's entry point, stack, address space, capability space and `x0` argument |
| 6 | `TCB_RESUME` | start a configured task |
| 7 | `SEND` | send a message |
| 8 | `RECV` | receive a message |
| 9 | `CALL` | send and wait for the reply |
| 10 | `REPLY_RECV` | reply, then receive the next message |
| 11 | `UNMAP` | remove a frame's mapping, or detach and zero a page table |
| 12 | `CAP_COPY` | copy a capability into another slot, optionally with fewer rights and a badge |
| 13 | `CAP_MOVE` | move a capability to another slot |
| 14 | `CAP_DELETE` | delete a capability and everything derived from it |
| 15 | `CAP_REVOKE` | delete everything derived from a capability and keep the capability |

An unknown number is not just an error code. K0 treats it as a sign that the task is broken or hostile, prints `k0: unknown syscall N, killing task`, and terminates the caller. The boot test requires exactly one such line.

## Fault isolation

A fault at EL0 is the faulting task's problem, not the system's. `k0_fault` prints one diagnostic line:

```
k0: task fault dabort-lower (esr 0x92000046 elr 0x400398 far 0x0), killing task
```

It then calls `kill_current`, which runs the teardown described in [scheduling.md](scheduling.md#teardown): it removes the task from whatever queue it is in, cuts the reply links in both directions (so the task waiting for this task's reply wakes with `NO_REPLY`, see [ipc.md](ipc.md)) and marks the task `Dead`. `EXIT` and an unknown system call use the same path.

### The finish path

Every system call and every fault ends in `finish`, which runs after the handler and after all capability cleanup. A handler can kill the calling task without a fault: `EXIT`, deleting its own CNode, address space or TCB, or revoking the untyped it came from. So `finish` decides what happens to the context only once everything else is done:

1. If the current task is `Dead`, no return value is written into its context. The context is zeroed, so no register of a dead task survives, and the kernel switches to the next ready task. If none is ready, it prints `k0: all tasks exited` and parks.
2. Otherwise the return value is written to `x0`. IPC calls that block write their results later, when the partner acts.

Cleanup comes before the switch because a revoke can destroy the very address space or CNode the task was using, and the next task must not run on top of half-finished cleanup.

## Timer interrupts from EL0

An IRQ (on QEMU) or FIQ (on Apple Silicon) taken from EL0 is saved the same way as a system call. The handler acknowledges the interrupt. If it was the timer, it re-arms the timer for one second, prints `k0: tick N` and calls `k0_preempt`, which rotates to the next ready task. Spurious interrupt numbers 1020 to 1023 are ignored. Any other interrupt number, an IRQ on Apple Silicon, or an FIQ on QEMU cannot happen by design, so the kernel prints `k0: UNEXPECTED ...` and parks.

## How it is tested

The root task self-tests start tasks that fault in each way that matters and check that only the faulting task dies, that the code after the faulting instruction never runs, and that the root task and the other tasks continue. The faults are a store through a null pointer, an FP/SIMD instruction, a read of an address that is mapped only in the root task, a write to a read-only page, a branch into a stack page, and a read of a kernel address. The test also kills a task with one unknown system call, and checks that thread pointer registers stay separate per task.

`tools/qemu-boot-test.sh` counts the lines exactly. It requires exactly six `k0: task fault ` lines (overridable with `K0_BOOT_FAULTS`) and exactly one `k0: unknown syscall ` line, and it requires that the fault line for the root-only address shows that address in `far`. A task that is resumed after it was destroyed, or that dies for a different reason, adds or removes a line and fails the test. It also requires the ordered markers, among them `k0: task fault dabort-lower`, `k0: task fault fp-simd`, `k0: task fault iabort-lower`, `k0: unknown syscall 65535, killing task` and `root: fault isolation tests pass`, and it fails on `k0: FATAL`, `k0: EXCEPTION`, `k0: PANIC` and `k0: UNEXPECTED`.

## Known gaps

- A faulting task is always killed. Delivering the fault to a handler task over IPC, so that a pager or supervisor can respond, is planned but not implemented.
- Any exception at EL1 other than an interrupt is fatal. The kernel does not recover from its own faults: the fatal vector path prints a diagnosis and halts the whole system. A kernel invariant violation found by the kernel's own checks stops the system the same way through `k0_arch::fatal`.
- A task that faults or makes an unknown system call kills only itself, but if it was the last runnable task the whole system parks (`k0: all tasks exited`). If all tasks are blocked, the kernel also parks (GHSA-q99x-2wrv-358p). Isolation does not cover availability.
- The fault message includes user addresses. That is useful for debugging, but it goes to a console that every task holding a `Console` capability can also write to.
- The 16 KiB (Apple) build compiles and passes host tests, but it has not been booted.
