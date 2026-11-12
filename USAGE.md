# Using K0

<div align="center">

English | [한국어](USAGE_KR.md)

</div>

This document shows how to build K0, run it, and check that it works. You do not need any ARM hardware. Everything runs in QEMU, a program that behaves like an ARM computer.

## What you need

- `rustup`, the Rust installer. On the first build it downloads the exact compiler and tools K0 uses (the toolchain: Rust 1.89.0 and the parts listed in `rust-toolchain.toml`) by itself.
- `qemu-system-aarch64` with its firmware files. On macOS, run `brew install qemu`. On Debian or Ubuntu, install the `qemu-system-arm` and `ipxe-qemu` packages.
- A C compiler for the build scripts: the Xcode command line tools on macOS, or `gcc`.
- Only to boot through UEFI (see below): UEFI firmware for QEMU. Homebrew's `qemu` already contains it. On Debian or Ubuntu, install the `qemu-efi-aarch64` package.

Once the toolchain is installed, building needs no internet connection, because every outside library is already in the `vendor/` folder.

## Running K0

```sh
cargo virt
```

This builds the kernel and starts it in QEMU, and the boot messages appear in your terminal. When the self-tests are done, the kernel keeps running and prints `k0: tick N` once a second. It never exits on its own. To quit QEMU, press `Ctrl-A`, then `X`.

### Reading the output

Lines that start with `k0:` come from the kernel. Lines that start with `root:` come from the root task, the first program.

| Line | Meaning |
| --- | --- |
| `root: hello from EL0` | The first program is running in user mode |
| `root: ... pass` | One group of self-tests passed |
| `root: secret at 0x...`, then `k0: task fault ... far 0x...` with the same address | A confined task tried to read memory that only the root task may see, and it was stopped at exactly that address |
| `k0: task fault ...` and `k0: unknown syscall 65535, killing task` | Rule violations that the tests cause on purpose. Each one stops only the task that broke the rule. A normal run has exactly 6 `k0: task fault` lines and 1 `k0: unknown syscall` line |
| `root: sched tests pass` | The last group of self-tests passed |
| `k0: tick N` | The kernel is still running |

Lines such as `root: FAIL`, `k0: FATAL`, `k0: PANIC` or `k0: EXCEPTION` mean that something went wrong. The automatic boot test below checks the whole list for you.

### Other ways to run

```sh
cargo virt --release                                                     # optimized build
K0_QEMU_CPU=max cargo virt                                               # newer CPU model with more security features (PAC, PAN, RNDR)
K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on cargo virt          # ask QEMU to start the kernel at the hypervisor level (EL2)
K0_QEMU_MACHINE=virt,gic-version=2 cargo virt                            # a GICv2 interrupt controller
K0_QEMU_MEM=4G cargo virt                                                # more memory
```

The runner script `tools/qemu-virt-runner.sh` builds the QEMU command line from these variables. Setting a variable to an empty value leaves that QEMU option out.

| Variable | Default |
| --- | --- |
| `K0_QEMU_MACHINE` | `virt,gic-version=3` |
| `K0_QEMU_CPU` | `cortex-a72` |
| `K0_QEMU_SMP` | `1` |
| `K0_QEMU_MEM` | `512M` |
| `K0_QEMU_ACCEL` | none (QEMU's software emulation, TCG) |
| `K0_IMAGE_TEXT_OFFSET` | none. Boots a copy of the image whose header `text_offset` is changed, so the same image is loaded at another physical address |
| `K0_QEMU_MONITOR` | none. Path of a Unix socket for the QEMU monitor, used to read the boot log from memory |

Everything after `--` is still passed to QEMU.

### 16 KiB build

The kernel uses 4 KiB memory pages by default. The 16 KiB version is a build option and cannot be chosen at run time:

```sh
cargo build -p k0-kernel --offline --no-default-features --features granule-16k --target-dir target/g16
K0_QEMU_CPU=max tools/qemu-boot-test.sh target/g16/aarch64-unknown-none-softfloat/debug/k0-kernel
```

`--no-default-features` is needed because the 4 KiB feature is on by default, and the build stops if both are on. `--target-dir` keeps the 16 KiB image apart from the normal one. The 16 KiB image refuses to boot on a CPU without 16 KiB page support, for example `cortex-a72`.

### Pinned build

To tie an image to one platform, boot the target once, copy the text after `k0: platform ` from its boot log, and build with it in `K0_PIN`:

```sh
K0_PIN='<text after "k0: platform ">' cargo build -p k0-kernel --offline --target-dir target/pin
```

The pinned image refuses to boot when the discovered platform, the entry level or the RAM range differs. This catches a wrong board or a swapped device tree. It is not a defence against a hostile boot chain, because the comparison uses values taken from the device tree.

### Reading the boot log from memory

Every line the kernel prints is also kept in a 16 KiB buffer in memory. If the kernel refuses to boot, or there is no supported console, this buffer is the only record. With QEMU started with `K0_QEMU_MONITOR=<socket path>`, read it with:

```sh
tools/boot-log.py <kernel ELF> <load address> <QEMU monitor socket>
```

The load address is the physical address where the image was loaded. On QEMU `virt` with an unchanged image it is `0x40080000`.

## Booting through UEFI

UEFI is the standard way for firmware to start a program on many computers. The firmware loads a program file with the ending `.efi` from a disk, runs it, and describes the machine to it. A loader is a small program that does this one job: it prepares the machine and then starts another program. K0 has its own UEFI loader, `crates/k0-efi` and `loader/`. It carries the kernel inside itself, so the one `.efi` file is all the firmware needs. The kernel does not change and boots the same way as without UEFI.

```sh
cargo build -p k0-kernel --offline
tools/build-loader.sh
tools/qemu-virt-runner.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel.efi
```

QEMU's default machine has ACPI on and gives no device tree. The loader reads the ACPI static tables and builds a minimal device tree from them, so that machine boots too. To try it:

```sh
K0_QEMU_MACHINE=virt,gic-version=3 tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel.efi
```

The loader then prints `k0-loader: device tree built from ACPI tables (MADT, GTDT and SPCR)`. The runner still uses `acpi=off` for a `.efi` file unless you set `K0_QEMU_MACHINE`.

`tools/build-loader.sh` takes an already built kernel and puts it into the loader. Without an argument it uses the debug 4 KiB kernel above. It prints the path of the result, which is the kernel file with `.efi` added. For another kernel, pass its path, for example the 16 KiB build from "16 KiB build". The runner starts QEMU with the EDK2 firmware (a UEFI firmware that QEMU can load) and the machine `virt,gic-version=3,acpi=off`. It looks for the firmware in Homebrew's QEMU and then in `/usr/share/AAVMF/AAVMF_CODE.fd`. To quit QEMU, press `Ctrl-A`, then `X`.

To test it, give the `.efi` file to the boot test:

```sh
tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel.efi
```

The boot test starts judging at the loader's first line, because the firmware prints its own messages before it, and some of those contain words like `failed`. Set `K0_BOOT_REUSE=skip` if you change the machine to a memory layout without an untyped region of the right size, as described under "Testing automatically".

These variables belong to the UEFI path. The rest of the `K0_QEMU_*` variables work as before.

| Variable | Meaning |
| --- | --- |
| `K0_QEMU_FIRMWARE` | Path of the UEFI firmware image. Default: the search described above |
| `K0_QEMU_ESP` | If set, QEMU boots the loader from a FAT drive, as a real computer boots a removable disk, instead of passing it with `-kernel`. The runner makes the drive from the folder `<file>.efi.esp` and puts the loader at `EFI/BOOT/BOOTAA64.EFI`, the standard path on AArch64 for a bootable removable drive |
| `K0_QEMU_EXTRA` | More QEMU arguments, split at spaces. For example `-device virtio-rng-pci` gives the firmware a random number source |
| `K0_LOADER_FEATURES` | Cargo features for the loader, given to `tools/build-loader.sh`. The only one is `refuse-halt`, explained below |

The FAT drive path:

```sh
K0_QEMU_ESP=1 tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel.efi
```

### What the loader prints

Firmware messages come first. Then the loader prints lines that start with `k0-loader:`, and then the kernel's own lines follow.

| Line | Meaning |
| --- | --- |
| `k0-loader: start at el1, kernel image 664 KiB with 4K granule, sha-256 b4554f24` | The loader runs at this exception level, and shows the size, the page size and the first bytes of the SHA-256 of the kernel inside it |
| `k0-loader: entropy none from the firmware` | The firmware has no random number source (or `entropy from the firmware` when it has one, which the loader then hands to the kernel) |
| `k0-loader: platform con=pl011@0x9000000 gic=v3@0x8000000 ...` | The console, interrupt controller and timer that the kernel found in the device tree |
| `k0-loader: memory 8 regions, 478 of 478 MiB usable kept, 0 ranges dropped, 27 MiB of boot services memory left unused` | The RAM the kernel will be told about. Boot services memory is never used, on purpose |
| `k0-loader: image at 0x5c410000, dtb at 0x5c1fc000` | Where the loader put the kernel and the device tree. Write it down if you need to read the boot log from memory |
| `k0-loader: exiting boot services` | The last line before the kernel starts. After this the firmware is out of the picture |

The numbers differ per machine and build. A line `k0-loader: the kernel will have no console (...)` warns that the kernel found no supported console, and `k0-loader: panic at <file>:<line>` means a bug in the loader.

### When the loader refuses

Before it leaves the firmware, the loader checks everything it can. If a check fails, it prints `k0-loader: refused: <reason>`, wipes and frees the memory it took, and returns an error to the firmware. The firmware then usually tries its next boot option. To make the loader stop instead, so that a machine boots K0 or nothing, build with `K0_LOADER_FEATURES=refuse-halt tools/build-loader.sh`. Once the loader has started to leave the firmware, every failure stops it.

| Reason | Meaning |
| --- | --- |
| `the firmware provides neither a device tree nor ACPI tables` | The configuration table has no device tree and no ACPI RSDP. QEMU with EDK2 always gives one of the two |
| `ACPI tables: <reason>` | An ACPI table is damaged, has a bad checksum or length, lies outside readable memory, or names a device in RAM or firmware memory (see design/en/uefi.md) |
| `device tree from ACPI tables: <reason>` | The values read from ACPI do not fit the minimal tree |
| `this cpu has no 16K translation granule` | A 16 KiB kernel on a CPU without 16 KiB pages. Use `K0_QEMU_CPU=max` |
| `embedded kernel image is corrupt (sha-256 mismatch)` | The kernel inside the file was changed after the build |
| `the firmware device tree ...` or `device tree for the kernel: ...` | The device tree from the firmware is damaged or has something the kernel refuses (see [design/en/uefi.md](design/en/uefi.md)) |

Only this loader may start K0 under UEFI. The kernel would also accept the firmware's device tree as it is, but on QEMU that tree lists memory that the firmware still uses. The loader replaces the memory description with one built from the UEFI memory map. Its design, its limits and what it does and does not protect against are in [design/en/uefi.md](design/en/uefi.md). Its place in the security model is in [SECURITY.md](SECURITY.md).

The loader has no signature check of its own. The SHA-256 of the kernel inside it only catches a damaged file. If you need to know who built the file, use Secure Boot in the firmware, with keys you enrol yourself: the firmware then checks the whole `.efi` file, kernel included.

What was tested: the UEFI rows of `tools/boot-matrix.sh` (see below) run in CI on QEMU with EDK2, and by hand on 2026-11-12 the rows `uefi-hvf-acpi`, `uefi-hvf-el1`, `uefi-hvf-el2` and `uefi-hvf-16k` on an Apple M5 Pro with HVF, and a signed loader under Secure Boot on one firmware (Debian's EDK2 2022.11 on QEMU 7.2), where an unsigned, a tampered and a wrong-key loader were refused by the firmware. Real ACPI machines, real boards, other firmware such as U-Boot, GICv4, redistributors described by CPU entries, the SBSA UART and 16550 consoles, KVM, the pinned build under UEFI and the cache flush on real hardware have not been tested.

## Testing automatically

```sh
tools/host-test.sh                    # unit tests on your computer, plus device tree fuzzing
cargo build -p k0-kernel --offline    # build the kernel
tools/qemu-boot-test.sh               # boot in QEMU and check every self-test
tools/boot-matrix.sh                  # boot every supported configuration (builds the 16 KiB, pinned and UEFI loader images too)
```

- `tools/host-test.sh` runs the 221 host tests (161 on an x86-64 computer): small tests of single parts of the kernel (unit tests) as ordinary programs on your computer. It also does fuzzing: it feeds the device tree parser and the UEFI loader's planning code random, broken input to find crashes. The 221 are 12 (`k0-abi`), 41 (`k0-cap`), 76 (`k0-boot`), 32 (`k0-efi`) and 30 for each of the two page sizes (`k0-mm`). The memory management tests use ARM instructions, so they run only on an ARM computer (an Apple Silicon Mac or Linux arm64), and the x86-64 count leaves out those 60.
- `tools/qemu-boot-test.sh` boots the kernel, reads the log line by line in a fixed order, fails on any error line, checks the exact number of intentional fault lines, and stops QEMU when it is done. It prints `boot-test: pass` on success and saves the full log in `target/qemu-boot-test.log`. It takes the machine from the `K0_QEMU_*` variables above and accepts `K0_BOOT_REUSE=skip` for memory layouts that have no untyped region of the right size, where the root task skips its reuse test.
- `tools/boot-matrix.sh` boots the kernel in each configuration that K0 calls supported and checks the result: QEMU virt with GICv3 and GICv2 at EL1 and EL2, three CPU models, 4 cores, the 16 KiB image, other load addresses, `xlnx-versal-virt`, a pinned image, and a static check that the image does not touch the physical timer. The `uefi-*` rows boot the kernel through the UEFI loader on EDK2: GICv3 and GICv2 at EL1 and EL2, the `max` CPU, 4 cores, the 16 KiB image, a random number source from the firmware and the FAT drive path, and the `uefi-acpi-*` rows (`uefi-acpi-gic3-el1`, `uefi-acpi-gic3-el2`, `uefi-acpi-gic2-el1`, `uefi-acpi-smp4`, `uefi-acpi-16k`, `uefi-acpi-rng`), which boot QEMU's default machine with ACPI on and no device tree. A static check (`tools/pe-check.py`) covers the loader file's form and its cache and MMU instruction order, which QEMU cannot test. For configurations that must be refused, it reads the reason from the boot log in memory, or for the loader from the serial log: a 16 KiB kernel on `cortex-a72`, and a kernel with one changed byte. You can name single rows as arguments. CI runs it. On an Apple Silicon Mac, `K0_MATRIX_HVF=1 tools/boot-matrix.sh` adds rows that run the kernel, also through the UEFI loader, on the real CPU cores through QEMU's HVF accelerator.

The default QEMU CPU model has no PAC, PAN or RNDR. To test those as well, boot on the `max` model and require the matching log lines:

```sh
K0_QEMU_CPU=max K0_BOOT_REQUIRE='rndr=on|k0: pac=on bti=present pan=on' \
  tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel
```

GitHub also runs these tests automatically on every pull request and every push to `main` (CI), on x86-64 and ARM64 Linux, inside sealed build environments (containers) with networking turned off, and runs a longer fuzzing campaign every week. To fuzz the device tree parser yourself or to run exactly what CI runs, see [CONTRIBUTING.md](CONTRIBUTING.md).

## When something goes wrong

| Problem | What to do |
| --- | --- |
| `qemu-system-aarch64` is not found | Install QEMU as described under "What you need" |
| `boot-test: kernel ELF not found` | Build first with `cargo build -p k0-kernel --offline` |
| The boot test times out on a slow computer | Give it more time, for example `K0_BOOT_TIMEOUT=300 tools/qemu-boot-test.sh` |
| The 16 KiB image does not boot on `cortex-a72` | Expected: that CPU has no 16 KiB page support. Use `K0_QEMU_CPU=max` |
| A boot stops early and nothing is printed | There may be no supported console. Read the boot log from memory with `tools/boot-log.py` |
| `runner: no UEFI firmware found, set K0_QEMU_FIRMWARE` | Install the firmware as described under "What you need", or set `K0_QEMU_FIRMWARE` to the path of an EDK2 `aarch64` code image |
| `build-loader: kernel ELF not found` | Build the kernel first with `cargo build -p k0-kernel --offline` |
| The screen shows only firmware messages, or the firmware menu | The firmware could not start the loader, or the loader refused and the firmware went on. Look for `k0-loader:` lines. With none, check that the `.efi` file exists and, with `K0_QEMU_ESP`, that the drive is found |
