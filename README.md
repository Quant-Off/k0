# K0

English | [한국어](README_KR.md)

K0 is a capability microkernel for AArch64, written in `no_std` Rust. Its resource model follows seL4. The kernel has no heap: it hands all free physical memory to the first user task as untyped capabilities, and every kernel object created after boot lives in memory that the task retyped. Beyond that, the kernel uses only fixed static storage: a pool of 32 page tables for its own mappings, a registry with 256 entries for address spaces (entry 0 is unused), and the scheduler state. The first targets are QEMU virt (4 KiB granule) and Apple Silicon (16 KiB granule). AMD64 and other architectures come after those targets are complete.

K0 is pre-release software. The crate version is 0.1.0, and the current `main` branch is unreleased and goes beyond the 0.1.0 tag. The kernel side is about 7,400 lines of Rust and assembly, comments included and tests excluded, so it can still be read end to end.

## Security status

The released v0.1.0 tag provides **no isolation between tasks**. A task created there is a thread of the root task, and its published advisories stand for that tag.

The current `main` branch (unreleased) isolates tasks. A task is bound to a CNode and an address space when it is configured, and tasks bound to different ones are isolated from each other (tasks that share them, like the root task's helper threads, are not). Slot numbers mean something only in the calling task's own CNode, and revoking or deleting a capability removes everything derived from it. What the design provides and the tests exercise: a confined task cannot name an object it holds no capability for, and it reaches only the memory that the holder of its `AddrSpace` capability mapped into its address space. Whoever holds a task's TCB, CNode or AddrSpace capability controls that task. What it does not provide: **availability**. A confined task can still starve or stall the system, for example by blocking last so that every task is blocked, or by exhausting the static kernel page table pool or the address-space registry. Isolation here covers memory and capabilities only. The 16 KiB (Apple) build is compiled and host-tested but has not been booted. Treat running untrusted code on K0 as experimental, and do not do it in a setting where denial of service matters. [SECURITY.md](SECURITY.md) ([한국어](SECURITY_KR.md)) explains the isolation model, the known advisories, and how to report a vulnerability.

## What works today

- **Boot**: loads as a Linux arm64 Image under QEMU and m1n1. Only the boot core proceeds. Entry at EL2 drops to EL1, and any unsupported entry state parks the core.
- **Memory**: kernel mappings are W^X with `SCTLR_EL1.WXN` set. There is a guard page below the boot stack, a higher-half alias, and boot-time self-checks that use address translation instructions.
- **Untrusted boot input**: the device tree is parsed as hostile input, and a memory map that overlaps a kernel MMIO window stops the boot.
- **Hardening**: when the CPU supports them, PAC keys are derived and loaded and PAN is enabled. Counter, debug and PMU access from EL0 is disabled. The stable compiler does not yet emit pointer authentication instructions, so kernel return addresses are not signed yet.
- **Capabilities**: untyped memory, retype into `Frame`, `PageTable`, `TCB`, `Endpoint`, `CNode` and `AddrSpace`, and `MAP` and `UNMAP`, where the target address space is named only by a capability. Capabilities carry rights (read, write, execute), endpoint capabilities can carry a badge, and `CAP_COPY` and `CAP_MOVE` move them between slots.
- **Tasks and scheduling**: `TCB_CONFIGURE` binds a task to a CNode and an address space, then `TCB_RESUME`, `YIELD`, `EXIT`, and round-robin scheduling with timer preemption.
- **IPC**: synchronous rendezvous `SEND`, `RECV`, `CALL` and `REPLY_RECV`. Sending needs the write right and receiving needs the read right on the endpoint capability. Messages travel in registers only, the receiver gets the sender capability's badge, and the right to reply is single-use.
- **Fault isolation**: a fault at EL0, an unknown system call, or `EXIT` terminates that task, not the system.
- **Task isolation and revocation**: every task runs with the CNode and address space it was bound to, and tasks bound to different ones are isolated from each other. Capabilities form a derivation list, and `CAP_REVOKE` and `CAP_DELETE` remove every capability derived from the target, unmap and finalize objects whose original capability goes away, tear down tasks that lose their CNode or address space, and let untyped memory be retyped again, zeroed each time. A frame is never writable in one place and executable in another.
- **Root task integrity**: the embedded root task image is hashed again with SHA-256 at boot. This is a corruption check, not a signature check. [SECURITY.md](SECURITY.md) explains why under "Scope".

The root task checks every item above with self-tests on every boot.

## Not yet implemented

- Capability transfer over IPC
- Notification objects that deliver interrupts to user space
- Delivery of EL0 faults to a handler task over IPC
- Timeouts, and more than one core
- Availability guarantees: the kernel still parks the core when every task is blocked, and a task holding much untyped memory can exhaust the kernel page table pool or the address-space registry
- Loading new executable code after boot: executable frames exist only for the root task image, so a confined task runs code from that image
- Multi-level capability lookup and hardware ASIDs (the capability space is single-level, and a `TTBR0` change flushes the whole TLB)
- Revoking a capability does not cancel a sender that is already queued on the endpoint, which still delivers once
- Ed25519 verification once the root task ships separately from the kernel
- Verified boot on Apple Silicon hardware (today the Apple build is only compiled and host-tested)
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

The host tests include a model test of the capability derivation list over random retype, copy, move, delete and revoke sequences, and tests of the user page table walkers for both granules. The boot self-tests build confined tasks and check that they cannot reach each other's memory or capabilities, that tasks can be deleted in every state, and that revoked untyped memory is retyped again.

CI runs the same tests on x86-64 and AArch64 inside containers with networking disabled, and runs a longer fuzzing campaign every week.

## Design notes

The [design notes](design/README.md) explain each subsystem: how it works, why it is built that way, how it is tested, and what is still missing.

## Repository layout

| Path | Contents |
| --- | --- |
| `kernel/` | Kernel binary: boot assembly, initialization order, system call policy (`syscall.rs`), object finalization on capability removal (`object.rs`), linker scripts |
| `crates/k0-abi` | System call numbers, error codes and the bootinfo layout shared with user space |
| `crates/k0-arch` | Exception vectors, EL0 entry and return, GICv3 and timer, PAC and PAN, early console |
| `crates/k0-boot` | DTB parser, root task image embedding and integrity check, PAC key derivation |
| `crates/k0-cap` | Capability slots and rights, CNodes, derivation list, untyped memory, retype, revoke and delete |
| `crates/k0-mm` | Kernel page tables, MMU enable, user address spaces and the address-space registry |
| `crates/k0-task` | TCBs with their bound CNode and address space, endpoints, root task loading |
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
