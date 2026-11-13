# Using K0

<div align="center">

English | [한국어](USAGE_KR.md)

</div>

This document shows how to build K0, run it, and check that it works. You do not need any ARM hardware. Everything runs in QEMU, a program that behaves like an ARM computer.

## What you need

- **rustup**: the Rust installer. On the first build it downloads the exact compiler and tools K0 uses (the toolchain: Rust 1.89.0 and the parts listed in `rust-toolchain.toml`) by itself.
- **QEMU**: you need `qemu-system-aarch64` with its firmware files. On macOS, run `brew install qemu`. On Debian or Ubuntu, install the `qemu-system-arm` and `ipxe-qemu` packages.
- **C compiler**: it is needed for the build scripts. Use the Xcode command line tools on macOS, or `gcc` elsewhere.
- **UEFI firmware**: it is for QEMU and is needed only to boot through UEFI (see "Booting through UEFI" below). Homebrew's `qemu` already contains it. On Debian or Ubuntu, install the `qemu-efi-aarch64` package.

Every outside library is already in the `vendor/` folder. Once the toolchain is installed, building therefore needs no internet connection.

## Running K0

```sh
$ cargo virt
```

This builds the kernel with a console driver and starts it in QEMU. The boot messages appear in your terminal. When the self-tests are done, the kernel keeps running and prints `k0: tick N` once a second.

> [!TIP]
> The kernel never exits on its own. To quit QEMU, press `Ctrl-A`, then `X`.

### Console features

Input and output are optional in K0. You can select them with Cargo features of the kernel crate.

> [!NOTE]
> Console input and the operator terminal are features too, but they are off by default. They are described in "Operator terminal" below.

Console output exists only in an image built with a console feature. An image has input only if it was built with `console-input` or `terminal`. `cargo virt` is an alias for `cargo run -p k0-kernel --features console`, so it always includes the console. The following picture shows where the output goes, depending on the features in the build.

<p align="center">
  <img src="assets/console-features.svg" alt="Output path by console feature. An image built without a feature writes only to the boot log in memory, and an image built with a console feature also writes to the matching UART.">
</p>

The following table lists the features and what each one adds.

| Feature | What it adds |
| --- | --- |
| `console-pl011` | A transmit driver for the PL011 UART and for the SBSA UART, which uses the same registers |
| `console-16550` | A transmit driver for the 16550 family of UARTs |
| `console` | Both drivers |

None of them is in `default`. `default` holds only `granule-4k`. The features only add console output and are never made default, and the 16 KiB image is built with `--no-default-features`. Therefore **every command has to name the features it wants**. You can enter them as follows:

```sh
$ cargo build -p k0-kernel --features console --offline                                                             # 4 KiB image with console
$ cargo build -p k0-kernel --offline --no-default-features --features granule-16k,console --target-dir target/g16   # 16 KiB image with console
$ cargo build -p k0-kernel --offline                                                                                # image without console
```

An image built without a console feature contains no UART driver code. It never maps the console device page and writes nothing to a UART. Every kernel line and every line from the first program goes only to the boot log in kernel memory (see "Reading the boot log from memory" below). The kernel then prints `k0: no console (driver not built in)`. An image with one driver treats a console of the other kind the same way. For example, QEMU virt has a PL011, so an image with only `console-16550` stays silent there.

Some things do not change with the features.

- The kernel still finds and validates the console node of the device tree. A device tree whose console fails validation is therefore refused even by an image that would never use the console.
- The `DEBUG_PUTC` system call and the Console capability are still there. The output of the first program still reaches the boot log.
- The UEFI loader still prints its own lines through the firmware console.

**All feature sets write the same ELF path**. To keep a console image, build the other one into its own folder, for example with `--target-dir target/quiet`.

To see which console drivers an image was built with, without booting it, you can enter the following:

```sh
$ grep -a 'k0: console drivers' <image file>
```

The line reads `k0: console drivers [pl011 16550]`, or `[pl011]`, `[16550]` or `[none]`. The kernel prints the same line first when it brings up the platform. It is a label, not proof. A plain `cargo run -p k0-kernel` boots the image without console, and `tools/qemu-virt-runner.sh` then tells you so.

### Reading the output

The following table lists the lines that an image with a console driver prints, and what they mean. Lines that start with `k0:` come from the kernel. Lines that start with `root:` come from the root task, the first program.

| Line | Meaning |
| --- | --- |
| `root: hello from EL0` | The first program is running in user mode |
| `k0: dtb seeds wiped <n>B` | Right before the root task starts, the kernel zeroed the random seed bytes (`rng-seed`, `kaslr-seed`) in the device tree copy and read them back. It reads `40B` on QEMU virt. It is printed by every image |
| `root: boot seed from rng-seed bytes 0x... and rndr` or `... without rndr` | How much randomness the kernel put into the boot seed it gave the root task: the number of `rng-seed` bytes, and whether the CPU random number instruction RNDR added to it. The seed itself is never printed. It is printed by every image |
| `root: ... pass` | One group of self-tests passed |
| `root: secret at 0x...`, then `k0: task fault ... far 0x...` with the same address | A confined task tried to read memory that only the root task may see, and it was stopped at exactly that address |
| `k0: task fault ...` and `k0: unknown syscall 65535, killing task` | Rule violations that the tests cause on purpose. Each one stops only the task that broke the rule. A normal run has exactly 6 `k0: task fault` lines and 1 `k0: unknown syscall` line. An image built with a `vault` feature has 8 `k0: task fault` lines |
| `k0: kernel reserve <T> tables + <E> address space entries = <B> bytes in <N> ranges` | The kernel set aside memory at the top of each range it hands out, for its own page tables and for the list of address spaces. It is printed before the `k0: untyped` lines. On QEMU virt with 512 MiB it reads `262 tables + 130130 address space entries = 0x206000 bytes in 3 ranges`. The entry count depends on the image size |
| `k0: memory <base> + <size> not handed out (<reason>)` | A memory range was too small to hold its own share of that reserve, or the share could not be mapped, so the kernel did not hand the range out. The boot continues. A normal run on QEMU virt does not print it |
| `root: timeout tests pass` | The self-tests of IPC timeouts passed |
| `root: slice test yield pass` and `root: slice test block pass` | A task that got the CPU between two ticks (by `YIELD`, or after the previous task blocked) was not cut by the first tick it met. They come before `root: preempt test pass` |
| `root: preempt test pass` | A task that never yields is still cut by the timer |
| `root: kernel reserve tests pass` | The self-tests of the kernel reserve passed (300 address spaces alive at once, and frames made from untyped memory across many page table blocks) |
| `root: oracle tests pass` | The self-tests of the signing oracle passed. Only an image built with a `vault` feature prints it (see "Signing oracle" below) |
| `root: oracle made no key (boot seed has no entropy)` | The vault refused to make a key because the boot seed was too weak. Only an image built with a `vault` feature can print it, and then `root: oracle tests pass` does not follow |
| `root: sched tests pass` | The last group of self-tests passed |
| `k0: idle (waiting for a timeout)` | Every task was waiting and at least one wait had a time limit, so the kernel waited for the next interrupt instead of stopping. It is printed at most once per boot |
| `k0: all tasks stopped (none ready, no timeout pending)` | Every task is waiting and no wait has a time limit. Nothing can wake a task today, so the machine stays idle. A normal run never prints it, and the boot test treats it as a failure. It is printed at most once per boot |
| `k0: tick N` | The kernel is still running |

Lines such as `root: FAIL`, `k0: FATAL`, `k0: PANIC` or `k0: EXCEPTION` mean that **something went wrong**. The automatic boot test described under "Testing automatically" checks the whole list for you.

### Other ways to run

You can change how K0 runs by setting environment variables, as follows:

```sh
$ cargo virt --release                                              # optimized build
$ K0_QEMU_CPU=max cargo virt                                        # newer CPU model with more security features (PAC, PAN, RNDR)
$ K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on cargo virt   # ask QEMU to start the kernel at the hypervisor level (EL2)
$ K0_QEMU_MACHINE=virt,gic-version=2 cargo virt                     # a GICv2 interrupt controller
$ K0_QEMU_MEM=4G cargo virt                                         # more memory
```

The runner script `tools/qemu-virt-runner.sh` builds the QEMU command line from these variables. Setting a variable to an empty value leaves that QEMU option out. The following table lists the variables and their defaults.

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

The kernel uses 4 KiB memory pages by default. The 16 KiB version is a build option and cannot be selected at run time. You can build and test it as follows:

```sh
$ cargo build -p k0-kernel --offline --no-default-features --features granule-16k,console --target-dir target/g16
$ K0_QEMU_CPU=max tools/qemu-boot-test.sh target/g16/aarch64-unknown-none-softfloat/debug/k0-kernel
```

`--no-default-features` is needed because the 4 KiB feature is on by default, and the build stops if both are on. `--target-dir` keeps the 16 KiB image apart from the normal one. The 16 KiB image refuses to boot on a CPU without 16 KiB page support, for example `cortex-a72`.

> [!IMPORTANT]
> Because `--no-default-features` is used, the console feature must be named too.

### Pinned build

To tie an image to one platform, boot the target once, copy the text after `k0: platform ` from its boot log, and build with it in `K0_PIN`. You can enter it as follows:

```sh
$ K0_PIN='<text after "k0: platform ">' cargo build -p k0-kernel --features console --offline --target-dir target/pin
```

The pinned image refuses to boot when the discovered platform, the entry level or the RAM range differs. A pinned build names the features it wants, like any other build. This catches a wrong board or a swapped device tree.

> [!NOTE]
> A pinned build is not a defence against a hostile boot chain, because the comparison uses values taken from the device tree.

### Reading the boot log from memory

Every line the kernel prints is also kept in a 16 KiB buffer in memory. EL0 output may use 6 KiB of it, and only the first 4 timer tick lines are logged. If the kernel refuses to boot, there is no supported console, or the image was built without a console feature, **this buffer is the only record**. If QEMU was started with `K0_QEMU_MONITOR=<socket path>`, you can read it as follows:

```sh
$ tools/boot-log.py <kernel ELF> <load address> <QEMU monitor socket>
```

The load address is the physical address where the image was loaded. On QEMU `virt` with an unchanged image it is `0x40080000`.

### Signing oracle

The signing oracle is the first service that runs on top of the isolation core. A vault task holds one Ed25519 key. Client tasks get signatures and the public key from it over IPC and never see the key. The oracle is optional and off by default. The kernel crate has two features for it, and features only add:

| Feature | What it adds |
| --- | --- |
| `vault` | The vault service with its protocol, its fixed policy and its self-test, but no signing algorithm. Every signing request is answered with "unsupported" |
| `vault-ed25519` | Includes `vault` and adds Ed25519 (PureEdDSA with SHA-512, as in FIPS 186-5 and RFC 8032) |

The code of the vault is part of the root task image, so a build must name the feature, and `cargo virt` does not include it. You can build and run an image with the oracle as follows:

```sh
$ cargo run -p k0-kernel --features console,vault-ed25519
```

You can also check it with the boot test. The self-test of the oracle adds two intended faults, so the test has to expect 8 `k0: task fault` lines instead of 6. You can enter it as follows:

```sh
$ cargo build -p k0-kernel --features console,vault-ed25519 --offline
$ K0_BOOT_FAULTS=8 K0_BOOT_REQUIRE='k0: dtb seeds wiped 40B|root: oracle tests pass' tools/qemu-boot-test.sh
```

In the boot log you should see these lines, in addition to the lines in "Reading the output":

- `k0: dtb seeds wiped 40B` and `root: boot seed from rng-seed bytes 0x0000000000000020 without rndr` (with `K0_QEMU_CPU=max` the second line ends with `and rndr`). QEMU virt provides a 32-byte `rng-seed`, which is `0x20`.
- Two more `k0: task fault` lines, 8 in total. They come from two clients that the self-test lets break the memory rules on purpose.
- `root: oracle tests pass` before `root: sched tests pass`. This line means that all the checks of the oracle passed: the vault made its key from the public RFC 8032 test seed, a signer got a signature that verifies, a changed message or signature does not verify, the admin and a reader were refused when they asked to sign, and signing was refused after the key was erased.

The vault makes a key only if the boot seed holds at least 32 `rng-seed` bytes or 256 bits from RNDR. If it does not, for example on a machine whose device tree has no `rng-seed` and whose CPU has no RNDR, the vault answers `NO_ENTROPY` and makes no key. The root task then prints `root: oracle made no key (boot seed has no entropy)` and stops the oracle test there. `root: oracle tests pass` does not appear, so a boot test that requires it fails, which is the intended result.

> [!NOTE]
> The implementation of Ed25519 is the vendored crate `ed25519-dalek` 2.2.0 and has no validation of any kind, and the key does not come from an approved random bit generator. There is one key, no persistence, no key import or export and no authentication of the operator. The availability of the vault is not protected, because all clients share one endpoint and must use timeouts. The root task is trusted and could read the vault. The oracle was only run in QEMU (TCG and HVF). The details and the limits are in [SECURITY.md](SECURITY.md).

### Operator terminal

The operator terminal is a small task that lets you type commands on the serial line and talk to the vault. It is optional and off by default. Two features of `k0-kernel` select it, and features only add.

| Feature | Adds |
| --- | --- |
| `console-input` | The kernel's receive path for the console UART and the system call `CONSOLE_READ`. It needs a console driver feature to do anything. Receive exists for PL011 and SBSA UART only, so a 16550 console has output and no input |
| `terminal` | The terminal task in the root task image, started as a service after the self-tests. It turns `console-input` on. With `vault` the terminal is the vault's operator. With `vault-ed25519` it can make an Ed25519 key |

An image without `console-input` has no receive code, no `CONSOLE_READ` and none of the new log lines. An image without `terminal` has no terminal code. You can build and run it as follows:

```sh
$ cargo build -p k0-kernel --features console,terminal,vault-ed25519
$ cargo run -p k0-kernel --features console,terminal,vault-ed25519
```

`cargo run` starts QEMU with the serial port on your terminal. QEMU's own escape key is `Ctrl-A` there (`Ctrl-A`, then `X` quits QEMU), so `Ctrl-A` never reaches K0.

After the self-tests, the root task starts the vault, hands it the boot seed, runs two hostile tasks wired like the terminal (they must be refused and end in a fault), drops its admin capability and starts the terminal. The boot log shows `root: vault seeded`, `root: terminal console filter [...]`, `root: terminal wiring holds`, two `k0: task fault` lines, `root: service started` and then the banner. On a machine without an input path it shows `root: no console input, terminal not started`. After the first read, the periodic `k0: tick N` lines stop, and one line `k0: tick lines off (console input in use)` marks that. In the following session, every command ends with exactly one line, `ok` or `err <reason>`:

```
K0 operator console, type 'help'
k0> vault status
state=seeded
alg=none
selftest=passed
seed=rng-seed
validated=no
ok
k0> vault keygen ed25519
state=ready
alg=ed25519
ok
k0> vault pubkey
pubkey=<32 hex characters>
       <32 hex characters>
ok
k0> vault zeroize
hint=type 'vault zeroize confirm' to erase the key
err confirm-required
k0>
```

Values are printed as `name=value`. The screen prompt is `k0> `. In the following table, the first column is what you type:

| Command | Argument | What it does |
| --- | --- | --- |
| `help` | | Lists the commands |
| `vault status` | | Prints the vault state (see below) |
| `vault pubkey` | | Prints the public key |
| `vault keygen <alg>` | `ed25519` | Makes the key from the held seed with the chosen algorithm and wipes the seed |
| `vault zeroize confirm` | | Erases the key. `vault zeroize` alone prints `hint=type 'vault zeroize confirm' to erase the key` and `err confirm-required` |

`vault status` prints `state=fresh|seeded|ready|zeroized|failed`, `alg=none|ed25519`, `selftest=passed|failed`, `seed=none|rng-seed|rndr|rng-seed+rndr` and `validated=no`. `seed=rng-seed` means that the bytes were present, not that they were unpredictable. `validated=no` is always printed, because the implementation has no validation of any kind. `vault pubkey` prints `pubkey=` and 32 hex characters, then a second line indented by 7 spaces with the other 32. A vault makes one key per boot: a second `vault keygen` gives `err key-exists`, and after `vault zeroize confirm` you have to reboot. Without the `vault` feature, `help` lists only `help` and `vault ...` answers `err no-vault`.

In the following table, the keys work on the line you are typing:

| Key | Effect |
| --- | --- |
| Enter (CR or LF) | Runs the line. A line has at most 64 characters, printable ASCII only |
| Backspace or DEL | Erases one character |
| `Ctrl-C` or `Ctrl-U` | Cancels the line (shown as `^C` or `^U`) |
| ESC sequences (arrow keys) | Ignored |

Bytes that reached the UART before the prompt are dropped. If the input was lost (a FIFO overrun, a break, a framing or parity error), everything up to the next line end is discarded and the command answers `err input-lost`. A keygen or zeroize that was delivered but not answered in time adds `hint=run 'vault status' to see whether it took effect`. In the following table, the error names are what comes after `err `:

| Error | Meaning |
| --- | --- |
| `unknown-command` | The command is not known |
| `bad-argument` | An argument is wrong or missing |
| `line-too-long` | The line is longer than 64 characters |
| `input-lost` | Input was lost before the line end |
| `confirm-required` | `vault zeroize` without `confirm` |
| `denied` | The operation is not allowed for the operator |
| `no-vault` | The image has no vault |
| `no-key` | The vault has no key |
| `key-exists` | The vault already made its key this boot |
| `no-entropy` | The boot seed has no entropy |
| `unsupported-algorithm` | The algorithm is not in this image |
| `zeroized` | The key was erased; a reboot is needed |
| `vault-failed` | The vault failed and wiped seed and key |
| `vault-timeout` | The vault did not answer in time |

> [!WARNING]
> There is no authentication on the serial line. Whoever holds the line is the operator and can do exactly the operator's four operations (`STATUS`, `PUBLIC_KEY`, `KEYGEN`, `ZEROIZE`), including erasing the key. The operator cannot get a signature, cannot deliver a seed and cannot change policy.

What stays true, and what is not covered:

- **Kernel code**: the receive code runs at EL1. It is small and never interprets a byte, but it is kernel code driven by an outside party. A user-space driver needs device and interrupt delegation, which K0 does not have.
- **Timing**: an outside party can now cause interrupts while tasks run (bounded by the reader), and the reader learns arrival times finer than the tick. Other tasks can notice the stolen time. This timing channel is not analysed.
- **Latency**: input waits behind running tasks. There are no priorities, so the latency is up to a tick per ready task, plus one more tick when the running task was switched in between two ticks.
- **Hardware**: a 16550 console has no input. The SBSA UART path, FIFO overruns and the receive timeout interrupt are not exercised by QEMU, only by host tests with a fake UART. The cut-off for an interrupt line that fires without a UART cause is also covered only by host tests. No test measures kernel time under a flood on a machine. Nothing ran on real hardware.
- **Screen**: if a kernel line appears while you are typing, the screen line is split but the buffer is whole, and Enter runs the whole buffer. Any task with `Console` output can still print lines that look like kernel or root task lines, and nothing authenticates the prefix. The terminal can additionally erase within its current line.
- **Bytes held back**: "dropped before the prompt" counts bytes that reached the UART. An emulator or adapter that holds bytes back delivers them later (QEMU does).
- **Service**: no signing client exists in service mode yet. The key is made and shown but not used by another task. The terminal and the vault are not restarted if they die. The root task is trusted.
- **Boot log**: terminal output counts against the user share of the in-memory boot log. Once that share is full, later task output is not recorded there.
- **Vault**: all earlier limits stay. The implementation is unvalidated, there is no side-channel analysis, the key does not come from an approved random bit generator, there is no persistence and there is no PQC. The availability of the vault and of the terminal is not protected.

If the console interrupt number is wrong, only the input path is lost: the boot log shows `k0: console input off (...)`. The reasons are `no console`, `no receive driver for this uart`, `no usable interrupt in the device tree`, `register frame too small`, `console not attached`, `interrupt number out of range` and `interrupt not available`. A line that keeps firing without a UART cause is switched off after 8 entries with `k0: console input off (interrupt N fired without a uart cause)`. With input on, the boot log shows `k0: console input on (pl011 irq 33)`. The interrupt number is part of the platform line (`k0: platform con=pl011@0x9000000,irq33 ...`), so a `K0_PIN` made before this change no longer matches and must be made again.

## Booting through UEFI

UEFI is the standard way for firmware to start a program on many computers. The firmware loads a program file with the ending `.efi` from a disk, runs it, and describes the machine to it. A loader is a small program that does this one job: it prepares the machine and then starts another program. K0 has its own UEFI loader, `crates/k0-efi` and `loader/`. It carries the kernel inside itself, so the one `.efi` file is all the firmware needs. The kernel does not change and boots the same way as without UEFI. The following picture shows the path of a UEFI boot.

<p align="center">
  <img src="assets/usage-uefi-path.svg" alt="Path of a UEFI boot. The kernel is built, tools/build-loader.sh embeds it into a loader .efi file, the firmware starts the loader, and the loader starts the kernel.">
</p>

You can build the kernel and the loader and run them in QEMU as follows:

```sh
$ cargo build -p k0-kernel --features console --offline
$ tools/build-loader.sh
$ tools/qemu-virt-runner.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel.efi
```

QEMU's default machine has ACPI on and gives no device tree. The loader reads the ACPI static tables and builds a minimal device tree from them, so that machine boots too. You can try it as follows:

```sh
$ K0_QEMU_MACHINE=virt,gic-version=3 tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel.efi
```

The loader then prints `k0-loader: device tree built from ACPI tables (MADT, GTDT and SPCR)`. The runner still uses `acpi=off` for a `.efi` file unless you set `K0_QEMU_MACHINE`.

`tools/build-loader.sh` takes an already built kernel and embeds it into the loader. Without an argument it uses the debug 4 KiB kernel above. It prints the path of the result, which is the kernel file with `.efi` added. Before that it prints `build-loader: embedding a kernel with: k0: console drivers [...]`, taken from the kernel image, so that nobody embeds a silent kernel unknowingly. To embed another kernel, pass its path, for example the 16 KiB build from "16 KiB build".

The loader prints its own lines through the firmware console before it exits boot services, whatever the embedded kernel was built with. After that, a kernel without console prints nothing more to the screen.

The runner starts QEMU with the EDK2 firmware (a UEFI firmware that QEMU can load) and the machine `virt,gic-version=3,acpi=off`. It looks for the firmware in Homebrew's QEMU and then in `/usr/share/AAVMF/AAVMF_CODE.fd`. To quit QEMU, press `Ctrl-A`, then `X`.

To test it, give the `.efi` file to the boot test:

```sh
$ tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel.efi
```

The memory judgement of the boot test (`K0_BOOT_MEMLOG`, see "Testing an image without console" below) cannot be used with a UEFI loader, because the load address is not fixed. The boot test starts judging at the loader's first line, because the firmware prints its own messages before it, and some of those contain words like `failed`. Set `K0_BOOT_REUSE=skip` if you change the machine to a memory layout without an untyped region of the right size, as described under "Testing automatically".

The variables in the following table belong to the UEFI path. The rest of the `K0_QEMU_*` variables work as before.

| Variable | Meaning |
| --- | --- |
| `K0_QEMU_FIRMWARE` | Path of the UEFI firmware image. Default: the search described above |
| `K0_QEMU_ESP` | If set, QEMU boots the loader from a FAT drive, as a real computer boots a removable disk, instead of passing it with `-kernel`. The runner makes the drive from the folder `<file>.efi.esp` and puts the loader at `EFI/BOOT/BOOTAA64.EFI`, the standard path on AArch64 for a bootable removable drive |
| `K0_QEMU_EXTRA` | More QEMU arguments, split at spaces. For example `-device virtio-rng-pci` gives the firmware a random number source |
| `K0_LOADER_FEATURES` | Cargo features for the loader, given to `tools/build-loader.sh`. The only one is `refuse-halt`, explained under "When the loader refuses" |

You can test the FAT drive path as follows:

```sh
$ K0_QEMU_ESP=1 tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel.efi
```

### What the loader prints

Firmware messages come first. Then the loader prints lines that start with `k0-loader:`, and then the kernel's own lines follow. The following table lists the loader's lines and what they mean.

| Line | Meaning |
| --- | --- |
| `k0-loader: start at el1, kernel image 664 KiB with 4K granule, sha-256 b4554f24` | The loader runs at this exception level, and shows the size, the page size and the first bytes of the SHA-256 of the kernel inside it |
| `k0-loader: entropy none from the firmware` | The firmware has no random number source (or `entropy from the firmware` when it has one, which the loader then hands to the kernel) |
| `k0-loader: platform con=pl011@0x9000000 gic=v3@0x8000000 ...` | The console, interrupt controller and timer that the kernel found in the device tree |
| `k0-loader: memory 8 regions, 478 of 478 MiB usable kept, 0 ranges dropped, 27 MiB of boot services memory left unused` | The RAM the kernel will be told about. Boot services memory is never used, on purpose |
| `k0-loader: image at 0x5c410000, dtb at 0x5c1fc000` | Where the loader put the kernel and the device tree. Write it down if you need to read the boot log from memory |
| `k0-loader: exiting boot services` | The last line before the kernel starts. After this the firmware is out of the picture |

The numbers differ per machine and build. A line `k0-loader: the kernel will have no console (...)` warns that the kernel found no supported console. A line `k0-loader: panic at <file>:<line>` means a bug in the loader.

### When the loader refuses

Before it leaves the firmware, the loader checks everything it can. If a check fails, it prints `k0-loader: refused: <reason>`, wipes and frees the memory it took, and returns an error to the firmware. The firmware then usually tries its next boot option. To make the loader stop instead, so that a machine boots K0 or nothing, build with `K0_LOADER_FEATURES=refuse-halt tools/build-loader.sh`. Once the loader has started to leave the firmware, every failure stops it. The following table lists the reasons and what they mean.

| Reason | Meaning |
| --- | --- |
| `the firmware provides neither a device tree nor ACPI tables` | The configuration table has no device tree and no ACPI RSDP. QEMU with EDK2 always gives one of the two |
| `ACPI tables: <reason>` | An ACPI table is damaged, has a bad checksum or length, lies outside readable memory, or names a device in RAM or firmware memory (see design/en/uefi.md) |
| `device tree from ACPI tables: <reason>` | The values read from ACPI do not fit the minimal tree |
| `this cpu has no 16K translation granule` | A 16 KiB kernel on a CPU without 16 KiB pages. Use `K0_QEMU_CPU=max` |
| `embedded kernel image is corrupt (sha-256 mismatch)` | The kernel inside the file was changed after the build |
| `the firmware device tree ...` or `device tree for the kernel: ...` | The device tree from the firmware is damaged or has something the kernel refuses (see [design/en/uefi.md](design/en/uefi.md)) |

> [!IMPORTANT]
> Only this loader may start K0 under UEFI. The kernel would also accept the firmware's device tree as it is, but on QEMU that tree lists memory that the firmware still uses.

The loader replaces the memory description with one built from the UEFI memory map. Its design, its limits and what it does and does not protect against are in [design/en/uefi.md](design/en/uefi.md). Its place in the security model is in [SECURITY.md](SECURITY.md).

The loader has **no signature check of its own**. The SHA-256 of the kernel inside it only catches a damaged file. If you need to know who built the file, use Secure Boot in the firmware, with keys you enrol yourself: the firmware then checks the whole `.efi` file, kernel included.

What was tested:

- **CI**: the UEFI rows of `tools/boot-matrix.sh` (see "Testing automatically" below) run on QEMU with EDK2.
- **By hand, 2026-11-12**: the rows `uefi-hvf-acpi`, `uefi-hvf-el1`, `uefi-hvf-el2` and `uefi-hvf-16k` ran on an Apple M5 Pro with HVF, and a signed loader ran under Secure Boot on one firmware (Debian's EDK2 2022.11 on QEMU 7.2). An unsigned, a tampered and a wrong-key loader were refused by the firmware.
- **Not tested**: real ACPI machines, real boards, other firmware such as U-Boot, GICv4, redistributors described by CPU entries, the SBSA UART and 16550 consoles, KVM, the pinned build under UEFI and the cache flush on real hardware.

## Testing automatically

Automatic testing has three tools. The following picture shows how they relate.

<p align="center">
  <img src="assets/usage-test-flow.svg" alt="How the test tools relate. tools/host-test.sh runs unit tests and fuzzing on your computer. tools/qemu-boot-test.sh boots one configuration and judges it from the serial port or from the boot log in guest memory. tools/boot-matrix.sh boots every supported configuration.">
</p>

You can run them one after the other as follows:

```sh
$ tools/host-test.sh                                      # unit tests on your computer, plus device tree fuzzing
$ cargo build -p k0-kernel --features console --offline   # build the kernel with console
$ tools/qemu-boot-test.sh                                 # boot in QEMU and check every self-test
$ tools/boot-matrix.sh                                    # boot every supported configuration (builds the 16 KiB, pinned and UEFI loader images too)
```

- `tools/host-test.sh` runs the 363 host tests (277 on other hosts): small tests of single parts of the kernel (unit tests) as ordinary programs on your computer. It also does **fuzzing**: it feeds the device tree parser and the UEFI loader's planning code random, broken input to find crashes. The 363 are 15 (`k0-abi`), 44 (`k0-cap`), 85 (`k0-boot`), 33 (`k0-efi`), 15 (`k0-conin`), 9 (`k0-slice`, the rule that decides whether a tick cuts the running task, run on a model of the scheduler), 31 (`k0-vault`, which is 20 with Ed25519 and 11 without), 45 (`k0-term`, which is 24 with the vault feature and 21 without) and 43 for each of the two page sizes (`k0-mm`). The memory management tests use ARM instructions, so they run only on an ARM computer (an Apple Silicon Mac or Linux arm64), and the count on other hosts leaves out those 86.
- `tools/qemu-boot-test.sh` boots the kernel and reads the log line by line in a fixed order. It fails on any error line, checks the exact number of intentional fault lines, and stops QEMU when it is done. The root task's timeout tests wait for real timer ticks, so a boot takes about 4 seconds longer than it did before they existed. The time slice test adds two runs of about 4 seconds in total where the preemption test it replaced took about 1. The test also counts the tick lines between `root: teardown tests pass` and `root: timeout tests pass` and wants at least 4, which is how it checks that a timeout does not end early. It prints `boot-test: pass` on success and saves the full log in `target/qemu-boot-test.log`. It takes the machine from the `K0_QEMU_*` variables above. It accepts `K0_BOOT_REUSE=skip` for memory layouts that have no untyped region of the right size, where the root task skips its reuse test. It reads the kernel lines from the **serial port**, so it needs an image with a console driver. For an image without one, see "Testing an image without console" below. If the serial log has no kernel line, it prints a hint about a kernel built without a console feature.
- `tools/boot-matrix.sh` boots the kernel in **each** configuration that K0 calls supported and checks the result. It boots QEMU virt with GICv3 and GICv2 at EL1 and EL2, and also three CPU models, 4 cores, the 16 KiB image, other load addresses, `xlnx-versal-virt`, a pinned image and the images with the signing oracle. It also checks statically that the image does not touch the physical timer. The matrix has 76 rows on a Mac with HVF and 65 in CI. You can name single rows as arguments. CI runs it.

The console rows check the console features. The following table lists the console rows and what they check.

| Row | What it checks |
| --- | --- |
| `console-symbols` | A static check that `llvm-nm` on the debug images shows the driver symbols exactly as the features say, and that each image file carries exactly its own `k0: console drivers` line |
| `quiet`, `quiet-16k` | An image without console, judged from memory, with an empty serial log and an unmapped console page |
| `console-pl011-only`, `console-16550-only` | Images with one driver. QEMU virt has a PL011, so the `console-16550-only` image has no driver for it and stays silent |
| `pl011-identity` | The device tree is patched so that the console node points at the RTC, and the kernel must print `k0: console is not a pl011, ignored`, write nothing and not keep that page mapped |
| `serial-check` | The judgement itself: a console image judged as quiet must fail on its serial output |

The oracle rows check the signing oracle. In the following table, `root: oracle tests pass` is required in every boot row, and the boot rows expect 8 fault lines.

| Row | What it checks |
| --- | --- |
| `vault` | An image with `vault-ed25519` on the default CPU. It also requires `k0: dtb seeds wiped 40B` |
| `vault-max` | The same image on the `max` CPU, so that RNDR goes into the boot seed (`... and rndr`) |
| `vault-el2` | The same image started at EL2 |
| `vault-16k` | The 16 KiB image with `vault-ed25519` |
| `vault-no-alg` | An image with `vault` only, which answers every signing request with "unsupported" |
| `vault-symbols` | A static check. The default image has no vault code and no crypto code, and the vault image has no writable static variable from them |

The `uefi-*` rows boot the kernel through the UEFI loader on EDK2: GICv3 and GICv2 at EL1 and EL2, the `max` CPU, 4 cores, the 16 KiB image, a random number source from the firmware and the FAT drive path. The `uefi-acpi-*` rows (`uefi-acpi-gic3-el1`, `uefi-acpi-gic3-el2`, `uefi-acpi-gic2-el1`, `uefi-acpi-smp4`, `uefi-acpi-16k`, `uefi-acpi-rng`) boot QEMU's default machine with ACPI on and no device tree. A static check (`tools/pe-check.py`) covers the loader file's form and its cache and MMU instruction order, which QEMU cannot test.

For configurations that must be refused, it reads the reason from the boot log in memory, or for the loader from the serial log. These are a 16 KiB kernel on `cortex-a72` and a kernel with one changed byte.

On an Apple Silicon Mac, `K0_MATRIX_HVF=1 tools/boot-matrix.sh` adds rows that run the kernel, also through the UEFI loader, on the real CPU cores through QEMU's HVF accelerator. Among them are `hvf-quiet` for the image without console, `hvf-vault` for the image with the oracle and `hvf-term` for the terminal image.

The input and terminal rows check the receive path and the terminal. In the following table, the rows `input-only`, `input-gicv2`, `input-16550` and `input-bad-irq` boot images with `console-input` and no terminal:

| Row | What it checks |
| --- | --- |
| `input-only` | An image with `console-input` and no terminal |
| `input-gicv2` | The same on GICv2 |
| `input-16550` | An image with only the 16550 driver: input is off (`console not attached`) and the log says so |
| `input-bad-irq` | A device tree with an interrupt number out of range costs only the input path (`interrupt number out of range`) |
| `input-symbols` | Static check. An image without `console-input` contains no receive code, no `CONSOLE_READ` and none of the new log lines |
| `term`, `term-max`, `term-el2`, `term-gicv2`, `term-16k` | The terminal image, run through `tools/term-test.py` on the default CPU, on the `max` CPU, at EL2, on GICv2 and with a 16 KiB image |
| `term-no-vault` | A terminal without the vault: `help` lists only `help` |
| `term-no-alg` | A terminal with `vault` but no algorithm |
| `term-uefi`, `term-uefi-acpi` | The terminal image through the UEFI loader (device tree path and ACPI path) |
| `hvf-term` | The terminal image under HVF (Mac only) |

`tools/term-test.py <image>` boots the image with QEMU's serial port on pipes and runs a scripted session. It covers every command and error, erase and cancel, CR LF, an ESC sequence, a line that is too long, key generation, the public key format, zeroize, 200 kB without a line end, 2000 lines in a row and a serial break. `tools/qemu-boot-test.sh` also knows `K0_BOOT_INPUT=on|off` and `K0_BOOT_SERVICE=vault|plain|none` to say what the image should show, and a judge-only mode `K0_BOOT_JUDGE`. A terminal image prints two more `k0: task fault` lines than the same image without the terminal (8 without the vault, 10 with it), so use `K0_BOOT_FAULTS` for it.

### Testing an image without console

An image without console writes nothing to the serial port, so the boot test reads the kernel lines from the boot log in guest memory through the QEMU monitor. The following three settings of `tools/qemu-boot-test.sh` do this.

| Setting | Meaning |
| --- | --- |
| `K0_BOOT_MEMLOG=<address>` | The physical address where the image was loaded, `0x40080000` on QEMU virt. The kernel lines are read from the boot log in guest memory instead of the serial port. Not usable with a UEFI loader |
| `K0_BOOT_SERIAL=empty` | Needs `K0_BOOT_MEMLOG`. Also requires that not one byte came out of the serial port |
| `K0_BOOT_CONSOLE_PAGE=mapped` or `unmapped` | Compares the console address in the `k0: platform` line with the list in `k0: device pages`. That line is printed just before the kernel enters user space and lists every device page the kernel keeps mapped. After it the kernel maps no further device |

You can enter it as follows:

```sh
$ cargo build -p k0-kernel --offline --target-dir target/quiet
$ K0_BOOT_MEMLOG=0x40080000 K0_BOOT_SERIAL=empty K0_BOOT_CONSOLE_PAGE=unmapped \
  tools/qemu-boot-test.sh target/quiet/aarch64-unknown-none-softfloat/debug/k0-kernel
```

What this shows and what it does not:

- An empty serial log shows only that no byte reached the serial port of QEMU virt, a PL011. It says nothing about register reads. Those are covered by the symbol check and by the device page list, which is the kernel's own report.
- The symbol check runs on debug images. Release (LTO) images of the feature sets without console are not built or booted by the matrix, and no image without console is booted through UEFI.
- An image without console reduces the output path, not what the kernel holds in memory. The boot log is plain kernel memory, and anyone who can read RAM or attach a debugger sees it.
- The line `k0: console drivers` can be imitated later by output from the first program, which can print lines that look like kernel lines while it holds the Console capability. Only its first occurrence, or the image file, identifies a build.

> [!NOTE]
> The 16550 driver has never been executed by any test, because QEMU virt has no such UART. Its code was checked by review only. The SBSA UART uses the PL011 driver without an identity check, and the identity check of the PL011 itself runs only for a device tree node that says `arm,pl011` and declares the full 4 KiB frame. Nothing has run on real hardware.

The default QEMU CPU model has no PAC, PAN or RNDR. To test those as well, boot on the `max` model and require the matching log lines. You can enter it as follows:

```sh
$ K0_QEMU_CPU=max K0_BOOT_REQUIRE='rndr=on|k0: pac=on bti=present pan=on' \
  tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel
```

GitHub also runs these tests automatically on every pull request and every push to `main` (CI), on x86-64 and ARM64 Linux, inside sealed build environments (containers) with networking turned off, and runs a longer fuzzing campaign every week. To fuzz the device tree parser yourself or to run exactly what CI runs, see [CONTRIBUTING.md](CONTRIBUTING.md).

## When something goes wrong

The following table lists what to do for each problem.

| Problem | What to do |
| --- | --- |
| `qemu-system-aarch64` is not found | Install QEMU as described under "What you need" |
| `boot-test: kernel ELF not found` | Build first with `cargo build -p k0-kernel --features console --offline` |
| The terminal stays blank after `cargo run -p k0-kernel` without features | The image has no console driver and prints nothing. Build with `--features console`, or use `cargo virt` |
| `runner: this kernel has no console driver and prints nothing here (build with --features console to see output)` | The image was built without a console feature, or the runner found `k0: console drivers [none]` in it. Build with `--features console` to see output, or read the boot log from memory |
| `boot-test: the serial log has no kernel line` | The same cause: a kernel built without a console feature writes nothing to the serial port. Build with `--features console`, or judge the log in guest memory with `K0_BOOT_MEMLOG=<load address>` |
| `boot-test` fails on a line that starts with `k0: all tasks` | Every task is waiting and no wait has a time limit, so nothing can wake a task. In the normal self-tests this does not happen. If you changed the root task, give each wait a timeout |
| `k0: memory <base> + <size> not handed out (<reason>)` appears in the boot log | The kernel found a memory range that is too small for its own reserve, or whose reserve it could not map. It left that range out and kept booting. The range is simply not usable as untyped memory. Check the memory size and the load address you gave QEMU |
| `root: oracle made no key (boot seed has no entropy)` appears, or `root: oracle tests pass` is missing | The boot seed held fewer than 32 `rng-seed` bytes and no RNDR. Use a machine whose device tree has an `rng-seed`, as QEMU virt does, or a CPU with RNDR (`K0_QEMU_CPU=max`) |
| `boot-test` fails on the number of `k0: task fault` lines for an image with a `vault` feature | That image prints 8 of them. Run the test with `K0_BOOT_FAULTS=8` |
| The boot test times out on a slow computer | Give it more time, for example `K0_BOOT_TIMEOUT=300 tools/qemu-boot-test.sh` |
| The 16 KiB image does not boot on `cortex-a72` | Expected: that CPU has no 16 KiB page support. Use `K0_QEMU_CPU=max` |
| A boot stops early and nothing is printed | There may be no supported console, or the image has no driver for it (an image with only one driver is silent on a console of the other kind). Read the boot log from memory with `tools/boot-log.py` |
| `runner: no UEFI firmware found, set K0_QEMU_FIRMWARE` | Install the firmware as described under "What you need", or set `K0_QEMU_FIRMWARE` to the path of an EDK2 `aarch64` code image |
| `build-loader: kernel ELF not found` | Build the kernel first with `cargo build -p k0-kernel --features console --offline` |
| `build-loader: embedding a kernel with: k0: console drivers [none]` | The loader will carry a kernel without console. It still prints its own lines, and the kernel prints nothing after them. Build the kernel with `--features console` and run `tools/build-loader.sh` again if you want output |
| The screen shows only firmware messages, or the firmware menu | The firmware could not start the loader, or the loader refused and the firmware went on. Look for `k0-loader:` lines. With none, check that the `.efi` file exists and, with `K0_QEMU_ESP`, that the drive is found |
| The terminal shows no `k0> ` prompt | Either the image was built without `terminal` (and `console-input`), or the log shows `k0: console input off (...)` or `root: no console input, terminal not started`. Build with `--features console,terminal,vault-ed25519` and read the reason in the boot log |
| Nothing happens when I type, or `Ctrl-A` does not arrive | Under `cargo run` QEMU keeps `Ctrl-A` for itself. A 16550 console has no input at all |
| `boot-test` fails on the number of `k0: task fault` lines for a terminal image | That image prints 2 more than without the terminal (8 or 10). Use `K0_BOOT_FAULTS` |
| A `K0_PIN` made earlier no longer matches | The platform line now contains the console interrupt (`irq33`). Make the pin again |
