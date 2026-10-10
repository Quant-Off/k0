# Contributing to K0

[한국어](CONTRIBUTING_KR.md)

Thank you for your interest in K0. K0 is a pre-release research microkernel. Before you change anything, read the isolation model in [SECURITY.md](SECURITY.md). It explains which guarantees the kernel does and does not provide today.

## Before you start

- **Security problems** never go into public issues or pull requests. Follow [SECURITY.md](SECURITY.md).
- **Small fixes** such as typos, test gaps or clear bugs can go straight to a pull request.
- **Anything larger** should start as an issue, so that the design can be agreed on before you write code. This applies to new system calls, new kernel objects, changes to the capability model and anything that touches `unsafe` code or assembly.
- **Read the design note** for the subsystem you want to change in [design/](design/README.md). Each note ends with the known gaps, which are good places to start.

## License and contributor agreement

K0 is licensed under the [PolyForm Noncommercial License 1.0.0](LICENSE). Quant Space may also offer K0 under separate commercial terms.

Before your first pull request can be merged, you must sign the [Contributor License Agreement](CLA.md). To sign it, comment on your pull request with this sentence:

```
I have read the K0 CLA (CLA.md) and I agree to its terms for this and all my future contributions to K0.
```

The agreement is currently a draft pending legal review. If it changes, you will be asked to sign the new version.

## Setting up

K0 builds fully offline. Every third-party crate is vendored under `vendor/`, and `.cargo/config.toml` forbids fetching anything else.

You need:

- `rustup`. The pinned toolchain in `rust-toolchain.toml` (Rust 1.89.0 with `rust-src`, `llvm-tools` and the `aarch64-unknown-none-softfloat` target) is installed automatically on first use.
- `qemu-system-aarch64` with its firmware ROMs. On Debian and Ubuntu this means the `qemu-system-arm` and `ipxe-qemu` packages. On macOS, `brew install qemu`.
- A host C compiler and linker for build scripts (`gcc` or Xcode command line tools).

The file [.oss-scanner/Dockerfile](.oss-scanner/Dockerfile) is a complete, tested recipe for this environment.

## Building

```sh
cargo virt                                   # build for QEMU virt and boot it
cargo build -p k0-kernel --offline           # build the QEMU virt image only
cargo apple                                  # Apple Silicon (m1n1) release build
```

The kernel never exits. Once the root task finishes its self-tests, it yields forever and the kernel prints `k0: tick N` once a second. Stop QEMU with `Ctrl-A X`.

## Testing

Run all three before you open a pull request.

```sh
tools/host-test.sh                           # unit tests and DTB fuzzing on the host
cargo build -p k0-kernel --offline
tools/qemu-boot-test.sh                      # boot under QEMU and check the self-tests
```

`tools/host-test.sh` runs the tests of `k0-abi`, `k0-cap` and `k0-boot` on any host. The tests of `k0-mm` contain AArch64 inline assembly, so they only run on an AArch64 host (Apple Silicon or Linux arm64). The script runs them for both the 4 KiB and the 16 KiB granule.

`tools/qemu-boot-test.sh` boots the debug kernel, checks the boot log line by line in a fixed order, fails on any error marker, and stops QEMU as soon as the root task reports `root: sched tests pass`. Set `K0_BOOT_TIMEOUT` (seconds) on slow machines.

The default QEMU CPU model (`cortex-a72`) has no PAC, PAN or RNDR. To exercise those paths as well, boot on QEMU's `max` model and require the matching log lines. Any arguments after the kernel path are passed to QEMU.

```sh
K0_BOOT_REQUIRE='rndr=on|k0: pac=on bti=present pan=on' \
  tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel -cpu max
```

### Where each part is tested

| Area | Host unit tests | Boot self-tests in the root task |
| --- | --- | --- |
| `k0-abi` constants and shared layouts | yes | indirectly |
| `k0-cap` untyped bootstrap and retype | yes | yes |
| `k0-boot` DTB parser, PAC key derivation, root task image | yes, plus fuzzing | yes |
| `k0-mm` page tables and W^X attributes | yes, on AArch64 hosts | yes |
| `k0-arch`, `k0-task`, `k0-sched`, `k0-ipc` | no | yes |

`k0-arch` assembles with ELF-only directives and the scheduler and IPC paths read system registers, so they are only exercised by booting. If you change them, extend the root task self-tests in `userspace/root-task/src/main.rs` and add the new pass line to the expected list in `tools/qemu-boot-test.sh`.

### Fuzzing the DTB parser

The DTB parser treats the device tree as untrusted input. Its fuzzer is a deterministic mutation fuzzer that needs no extra crates. It generates random device trees, checks that the parser returns exactly what the generator described, then mutates the blob and checks that the parser never panics and that every accepted result stays within its limits. It does not use coverage feedback.

```sh
host="$(rustc -vV | sed -n 's/^host: //p')"
K0_FUZZ_ITERS=5000000 cargo test --release --offline --target "$host" -p k0-boot fuzz -- --nocapture
```

`K0_FUZZ_SEED` changes the base seed. When a case fails, the test prints a `K0_FUZZ_CASE=0x...` value. Run the same command with that variable set to replay only that case and dump the input.

### Running exactly what CI runs

CI builds the scanner image and then runs the tests inside it with networking disabled. This proves that building and testing need nothing outside the repository.

```sh
docker build --file .oss-scanner/Dockerfile --tag k0-ci .
docker run --rm --network none k0-ci tools/host-test.sh
docker run --rm --network none k0-ci tools/qemu-boot-test.sh
```

Run this from a clean checkout, because the build context includes everything in the working directory.

## Rules for kernel changes

- **No new dependencies** in kernel or root task crates. If a dependency is truly unavoidable, discuss it in an issue first. It must then be vendored and pinned.
- **Every `unsafe` block** needs a `// SAFETY:` comment that says why the required invariant holds at that point. "This is safe" is not a reason.
- **Fail secure.** A failure during boot parks the core. A failing system call returns an error and leaves no partial state behind.
- **Treat every input from outside the kernel as hostile.** This includes the DTB and every system call argument. Use checked arithmetic and validate before you dereference.
- **Keep W^X.** No mapping is ever both writable and executable, at either exception level.
- **Add a test** with every behavior change. Pure logic gets a host unit test. System call behavior gets a root task self-test.
- **Code comments** in this repository are written in Korean. You may write yours in English. A maintainer may translate them when merging.

## Commits and pull requests

- One logical change per commit. A reviewer should be able to read each commit on its own.
- Keep the subject line short and plain. Korean or English are both fine. Do not use type prefixes such as `feat:` or `fix:`.
- In the pull request, say what changed, why, and how you tested it. Link the issue if there is one.
- CI must pass. It builds both platform images, runs the host tests on x86-64 and AArch64, and boots the kernel under QEMU on two CPU models.

## AI-assisted contributions

Parts of K0 were written with AI assistance, and some commits in the history name an AI model as co-author. AI tools are allowed for contributions too, under these conditions:

- You understand every line you submit and can explain it in review. This applies above all to `unsafe` code, assembly and anything on a trust boundary.
- You say in the pull request description which parts were produced with AI assistance.
- You have the right to submit the output, as the [CLA](CLA.md) requires.

Every change is reviewed by a maintainer before it is merged, however it was produced.

## Code of conduct

Everyone who takes part in K0 is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
