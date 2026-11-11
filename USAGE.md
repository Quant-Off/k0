# Using K0

<div align="center">

English | [한국어](USAGE_KR.md)

</div>

This document shows how to build K0, run it, and check that it works. You do not need any ARM hardware. Everything runs in QEMU, a program that behaves like an ARM computer.

## What you need

- `rustup`, the Rust installer. On the first build it downloads the exact compiler and tools K0 uses (the toolchain: Rust 1.89.0 and the parts listed in `rust-toolchain.toml`) by itself.
- `qemu-system-aarch64` with its firmware files. On macOS, run `brew install qemu`. On Debian or Ubuntu, install the `qemu-system-arm` and `ipxe-qemu` packages.
- A C compiler for the build scripts: the Xcode command line tools on macOS, or `gcc`.

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

## Testing automatically

```sh
tools/host-test.sh                    # unit tests on your computer, plus device tree fuzzing
cargo build -p k0-kernel --offline    # build the kernel
tools/qemu-boot-test.sh               # boot in QEMU and check every self-test
tools/boot-matrix.sh                  # boot every supported configuration (builds the 16 KiB and pinned images too)
```

- `tools/host-test.sh` runs the 183 host tests (123 on an x86-64 computer): small tests of single parts of the kernel (unit tests) as ordinary programs on your computer. It also does fuzzing: it feeds the device tree parser random, broken input to find crashes. The memory management tests use ARM instructions, so they run only on an ARM computer (an Apple Silicon Mac or Linux arm64).
- `tools/qemu-boot-test.sh` boots the kernel, reads the log line by line in a fixed order, fails on any error line, checks the exact number of intentional fault lines, and stops QEMU when it is done. It prints `boot-test: pass` on success and saves the full log in `target/qemu-boot-test.log`. It takes the machine from the `K0_QEMU_*` variables above and accepts `K0_BOOT_REUSE=skip` for memory layouts that have no untyped region of the right size, where the root task skips its reuse test.
- `tools/boot-matrix.sh` boots the kernel in each configuration that K0 calls supported and checks the result: QEMU virt with GICv3 and GICv2 at EL1 and EL2, three CPU models, 4 cores, the 16 KiB image, other load addresses, `xlnx-versal-virt`, a pinned image, and a static check that the image does not touch the physical timer. For configurations that must be refused, it reads the reason from the boot log in memory. You can name single rows as arguments. CI runs it. On an Apple Silicon Mac, `K0_MATRIX_HVF=1 tools/boot-matrix.sh` adds rows that run the kernel on the real CPU cores through QEMU's HVF accelerator.

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
