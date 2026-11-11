# Boot and the jump to the higher half

[한국어](boot_KR.md)

This note follows K0 from the first instruction to the moment the root task runs at EL0. It covers `kernel/src/arch/aarch64/boot.S`, `kernel_init` and `kernel_main` in `kernel/src/main.rs`, and the linker scripts `kernel/virt.ld` and `kernel/apple.ld`.

## The entry contract

K0 is loaded as a Linux arm64 Image. The first 64 bytes of the image are the Image header: a branch to the real entry point, the load offset, the image size, flags and the `ARM\x64` magic. The linker script fills in the offset, size and flags. QEMU and m1n1 both recognize this header and enter the kernel under the Linux boot protocol:

- `x0` holds the physical address of the device tree blob (DTB).
- The MMU and the data cache are off.
- The CPU is at EL2 or EL1.

The kernel is linked at a higher-half virtual address (`0xFFFF_0000_0000_0000` plus the physical load address) but starts running at its physical load address. Everything in this note follows from that gap.

## Phase 0: `boot.S`

The assembly entry does the minimum needed to reach Rust safely, and parks the core in a `wfe` loop whenever something is unexpected.

1. **Mask all exceptions.** No vector table is installed yet, so any exception would be undefined behavior.
2. **Pick the boot core.** It compares the whole affinity value of `MPIDR_EL1` (Aff3, Aff2, Aff1 and Aff0) with zero. Apple Silicon puts the cluster number in Aff1, so checking only Aff0 would let core 0 of every cluster through. Every other core parks. K0 is single-core today.
3. **Check the exception level.** EL1 continues. EL2 drops to EL1. EL0 and EL3 park.
4. **Drop from EL2 to EL1.** `HCR_EL2.RW` makes EL1 AArch64. `HCR_EL2.HCD` turns `hvc` into an undefined instruction at EL1, because EL2 has no vector table and a trap to EL2 would hang without a diagnosis. If the CPU has pointer authentication, `HCR_EL2.APK` and `HCR_EL2.API` stop EL1 key and PAC instruction use from trapping to EL2. The code also gives EL1 the physical timer and counter, zeroes the virtual counter offset, and clears the EL2 traps for FP/SIMD, system registers, debug and PMU. Then it returns to EL1h with all exceptions masked.
5. **Normalize EL1.** Whatever the bootloader left behind, `SCTLR_EL1` is set to a known value: MMU and caches off, little-endian, stack alignment checks at EL0 and EL1, `WFI` and `WFE` trapped at EL0, and EL0 access to the interrupt mask bits denied. `CPACR_EL1` is set to zero, so FP/SIMD, SVE and SME instructions trap at both EL0 and EL1. The kernel is built for a soft-float target and keeps no vector register state for tasks.
6. **Set the boot stack** to `__boot_stack_top`, **zero `.bss`**, and call `kernel_init(x0)`. `x0` is never touched on the way, so the DTB address arrives as the first argument.

## `kernel_init`: before the jump

`kernel_init` runs at the physical load address with the MMU off. Code that forms addresses relative to the program counter (`adr`, `adrp`) gets correct physical addresses. Data that contains absolute pointers was relocated by the linker to higher-half addresses, though. That covers string slices stored in statics, vtables and `core::fmt` machinery. Dereferencing such a pointer now faults. So this stage prints only literal strings and hexadecimal integers, and it does nothing that needs `writeln!`.

It does four things:

1. **Installs the exception vectors** with their physical address, so that any fault from here on prints a diagnosis instead of hanging.
2. **Reads only the DTB header** with `k0_boot::dtb_span`: the magic and the total size, at an 8-byte aligned, non-null address, with a size between 40 bytes and 2 MiB. The full parse needs relocated data, so it waits until after the jump. The size is needed now, because the DTB has to be mapped before the jump.
3. **Turns on the MMU** with `k0_mm::enable_paging`. The layout comes from linker symbols: text, read-only data, data and `.bss`, the boot stack, the DTB, and the device windows for the UART and, on QEMU, the GICv3 distributor and redistributor. Everything is mapped twice, once at its physical address (identity, through `TTBR0`) and once in the higher-half alias (through `TTBR1`). [memory.md](memory.md) describes the tables and permissions.
4. **Jumps.** Inline assembly adds the alias offset to the stack pointer, builds the absolute address of `kernel_main` with `movz` and `movk`, and branches to it with `x0` still holding the DTB address.

## `kernel_main`: after the jump

The program counter and stack pointer now sit in the higher half, relocated data is valid, and normal Rust works. The order of the steps below is the security argument, so `kernel_main` is one straight sequence. Before the jump, typestate tokens such as `Mmu` and `Vectors` prove that one step happened before another. Those tokens cannot cross the jump, so after it the order of the code is the enforcement. Every failure prints a reason and parks the core.

| Log line | Step |
| --- | --- |
| `k0: higher half (pc = ...)` | Vectors reinstalled at their virtual address. The early console switches to the alias, so it keeps working after the identity map is gone. |
| `k0: w^x checks pass (granule 4K, wxn)` | Address translation instructions confirm that text is readable and not writable, read-only data is not writable, the stack guard page is unmapped, and the stack is writable. |
| `k0: memory ...` | The DTB is parsed through the alias. [dtb.md](dtb.md) covers the parser. The memory map must not overlap any kernel MMIO window, and no region may reach beyond the CPU's physical address width (`k0_mm::pa_bits`). |
| `k0: root task integrity ok (...)` | The embedded root task image is hashed again and compared with the hash fixed at build time. |
| `k0: entropy ...`, `k0: pac=... bti=... pan=...` | PAC keys are derived and loaded, and the key words are wiped from the boot stack. PAN is enabled and EL0 access to counters, debug and PMU is turned off. See [hardening.md](hardening.md). |
| `k0: irq on (timer 1s)` | The GIC (on QEMU) and the timer are set up, and IRQ and SError are unmasked. |
| `k0: kernel table pool N/32` | A 2 MiB boot frame window is chosen and mapped, then the number of kernel page tables in use is printed (`18/32` on QEMU virt). |
| `k0: root task loaded (entry ...)` | The root task is copied into frames from the window, its root TCB is made in the window, and its address space is built. The kernel does not print the physical address of the user root table. The address space is then entered in the address space registry. See [user-address-space.md](user-address-space.md). |
| `k0: untyped ...`, `k0: root cnode ready (N caps, N untyped, N image frames, N slots)` | The root CNode (an object in the window with `ROOT_CNODE_SLOTS` = 512 slots, as the log shows) is filled: slot 1 the root TCB, slot 2 the root address space, slot 3 the console, slot 4 the root CNode itself, then every free physical range as an untyped capability, then one Frame capability per mapped granule of the root task image (text with R and X, read-only data with R, data with R and W). Finally the root task is bound to its CNode and address space, exactly as any other task is: copies of those two capabilities go into its TCB. See [capabilities.md](capabilities.md). |
| `k0: bootinfo page ready` | The capability list is written into the read-only bootinfo page: a header and one descriptor per slot, with the VA, size and rights of every image Frame. |
| `k0: user w^x checks pass` | `TTBR0` now points at the user tables, the identity map is gone, and the user mappings are checked from both EL0 and EL1. |
| `k0: entering root task (el0)` | `k0_sched::handoff` enters EL0. |

### Choosing the boot frame window

`pick_window` looks for 2 MiB of RAM right after the kernel image. If the candidate overlaps the DTB or a firmware reserved region, it moves past the obstacle and tries again, for at most 64 rounds. It returns a range only if the whole window fits inside one memory region from the DTB. The window holds the root task image, its stack, its page tables, the bootinfo page, the root TCB and the root CNode. It is excluded from the untyped capabilities, so the root task can never retype memory the kernel is still using.

### After the handoff

`handoff` records the root task as the running task and points the vector code at its saved context. `enter_user` then masks all exceptions (`DAIFSet`), rewinds the kernel stack to the top, and returns to EL0 through the same path every later return uses. The mask comes first because that path writes `ELR_EL1` and `SPSR_EL1` before `eret`. The boot path has already unmasked the timer, and an interrupt in between would overwrite both registers with EL1 values. From then on, kernel code runs only in response to an exception: a system call, a fault, or the timer interrupt. [exceptions.md](exceptions.md) describes that path.

## How it is tested

`tools/qemu-boot-test.sh` boots the debug kernel under QEMU and checks the log lines above in order, then the root task self-tests. A failure line from any step fails the test. The self-checks inside `kernel_main` are themselves tests: if a mapping is wrong, the boot stops with the name of the failing check.

## Known gaps

- Only one core runs. The secondary cores park in `boot.S` forever.
- The Apple Silicon build is compiled in CI but has not yet been booted on hardware.
- The kernel always sits at the same virtual address. There is no address randomization.
- A release build prints nothing on panic. That is intentional, but it also means a release build gives no diagnosis on panic.
