# Exceptions, system calls and fault isolation

[한국어](exceptions_KR.md)

After the handoff, kernel code runs only in response to an exception. This note describes the vector table, how user state is saved and restored, how system calls are dispatched, and how a faulting task is contained. The code is in `crates/k0-arch/src/vectors.S`, `vectors.rs`, `usermode.rs` and `irq.rs`, and in `k0_syscall`, `k0_fault` and `k0_preempt` in `kernel/src/main.rs`.

## Mechanism and policy

`k0-arch` contains the mechanism: saving registers, decoding the exception, acknowledging interrupts. The decisions live in the kernel binary. `k0-arch` declares three external functions, `k0_syscall`, `k0_fault` and `k0_preempt`, and the kernel binary defines them. Every policy that affects a task is therefore in one file, `kernel/src/main.rs`.

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

The handler receives a mutable reference to this context and may change it: write return values, advance the program counter, or point `__current_context` at another task. When the handler returns, execution falls through into `__user_restore`, which loads `__current_context` again and restores the user state from whatever context it now names. That is why a task switch is just a pointer change (see [scheduling.md](scheduling.md)). The very first entry to EL0 at handoff uses the same restore path.

FP/SIMD registers are not part of the context. They do not need to be, because every FP/SIMD instruction at EL0 traps (see [hardening.md](hardening.md)).

## Synchronous exceptions from EL0

`el0_sync` reads `ESR_EL1` and decides by exception class:

- **`SVC` from AArch64** goes to `k0_syscall`. The CPU has already set the return address to the instruction after `svc`.
- **A trapped `WFI` or `WFE`** is skipped. The return address moves past the instruction and the task continues.
- **Everything else** is a fault: aborts, undefined instructions, trapped system register accesses, FP/SIMD use, alignment faults, `brk` and the rest. It goes to `k0_fault` together with `FAR_EL1`.

## System calls

The system call number is in `x8`. Arguments are in `x0` to `x5`, and the result comes back in `x0`. IPC receive calls also return data in `x1` to `x5`. Errors are negative values (see `k0_abi::err`).

| Number | Call | Description |
| --- | --- | --- |
| 0 | `DEBUG_PUTC` | write one filtered byte to the console |
| 1 | `YIELD` | go to the back of the ready queue |
| 2 | `EXIT` | terminate the calling task |
| 3 | `RETYPE` | create a kernel object from untyped memory |
| 4 | `MAP` | map a frame or install a page table |
| 5 | `TCB_CONFIGURE` | set a new task's entry point, stack and address space |
| 6 | `TCB_RESUME` | start a configured task |
| 7 | `SEND` | send a message |
| 8 | `RECV` | receive a message |
| 9 | `CALL` | send and wait for the reply |
| 10 | `REPLY_RECV` | reply, then receive the next message |

An unknown number is not just an error code. K0 treats it as a sign that the task is broken or hostile, prints `k0: unknown syscall N, killing task`, and terminates the caller.

## Fault isolation

A fault at EL0 is the faulting task's problem, not the system's. `k0_fault` prints one diagnostic line:

```
k0: task fault dabort-lower (esr 0x92000046 elr 0x400398 far 0x0), killing task
```

It then calls `kill_current`, which does two things in order:

1. It withdraws any reply right the task holds, so the task waiting for that reply wakes with `NO_REPLY` (see [ipc.md](ipc.md)).
2. It marks the task `Dead` and switches to the next ready task.

If the dead task was the last runnable one, the kernel prints `k0: all tasks exited` and parks.

## Timer interrupts from EL0

An IRQ (on QEMU) or FIQ (on Apple Silicon) taken from EL0 is saved the same way as a system call. The handler acknowledges the interrupt. If it was the timer, it re-arms the timer for one second, prints `k0: tick N` and calls `k0_preempt`, which rotates to the next ready task. Spurious interrupt numbers 1020 to 1023 are ignored. Any other interrupt number, an IRQ on Apple Silicon, or an FIQ on QEMU cannot happen by design, so the kernel prints `k0: UNEXPECTED ...` and parks.

## How it is tested

The root task self-tests start one task that stores through a null pointer and another that executes an FP/SIMD instruction. They check that each of these tasks is killed, that the code after the faulting instruction never runs, and that the root task and its other tasks continue. They also check that thread pointer registers stay separate per task. `tools/qemu-boot-test.sh` requires both fault lines, `k0: task fault dabort-lower` and `k0: task fault fp-simd`, followed by `root: fault isolation tests pass`.

## Known gaps

- A faulting task is always killed. Delivering the fault to a handler task over IPC, so that a pager or supervisor can respond, is planned but not implemented.
- Any exception at EL1 other than an interrupt is fatal. The kernel does not recover from its own faults.
- The fault message includes user addresses. That is useful for debugging, but it goes to a console that every task can also write to.
