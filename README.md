# K0

[한국어](README_KR.md)

K0 is a capability microkernel for AArch64, written in `no_std` Rust. Its resource model follows seL4. The kernel has no heap: it hands all free physical memory to the first user task as untyped capabilities, and every kernel object created after boot lives in memory that the task retyped. Beyond that, the kernel uses only fixed static storage, such as a pool of 32 page tables for its own mappings. The first targets are QEMU virt (4 KiB granule) and Apple Silicon (16 KiB granule). AMD64 and other architectures come after those targets are complete.

K0 is pre-release software at version 0.1.0. The kernel side is about 5,200 lines of Rust and assembly, comments included, so it can be read end to end.

## Security status

Version 0.1.0 provides **no isolation between tasks**. A task created with `TCB_CONFIGURE` and `TCB_RESUME` is a thread of the root task, not a confined process. Do not run untrusted code as a task. [SECURITY.md](SECURITY.md) ([한국어](SECURITY_KR.md)) explains the isolation model, the known advisories, and how to report a vulnerability.

## What works today

- **Boot**: loads as a Linux arm64 Image under QEMU and m1n1. Only the boot core proceeds. Entry at EL2 drops to EL1, and any unsupported entry state parks the core.
- **Memory**: kernel mappings are W^X with `SCTLR_EL1.WXN` set. There is a guard page below the boot stack, a higher-half alias, and boot-time self-checks that use address translation instructions.
- **Untrusted boot input**: the device tree is parsed as hostile input, and a memory map that overlaps a kernel MMIO window stops the boot.
- **Hardening**: when the CPU supports them, PAC keys are derived and loaded and PAN is enabled. Counter, debug and PMU access from EL0 is disabled. The stable compiler does not yet emit pointer authentication instructions, so kernel return addresses are not signed yet.
- **Capabilities**: one root CNode, untyped memory, retype into `Frame`, `PageTable`, `TCB` and `Endpoint`, and `MAP`, where the target address space is named only by a capability.
- **Tasks and scheduling**: `TCB_CONFIGURE`, `TCB_RESUME`, `YIELD`, `EXIT`, and round-robin scheduling with timer preemption.
- **IPC**: synchronous rendezvous `SEND`, `RECV`, `CALL` and `REPLY_RECV`. Messages travel in registers only, and the right to reply is single-use.
- **Fault isolation**: a fault at EL0 terminates that task, not the system.
- **Root task integrity**: the embedded root task image is hashed again with SHA-256 at boot. This is a corruption check, not a signature check. [SECURITY.md](SECURITY.md) explains why under "Scope".

The root task checks every item above with self-tests on every boot.

## Not yet implemented

- Per-task capability spaces and `AddrSpace` retype
- A capability derivation tree with `MINT` (badging, rights reduction) and `revoke`
- Capability transfer over IPC
- Notification objects that deliver interrupts to user space
- Delivery of EL0 faults to a handler task over IPC
- Ed25519 verification once the root task ships separately from the kernel
- Verified boot on Apple Silicon hardware (today the Apple build is only compiled)
- AMD64 and other architectures

## Building and running

```sh
cargo virt    # build for QEMU virt and boot it
cargo apple   # Apple Silicon (m1n1) release build
```

You need `rustup`, `qemu-system-aarch64` with its firmware ROMs, and a host C compiler. Every dependency is vendored, so building needs no network access. [CONTRIBUTING.md](CONTRIBUTING.md) has the full setup.

## Testing

```sh
tools/host-test.sh                  # host unit tests and DTB fuzzing
cargo build -p k0-kernel --offline
tools/qemu-boot-test.sh             # boot under QEMU and check the root task self-tests
```

CI runs the same tests on x86-64 and AArch64 inside containers with networking disabled, and runs a longer fuzzing campaign every week.

## Design notes

The [design notes](design/README.md) explain each subsystem: how it works, why it is built that way, how it is tested, and what is still missing.

## Repository layout

| Path | Contents |
| --- | --- |
| `kernel/` | Kernel binary: boot assembly, initialization order, system call policy, linker scripts |
| `crates/k0-abi` | System call numbers, error codes and the bootinfo layout shared with user space |
| `crates/k0-arch` | Exception vectors, EL0 entry and return, GICv3 and timer, PAC and PAN, early console |
| `crates/k0-boot` | DTB parser, root task image embedding and integrity check, PAC key derivation |
| `crates/k0-cap` | Capabilities, untyped memory, retype |
| `crates/k0-mm` | Kernel page tables, MMU enable, user address spaces |
| `crates/k0-task` | TCBs, endpoints, root task loading |
| `crates/k0-sched` | Round-robin scheduler |
| `crates/k0-ipc` | Synchronous IPC |
| `userspace/root-task` | The first user task and its self-tests |
| `tools/` | QEMU runner, boot test, host test |
| `vendor/` | Vendored third-party crates (`sha2` and its dependencies) |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Before your first contribution is merged, you must sign the [Contributor License Agreement](CLA.md), unless it only fixes typos or broken links in documentation or comments. Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

K0 is source-available under the [PolyForm Noncommercial License 1.0.0](LICENSE). You may use, modify and redistribute it for noncommercial purposes, as long as you pass on the license text and the `Required Notice:` line that carries the copyright notice. Commercial use is not permitted. For a commercial license, contact <qtfelix@qu4nt.space>.

Third-party crates under `vendor/` keep their own licenses (MIT or Apache-2.0).
