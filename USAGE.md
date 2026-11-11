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
cargo virt --release                                          # optimized build
cargo virt -- -cpu max                                        # newer CPU model with more security features (PAC, PAN, RNDR)
cargo virt -- -machine virt,gic-version=3,virtualization=on   # ask QEMU to start the kernel at the hypervisor level (EL2)
cargo virt -- -m 4G                                           # more memory
```

Everything after `--` is passed to QEMU.

## Testing automatically

```sh
tools/host-test.sh                    # unit tests on your computer, plus device tree fuzzing
cargo build -p k0-kernel --offline    # build the kernel
tools/qemu-boot-test.sh               # boot in QEMU and check every self-test
```

- `tools/host-test.sh` runs small tests of single parts of the kernel (unit tests) as ordinary programs on your computer. It also does fuzzing: it feeds the device tree parser random, broken input to find crashes. The memory management tests use ARM instructions, so they run only on an ARM computer (an Apple Silicon Mac or Linux arm64).
- `tools/qemu-boot-test.sh` boots the kernel, reads the log line by line in a fixed order, fails on any error line, checks the exact number of intentional fault lines, and stops QEMU when it is done. It prints `boot-test: pass` on success and saves the full log in `target/qemu-boot-test.log`.

The default QEMU CPU model has no PAC, PAN or RNDR. To test those as well, boot on the `max` model and require the matching log lines:

```sh
K0_BOOT_REQUIRE='rndr=on|k0: pac=on bti=present pan=on' \
  tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel -cpu max
```

GitHub also runs these tests automatically on every pull request and every push to `main` (CI), on x86-64 and Arm64 Linux, inside sealed build environments (containers) with networking turned off, and runs a longer fuzzing campaign every week. To fuzz the device tree parser yourself or to run exactly what CI runs, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Building for Apple Silicon

```sh
cargo apple
```

This builds the version for Apple Silicon, which uses 16 KiB memory pages and is loaded by the m1n1 boot loader. It has only been compiled and host-tested. It has never been booted on a real machine.

`cargo apple` writes to the same file as the optimized QEMU build (`target/aarch64-unknown-none-softfloat/release/k0-kernel`). If you boot that file in QEMU right after `cargo apple`, for example with `tools/qemu-boot-test.sh`, it is the Apple build and will not boot. Rebuild it first with `cargo build -p k0-kernel --offline --release`. `cargo virt --release` also rebuilds it before booting.

## When something goes wrong

| Problem | What to do |
| --- | --- |
| `qemu-system-aarch64` is not found | Install QEMU as described under "What you need" |
| `boot-test: kernel ELF not found` | Build first with `cargo build -p k0-kernel --offline` |
| The boot test times out on a slow computer | Give it more time, for example `K0_BOOT_TIMEOUT=300 tools/qemu-boot-test.sh` |
| The optimized file in `target/.../release/` does not boot in QEMU after `cargo apple` | The file holds the Apple build. Rebuild with `cargo build -p k0-kernel --offline --release` |
