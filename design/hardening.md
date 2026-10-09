# Boot hardening: PAC, PAN, EL0 exposure and root task integrity

[한국어](hardening_KR.md)

This note covers the CPU security features K0 turns on at boot, what each one protects today and what it does not yet protect, and how the embedded root task image is checked. The code is in `crates/k0-arch/src/hardening.rs`, `crates/k0-boot/src/entropy.rs`, `crates/k0-boot/src/roottask.rs`, `crates/k0-boot/build.rs` and the boot path in `kernel/src/main.rs`.

Some of these features are groundwork, not working protection yet. The sections below say which is which.

## Boot entropy and PAC key derivation

`derive_pac_keys` produces the ten 64-bit words that make up the PAC keys: an instruction key pair A and B, a data key pair A and B, and the generic key, two words each. It mixes three inputs:

- the bytes of `rng-seed` and `kaslr-seed` from the DTB's `/chosen` node (up to 72 bytes, possibly none),
- the system counter `CNTPCT_EL0`, and
- one value from the `RNDR` random number instruction, if the CPU implements FEAT_RNG and the instruction succeeds.

The derivation is SHA-256 in counter mode. Each 32-byte block hashes a fixed domain string (`k0-pac-boot-key-v1`), a one-byte block counter, the length of the DTB entropy as a little-endian 64-bit value, the DTB entropy itself, and the other inputs as little-endian words. The length prefix keeps DTB bytes from being confused with the words that follow them. Three blocks give the ten words.

Because the inputs are hashed together, a hostile or predictable input cannot cancel out the entropy of another input. The result is still no better than the best input. If the DTB provides no seed and the CPU has no `RNDR`, the only input is the counter, and the boot log says `k0: WARNING pac key entropy low (cntpct only)`.

## Pointer authentication (PAC)

If the CPU implements address authentication, `hardening::enable` writes the four address keys and, if generic authentication exists, the generic key. It then sets `EnIA`, `EnIB`, `EnDA` and `EnDB` in `SCTLR_EL1`. When the kernel entered at EL2, `boot.S` already set `HCR_EL2.APK` and `HCR_EL2.API`, so key access and PAC instructions at EL1 do not trap to EL2.

**What this protects today: nothing yet.** PAC only helps when the code signs and checks pointers. The `-Z branch-protection` option that makes the Rust compiler sign return addresses is unstable, and K0 builds with the stable 1.89 toolchain. No instruction in the kernel or the root task uses the keys. The hardware is ready for the day the compiler can emit those instructions.

## Branch target identification (BTI)

BTI is detected and reported in the boot log, but it is not enabled. Turning on the guarded page bit would make every indirect branch fault unless it lands on a `BTI` instruction, and the stable compiler does not emit those landing pads. Enabling BTI now would break the kernel.

## Privileged access never (PAN)

If the CPU implements FEAT_PAN, the kernel clears `SCTLR_EL1.SPAN`, so that the CPU sets `PSTATE.PAN` on every exception entry to EL1, and sets `PSTATE.PAN` immediately with a raw instruction encoding. With PAN set, any EL1 load or store through a mapping that EL0 can access faults.

This is real protection today. It turns a whole class of kernel bugs, dereferencing a pointer supplied by user space, into a fault instead of a silent read or write. K0 never needs such access: it reaches user frames only through its own EL1-only alias (see [user-address-space.md](user-address-space.md)). With FEAT_PAN2, the boot self-check uses `AT S1E1RP` to confirm that PAN really blocks an EL1 read of user text.

## Closing what EL0 can observe

The bootloader may leave registers in states that let user space observe or control more than it should. K0 resets them to known values instead of trusting them.

| Setting | Effect for EL0 |
| --- | --- |
| `CNTKCTL_EL1 = 0` | reading the counter or the timer registers traps |
| `MDSCR_EL1 = 0` | no breakpoint, watchpoint or single-step events are armed |
| `PMUSERENR_EL0 = 0`, only when a standard PMUv3 is present | reading or programming the performance counters traps |
| `SCTLR_EL1` from `boot.S` | `WFI` and `WFE` trap, interrupt mask bits are inaccessible, stack alignment is checked |
| `CPACR_EL1 = 0` from `boot.S` | every FP/SIMD, SVE and SME instruction traps |

A trap at EL0 is handled as a fault, and the task is terminated (see [exceptions.md](exceptions.md)). The only exception is `WFI` and `WFE`, which the kernel skips. Trapping FP/SIMD means the kernel never has to save vector registers, so a task can never read vector state left behind by another task.

The EL0 thread pointer registers `TPIDR_EL0` and `TPIDRRO_EL0` are part of each task's saved context. `TCB_CONFIGURE` zeroes them, so a new task starts without the creator's values.

`DEBUG_PUTC` requires the `Console` capability and replaces every byte that is neither printable ASCII nor a newline with `?`. A task cannot send terminal escape sequences to whoever reads the console.

## Root task integrity

The kernel contains no ELF loader. `crates/k0-boot/build.rs` builds the root task on the build host, parses its ELF file there, and converts it into a flat image plus a short list of segments. During the conversion it enforces:

- a little-endian AArch64 ELF64 executable with in-bounds headers and segments,
- no segment that is both writable and executable,
- 16 KiB alignment for every segment, which fits both granules, with segments sorted and not overlapping,
- an entry point inside an executable segment,
- at most four segments and at most 1 MiB of flat image.

Any violation fails the build. Before it hashes anything, the build script also checks the vendored `sha2` crate against two FIPS 180-4 test vectors. It then bakes the image, the segment list and the image's SHA-256 hash into the kernel's read-only data.

At every boot, `verify_root_task` hashes the embedded image again and compares the result with the baked-in hash. On a mismatch, the boot stops. The first bytes of the hash appear in the boot log for comparison with the build output.

**This is a corruption check, not an authenticity check.** The image, the reference hash and the code that compares them all live in the same kernel image. Anyone who can modify the kernel image can change all three. Signing the root task with an asymmetric key would not change that while the public key also lives in the kernel image. Protection against a modified kernel image belongs to the boot chain, which has to verify the whole image. The check does catch build pipeline mistakes, damage on the loading path, and a tampered image blob when the rest of the kernel is intact. The plan is to add Ed25519 verification when the root task is shipped separately from the kernel, so that only the public key stays in the kernel.

## Panics

The panic handler masks all interrupts and parks the core. Debug builds print the panic message first. Release builds print nothing, so that a panic in the field does not leak internal state.

## How it is tested

- `crates/k0-boot/src/entropy/tests.rs` pins the key derivation with known answers computed independently with Python's `hashlib`, and checks that flipping any input bit changes the keys and that the length prefix separates the inputs.
- `crates/k0-boot/src/roottask/tests.rs` checks the embedded image against its own hash and the layout rules from the consumer side.
- The default QEMU CPU (`cortex-a72`) has none of PAC, PAN or RNDR. CI therefore boots a second time on QEMU's `max` CPU and requires `rndr=on` and `k0: pac=on bti=present pan=on` in the log, along with every other boot check and self-test, including the PAN-aware user check.
- The root task self-tests confirm that a new task starts with zeroed thread pointer registers, that the root task's values survive the other task's changes, and that an FP/SIMD instruction terminates only the task that issued it.

## Known gaps

- PAC keys are loaded, but no code uses them yet, so return addresses are not protected.
- The same PAC keys serve EL0 and EL1 and every task. Once code starts signing pointers, a task could forge pointers the kernel accepts. Per-task or per-level keys are not implemented.
- BTI is not enabled.
- The root task check proves integrity against accidents and limited tampering, not authenticity.
- None of this has run on real hardware yet. CI uses QEMU only.
