# K0 Microkernel

<div align="center">

English | [한국어](README_KR.md)

</div>

K0 is a small operating system kernel for 64-bit ARM (AArch64) computers. It is written in Rust. It is a **microkernel**: the kernel does only what has to be done inside the kernel, above all keeping programs apart. Everything else is meant to run as ordinary programs on top of it. A program can use only the resources it holds a key for. These keys are called capabilities, and the design follows the seL4 kernel.

> [!IMPORTANT]
> One K0 image is meant to boot wherever the hardware and the boot loader meet the contract in [INTRODUCTION.md](INTRODUCTION.md). The contract is the Linux arm64 boot protocol. A UEFI loader (`loader/`) can start the kernel from firmware that provides a device tree or ACPI static tables.
>
> So far it has been booted on several QEMU machines, directly and through UEFI firmware (EDK2, also under UEFI Secure Boot). It has also been booted on real Apple cores as a hypervisor guest. It has not been booted on any real board or on any firmware other than EDK2.
>
> Today K0 is a research platform for the isolation core of high-security systems. It boots and checks its own security properties, but by default it does no useful work of its own yet.

## Security status

- **Published version**: the version published as the `v0.1.0` tag has **no isolation between programs**. The published security advisories (public notices about known flaws) apply to it.
- **Development branch**: the current `main` branch (unreleased) keeps programs apart in memory and in the keys they can use. It does **not** protect availability in general. One program can still hold up the others, for example by never giving up a wait.

> [!WARNING]
> Do not run code you do not trust on K0 anywhere an outage would matter.

[SECURITY.md](SECURITY.md) explains the isolation model, the known advisories and how to report a vulnerability.

## Features at a glance

- **No kernel heap**: the kernel has no memory allocator of its own (heap). Apart from a small fixed amount of memory, it uses only memory that a program hands to it.
- **Keys instead of trust**: every resource is reached through a key. A copy of a key never has more rights than the original, and taking a key back removes every copy made from it.
- **Separated programs**: programs can be given their own key ring and their own memory map, and then they cannot reach each other's memory or keys. When a program crashes, only that program is stopped.
- **Wiped before reuse**: memory is filled with zeros every time it is handed out again.
- **Write or run, never both**: no memory is writable and executable at the same time. The flip side is that no new code can become executable after boot, so programs cannot load new code.
- **Distrust of boot input**: the hardware description handed over at boot is checked as if an attacker wrote it.
- **Optional signing oracle**: a build option adds a task that signs messages for other tasks and never gives out its key. It is off by default, the Ed25519 implementation is unvalidated, and it keeps one key in memory only. See [INTRODUCTION.md](INTRODUCTION.md).

The first program checks these properties with self-tests on every boot.

## Quick start

```sh
# Clone the repository and enter it
$ git clone https://github.com/Quant-Off/k0.git
$ cd k0

# Build K0 and boot it in QEMU (quit with Ctrl-A, then X)
$ cargo virt
```

[USAGE.md](USAGE.md) explains what you need, how to read the output and how to run the automatic tests.

## Documents

In the following table, you can find the documents in this repository. The design notes are under `design/`.

| Document | Contents |
| --- | --- |
| [INTRODUCTION.md](INTRODUCTION.md) | What K0 is, what works today, what is still missing, and where the code lives |
| [USAGE.md](USAGE.md) | Setting up, building, running and testing |
| [SECURITY.md](SECURITY.md) | Isolation model, known advisories, reporting a vulnerability |
| [Design notes](design/en/README.md) | How each part of the kernel works and why it was built that way |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to contribute |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Before your first contribution is merged, you must sign the [Contributor License Agreement](CLA.md), unless it only fixes typos or broken links in documentation or comments. Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

K0 is source-available software under the [PolyForm Noncommercial License 1.0.0](LICENSE). **Noncommercial use**, **modification** and **redistribution** are permitted, as long as you pass on the **license text** and the `Required Notice:` line (the copyright notice). **Commercial use is not permitted**. If you need a commercial license, please contact <qtfelix@qu4nt.space>.

> [!NOTE]
> Third-party crates under the `vendor/` directory keep their own licenses (MIT or Apache-2.0).
