# Introduction to K0

<div align="center">

English | [한국어](INTRODUCTION_KR.md)

</div>

This document explains in plain words what K0 is, what it can do today and what it cannot do yet. Each technical term appears once in parentheses, so that you can look it up in [SECURITY.md](SECURITY.md) and the [design notes](design/en/README.md).

## What K0 is

An operating system kernel is the program that controls the hardware and decides what every other program may do. Most kernels are large and do many jobs themselves. K0 is a microkernel: it does only the few jobs that must be done inside the kernel and leaves everything else to ordinary programs. The kernel side is about 7,400 lines of Rust and assembly (comments included, tests excluded), small enough to read from start to end.

The most important of those jobs is keeping programs apart. Think of a dormitory. Each room is a block of memory, each key opens one thing, and the kernel is the caretaker. A resident can open only the doors they hold a key for, cannot turn a copy into a stronger key, and when a key is taken back, every copy made from it stops working too. In K0 these keys are called capabilities. The idea comes from the seL4 kernel.

## Words used here

| Word | Meaning |
| --- | --- |
| Task | One running program |
| Root task | The first program. It starts with every key the kernel hands out at boot |
| Capability | A key to one resource. A program names a key only by its slot number, and the key itself stays in memory that only the kernel can touch, so it cannot be forged |
| CNode | A key ring: a table of slots that holds capabilities |
| Address space | The memory map of one program. Memory that is not on the map does not exist for that program |
| Untyped memory | Free memory with no purpose yet. A program turns it into kernel objects (retype) |
| Endpoint | A mailbox through which tasks exchange messages (IPC) |
| System call | A request that a program makes to the kernel |
| Interrupt | A signal from the hardware, such as a timer, that makes the processor stop and run the kernel |

## Goals

K0 is meant to become the isolation core of high-security systems: cross-domain guards and data diodes (a separation kernel), key vaults and signing oracles (something like a software HSM), measured-boot components on top of a hardware root of trust, and a base for formal verification and certification research. None of these works yet. Today K0 is a research platform: it boots and checks its own security properties, but it does no useful work of its own yet.

## What works today

- **Starting up**: one generic image boots wherever the following holds. The processor is ARMv8-A or ARMv9-A and little-endian. The boot loader enters the kernel with the memory map switched off (MMU off), at EL1 or at the hypervisor level (EL2), following the Linux arm64 Image protocol, with the address of the device tree in register x0. The interrupt controller is a GICv2 or GICv3, the processor has the ARM generic timer, RAM lies below 2^47 bytes (128 TiB), and the boot loader starts exactly one processor core. K0 uses that one core. The image can be loaded at any physical address that fits the page grid, and it always runs at the virtual address `0xFFFF_8000_0000_0000`. If it is started at EL2, the kernel sets up the EL2 state itself, steps down to EL1 once and never goes back. If it is started in any state it does not support, it stops instead of guessing.
- **Starting from UEFI firmware**: firmware that hands over a device tree can start K0 through the loader in `loader/` (see [design/en/uefi.md](design/en/uefi.md)). The loader is a UEFI application with the kernel image embedded in it. It plans which memory is RAM from the UEFI memory map, rewrites the firmware's device tree to say so, checks the result with the kernel's own device tree code, exits the firmware services, turns the MMU off and enters the kernel with the same contract as above. Only free memory and the loader's own memory count as RAM. Memory that firmware still uses never does, so about 23 to 32 MiB of boot services memory stays unused on EDK2. The kernel does not change, and it also does not know it was started this way. Only this loader may start the kernel under UEFI: a generic UEFI boot loader that passes the firmware device tree through would hand firmware memory to user space. Firmware that provides no device tree (ACPI only) is refused. The loader checks the embedded kernel with a SHA-256 hash, which catches corruption but is not a signature. Under Secure Boot the firmware verifies the whole loader file, kernel included.
- **Finding devices**: from the device tree the kernel takes only three things: the console (the node named by `stdout-path` in `/chosen`; PL011, SBSA UART or 16550 family), the interrupt controller (the interrupt parent of the timer node; GICv2 or GICv3) and the interrupt number of the EL1 virtual timer. They go into one platform descriptor that never changes, printed as a `k0: platform ...` line, and it is checked against the memory map before any device page is mapped. The hardware must agree with the device tree (the identification registers of the GIC and the PL011), otherwise the kernel refuses to boot. With any other UART, or without `stdout-path`, there is no console.
- **Boot log**: every line the kernel prints also goes into a 16 KiB buffer in kernel memory. Lines printed before the console is attached are replayed once it is. With no console, this buffer is the only record. `tools/boot-log.py` reads it from the memory of a QEMU guest. Output of user tasks is recorded only up to a quota.
- **Timer**: only the EL1 virtual timer is used, so it works inside hypervisor guests. The image never touches the physical timer, and the boot matrix checks that.
- **Granule**: the page size is 4 KiB by default. A 16 KiB build is a build option (see [USAGE.md](USAGE.md)). It is not chosen at run time, and on a processor without 16 KiB support the 16 KiB image refuses to boot.
- **Pinned build**: an image built with `K0_PIN` set to the platform line of a boot log refuses to boot when the discovered platform, the entry level or the RAM range differs. This catches a wrong board or a swapped device tree. It is not a defence against a hostile boot chain, because the comparison uses values that come from the device tree.
- **Protecting its own memory**: no page of kernel memory is both writable and executable (W^X). An empty guard page sits below the kernel's boot stack, so running off the end of the stack stops the kernel instead of damaging other memory. At boot, the kernel asks the processor to look up its own memory map (address translation instructions) to check that these settings really took effect.
- **Distrusting boot input**: the hardware description handed over at boot (the device tree, DTB) is read as if an attacker wrote it. If it claims memory that overlaps hardware registers (MMIO) or lies beyond what the processor can address, the boot stops.
- **Processor security features**: when the processor has them, K0 loads the keys that let the processor detect tampered pointers (pointer authentication, PAC) and turns on the guard that keeps the kernel from accidentally using program memory (PAN). Programs cannot read the hardware counters, debug registers or performance counters. The compiler does not produce pointer signing instructions yet, so the places where the kernel's functions return to are not protected yet.
- **Keys**: free memory can be turned into blocks of memory (frames), the tables that make up memory maps (page tables), tasks (TCB), mailboxes (Endpoint), key rings (CNode) and address spaces (AddrSpace). Memory is placed on an address space's map only by showing that address space's key (MAP, UNMAP). Keys carry rights (read, write, execute). A mailbox key can carry a tag that tells the receiver who sent a message (badge). Keys can be copied with the same or fewer rights, or moved between slots (CAP_COPY, CAP_MOVE).
- **Running tasks**: a task is tied to one key ring and one address space when it is set up (TCB_CONFIGURE) and then started (TCB_RESUME). Tasks take turns, and a timer interrupts a task that runs too long (round-robin scheduling with timer preemption). A task can give up its turn (YIELD) or finish (EXIT).
- **Messages**: tasks exchange short messages that travel in processor registers. The sender waits until the receiver takes the message (synchronous IPC: SEND, RECV, CALL, REPLY_RECV). Sending needs the write right on the mailbox key and receiving needs the read right. The receiver learns the tag of the sender's key, and the right to reply can be used only once.
- **Crash containment**: when a task crashes, makes a system call the kernel does not know, or exits, only that task is stopped (fault isolation).
- **Task isolation and taking keys back**: tasks tied to different key rings and address spaces cannot reach each other's memory or keys. Every key remembers which key it was copied from (derivation list). Taking a key back (CAP_REVOKE) or deleting it (CAP_DELETE) also removes every key copied from it, takes memory off the maps, shuts down objects whose original key is gone, stops tasks that lose their key ring or address space, and fills freed memory with zeros before it can be used again. No memory is ever writable in one place and executable in another.
- **Checking the first program**: the root task is built into the kernel image, and at every boot its fingerprint (a SHA-256 hash) is checked again. This catches corruption, but it does not prove who made the image the way a signature would. [SECURITY.md](SECURITY.md) explains why under "Scope".

The root task tests every item above with self-tests on every boot.

## Where it has run

Only the first two tiers may be called supported.

- **Tier 1, checked by `tools/boot-matrix.sh` (CI runs it on QEMU 7.2)**: QEMU virt with GICv3 and with GICv2, each entered at EL1 and at EL2; the CPU models cortex-a72, cortex-a53 and max; the security extensions switched on (`secure=on`) for both GIC versions; 4 processor cores started by the machine (K0 still uses one); the 16 KiB image on max and its refusal on cortex-a72; the same image loaded at three other physical addresses (the header `text_offset` patched) and a refusal for an address off the 16 KiB grid; `xlnx-versal-virt` (RAM at address 0, GICv3 and PL011 elsewhere, entered at EL2); a pinned image that boots on its own platform and refuses on another; and a static check that the image does not touch the physical timer. The rows also ran locally on QEMU 11.1.3. The UEFI rows boot the same kernel images through the loader on EDK2 firmware (Debian `qemu-efi-aarch64` 2022.11 in CI, QEMU `virt` with `acpi=off`): GICv3 and GICv2, each at EL1 and at EL2; the max CPU (pointer authentication, BTI, PAN); 4 cores; the 16 KiB image; a firmware random number generator (the entropy reaches the kernel); starting from a FAT drive instead of `-kernel`; and a static check of the loader's PE file form and of the order of its cache and MMU instructions (`tools/pe-check.py`). Three refusals are read from the serial log: firmware with no device tree (QEMU with ACPI on), the 16 KiB image on a CPU without 16 KiB pages, and a loader whose embedded kernel has one flipped byte.
- **Tier 2, run by hand**: on 2026-11-11, Apple M5 Pro, QEMU 11.1.3 with HVF, which runs real Apple cores as a hypervisor guest: the 4 KiB image at EL1, entry at guest EL2, the 16 KiB image and 4 cores (`K0_MATRIX_HVF=1 tools/boot-matrix.sh`). On 2026-11-12, the same host and QEMU, the UEFI loader through HVF at EL1, at EL2 and with the 16 KiB image. Also on 2026-11-12, the real loader under UEFI Secure Boot: Debian EDK2 2022.11 in QEMU 7.2 (TCG), with test keys enrolled in a throwaway container. A signed loader booted, both from the FAT drive and with `-kernel` (the firmware checks that path too). An unsigned loader, a signed loader with one byte of the embedded kernel flipped, and a loader signed with a key that was not enrolled were all refused by the firmware before any loader code ran. This used one firmware, and each boot was stopped after about 60 seconds.
- **Tier 3, expected by the contract but never booted**: any real board, KVM, Firecracker, Cloud Hypervisor, crosvm, the 16550 console driver (no machine in the matrix has one), GICs with real security extensions on hardware, and processors whose `HCR_EL2.E2H` is fixed to 1. The EL2 path for such processors follows Linux but has not been run anywhere. As with Linux, an EL3 firmware must enable the fine-grained-trap and HCRX controls when those features exist. Under UEFI also: any real board, U-Boot's EFI implementation, Raspberry Pi UEFI, any firmware other than EDK2 on QEMU `virt`, KVM, a pinned build (`K0_PIN`), and the cache flush before the MMU is turned off on real hardware.

## How far the isolation goes

What it stops:

- A task cannot use a resource it holds no key for, and it cannot see memory that is not on its own map.

What it does not stop:

- **Availability**: a task can still bring everything to a halt. If every task ends up waiting for a message, the kernel stops the processor core, and a confined task can cause this by being the last one to wait. A task given a lot of free memory can also use up the kernel's fixed tables (32 page tables for the kernel's own use, 256 address space entries), and then nobody can create new objects.
- Tasks that share one key ring and one address space are not isolated from each other. They behave like threads, parts of one program that share everything.
- Whoever holds the key to a task, to its key ring or to its address space controls that task.

## Not there yet

Missing features:

- Handing a key to another task inside a message (capability transfer over IPC)
- Telling a program about hardware interrupts (Notification objects)
- Reporting a crashed task to a handler task instead of only stopping it
- Timeouts, and more than one processor core
- Giving programs access to hardware devices (device memory and interrupts). Until this exists, K0 cannot exchange data with the outside world except through the text console that prints these messages
- Loading new executable code after boot. This is the flip side of the write-or-run rule: executable memory exists only for the root task image, so a confined task runs code from that image
- Multi-level key lookup, and hardware address space tags (ASIDs). Today every switch to another address space empties the processor's cache of memory map lookups (the TLB)
- Checking a digital signature (Ed25519) once the root task ships separately from the kernel
- Booting on real boards, and on KVM, Firecracker, Cloud Hypervisor or crosvm (tier 3 above)
- Entry from EL3, and ACPI. UEFI with a firmware device tree exists (see above). Firmware that describes the machine only with ACPI tables is refused. Reading the static ACPI tables to build a device tree is planned as the next scope, in the loader and not in the kernel. Firmware other than EDK2 and real boards with UEFI are untested
- Interrupt controllers other than GICv2 and GICv3, confidential VMs, a 64 KiB granule, big-endian, boards whose device tree declares a timer erratum, and RAM above 2^47 bytes. These are excluded by decision
- Apple Silicon bare metal. There is no plan for it
- AMD64, the processors in most PCs, and other architectures

Missing protection and assurance:

- Availability guarantees (see the previous section)
- DMA isolation: some devices read and write memory directly, without the processor (DMA). Once devices are handed to programs, a memory map for devices (an IOMMU, such as the SMMU on ARM) has to stop such a device from reading all memory. Nothing for this exists yet
- Side channels, where secrets leak through timing or through caches the processor shares, rather than by direct access: a few such leaks are closed (programs cannot read the hardware counters, and the TLB is emptied whenever the address space changes), but there is no general defence against cache or timing side channels, and they have not been analysed
- Taking a key back does not cancel a sender already waiting at the mailbox. Its message is still delivered once
- Assurance comes from testing only: unit tests and fuzzing (feeding random, broken input to find crashes) on the build machine, and self-tests at boot in QEMU and in a hypervisor guest on real Apple cores. There is no formal proof, a mathematical proof that the code does what it should, and nothing has run on a real board

## Where the code lives

| Path | Contents |
| --- | --- |
| `kernel/` | The kernel program: startup assembly, the order of initialization, system call handling (`syscall.rs`), cleanup when a key is removed (`object.rs`), files that describe the memory layout of the kernel image (linker scripts) |
| `crates/k0-abi` | System call numbers, error codes and the boot information layout that programs share with the kernel |
| `crates/k0-arch` | Handling of errors, system calls and interrupts (exceptions), entering and leaving user mode, interrupt controller and timer, PAC and PAN, early console |
| `crates/k0-boot` | Device tree parser, the built-in root task image and its integrity check, PAC key derivation, the search for the boot memory window (shared with the loader) |
| `crates/k0-efi` | The loader's logic without `unsafe` code: planning RAM from the UEFI memory map, rewriting the firmware device tree, an independent re-check of the result, and the check of the kernel image header. It runs the kernel's own device tree code on the result before the jump |
| `crates/k0-cap` | Keys and their rights, key rings, the derivation list, untyped memory, retype, revoke and delete |
| `crates/k0-mm` | Kernel page tables, turning memory maps on (MMU), program address spaces and their registry |
| `crates/k0-task` | Tasks with their key ring and address space, mailboxes, loading the root task |
| `crates/k0-sched` | Round-robin scheduler |
| `crates/k0-ipc` | Synchronous messages |
| `loader/` | The UEFI application (`k0-loader`, target `aarch64-unknown-uefi`) that embeds a built kernel image and starts it: UEFI calls, the flow, and the final cache flush, MMU off and jump |
| `userspace/root-task` | The first program and its self-tests |
| `tools/` | QEMU runner (also for `.efi` files), boot test, boot matrix, boot log reader, host test, loader build (`build-loader.sh`), PE and instruction order check (`pe-check.py`) |
| `ci/` | The CI build and test image (`Dockerfile`) and the threat model (`threat_model.md`) |
| `vendor/` | Third-party crates kept in the repository (`sha2` and its dependencies) |
