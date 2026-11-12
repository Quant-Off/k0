# Security Policy

<div align="center">

English | [한국어](SECURITY_KR.md)

</div>

K0 is a pre-release research microkernel. The released version has no task isolation, the unreleased `main` branch has it but with the limits described below, and those gaps are security relevant. Read this document before running any code on K0 that you do not fully trust.

## Supported versions

| Version | Status | Notes |
| --- | --- | --- |
| 0.1.0 (tag `v0.1.0`) and every earlier commit on `main` | Pre-release | No task isolation. See "Isolation model" below. |
| Current `main` branch | Unreleased | Task isolation with known limits. See "Isolation model" below. |

There is no fixed version yet. Every commit up to and including the `v0.1.0` tag is affected by the advisories listed in "Known advisories". The version number follows SemVer: while the major version is 0, any minor release may break the system call ABI, and the first release that ships per-task capability spaces will be `0.2.0`.

## Isolation model

This section describes two different states of the code. Do not mix them up.

### Released tag `v0.1.0` and every earlier commit

These provide **no isolation between tasks**. This is a known design gap, not an incidental bug, and it holds for every task those versions can run.

- There is exactly one capability space, the static root CNode. Every system call resolves its slot arguments in that CNode regardless of which task made the call.
- There is exactly one address space, the root task's TTBR0 root. `TCB_CONFIGURE` can only bind a TCB to that address space.
- There is no capability derivation tree, no `MINT`, no `revoke`, and no capability transfer over IPC.

A task created with `TCB_CONFIGURE` and `TCB_RESUME` in those versions is a **thread of the root task**, not a confined process. It holds every capability the root task holds, it can read and write all of the root task's memory, it can retype any `Untyped` capability, it can configure and resume every other TCB, and it can send to or receive from every `Endpoint`. Treat every task as fully trusted there. Do not run untrusted or third-party code as a K0 task. There is no workaround at the kernel level.

### The current `main` branch (unreleased)

The current `main` branch contains task isolation. No release or tag contains it yet, and its system call ABI is incompatible with `v0.1.0` (bootinfo layout version 2).

What a confined task is and can do:

- Every task is bound to a capability space, a CNode object, when it is configured. Slot numbers are resolved only in the calling task's own CNode, so a slot number of one task means nothing in another.
- Every task runs in the address space named by an `AddrSpace` capability. A task reaches exactly the memory that the holder of its `AddrSpace` capability mapped into that address space.
- Both bindings are held as capabilities inside the task's TCB. If either capability is revoked or deleted, the task is torn down.
- Isolation holds between tasks bound to different CNodes and address spaces. Tasks that share them, as the root task's helper threads do, are threads of one another. Whoever holds a task's TCB, CNode or AddrSpace capability controls that task.
- Capabilities carry rights (R, W, X). Rights can only be removed by `CAP_COPY`, never added. `Endpoint` capabilities can carry a badge, set once by `CAP_COPY`, which the receiver sees. Retyped frames never gain X, and X exists only on the root task's text frames, so no physical frame is writable in one place and executable in another.
- Capabilities form a derivation list. `CAP_REVOKE` removes everything derived from a capability, `CAP_DELETE` also removes the slot, an object is finalized when its original capability is removed, and the underlying `Untyped` memory can then be retyped again. Retyped objects are zeroed before a capability to them exists.
- A fault, an exit, or an unknown system call in a confined task terminates only that task.

What is still NOT provided on `main`:

- **Availability.** Isolation does not cover it. If every task is blocked the kernel still parks the core (GHSA-q99x-2wrv-358p), and a confined task can cause this by being the last task to block.
- **Kernel resource exhaustion.** A task holding a lot of `Untyped` memory can exhaust the static pool of 32 kernel page tables used for alias mappings, after which `RETYPE` fails with `KERNEL_RESOURCE` for every task, or exhaust the static address space registry.
- No fault delivery to a handler task, no notifications, no timeouts, and no capability transfer over IPC.
- A sender already queued on an endpoint still delivers once after its capability is revoked.
- One core only.
- New executable code cannot be loaded after boot, so a confined task runs code from the root task image.
- Only the evidence tiers in [INTRODUCTION.md](INTRODUCTION.md) count as supported. Real boards, KVM and other hypervisors, the 16550 console driver, and processors with `HCR_EL2.E2H` fixed to 1 have never been booted.
- The device tree is not trusted, but it now supplies device addresses. A wrong or hostile device tree can point the console at another device. The kernel limits this: it checks the platform descriptor against the memory map, checks the identification registers of the GIC and the PL011 (no such register exists for a 16550, so that driver has no such check), maps a device page only after validation, bounds every console wait, and offers a pinned build (`K0_PIN`) that refuses a different platform. The pinned build compares values that come from the device tree, so it does not stop a hostile boot chain.
- The address of the device tree cannot be checked before the first read. The boot loader passes it in `x0`, and the kernel has no memory map until it has read that blob. The kernel refuses a null or misaligned address, an address at or above 2^47 and a range that overlaps the kernel image. It then maps the address as read-only normal memory and reads eight bytes. It reads further, up to 2 MiB, only if those bytes look like a device tree header. A boot loader that passes the address of a device therefore makes the kernel read device registers as if they were memory, and reading some registers has side effects. The mapping is cacheable, so the processor may fetch more of that page than the eight bytes the kernel asks for. The later check that the device tree lies inside declared memory uses the memory map from the same blob. The kernel never writes through this mapping.
- Kernel output and task output share one console and one boot log, and a line does not say who wrote it. A task that holds a `Console` capability can print a line that starts with `k0:`. The kernel replaces control bytes and limits how much of the boot log task output may use, but nothing that reads the console or the log, including `tools/qemu-boot-test.sh`, can treat a `k0:` line printed after `k0: entering root task (el0)` as written by the kernel.
- Isolation covers confidentiality and integrity, not availability. A trap to EL2 after the kernel has stepped down to EL1 parks the whole machine by design.
- Entry at EL2 relies on the firmware contract that Linux also uses: an EL3 firmware must enable the fine-grained-trap and HCRX controls when those features exist.
- The root task is still trusted and holds the initial `Untyped` capabilities, and `write_bootinfo` still exposes their physical addresses to it.

Even on `main`, treat running untrusted code as experimental. The isolation is exercised by the host tests and the boot self-tests, and the availability limits above still apply. Isolation bypasses on `main` are welcome reports.

## The UEFI loader

`loader/` is a UEFI application that embeds a built kernel and starts it with the usual boot contract, and `crates/k0-efi` holds its logic. The kernel does not change. What the loader does and why it exists are in [design/en/uefi.md](design/en/uefi.md). The security-relevant facts:

- The loader decides what the kernel will treat as RAM. Only free memory and loader memory count. Everything else never does: boot services memory, runtime memory, unknown types, overlapping descriptors, reservations and devices found in the firmware device tree. It drops the firmware tree's own memory description, because on QEMU with EDK2 that tree declares all 512 MiB as RAM although firmware regions lie inside, and an unmodified kernel would hand about 5.5 MiB of firmware memory to user space. It then checks its own result with a second classifier and with the kernel's own parser and validation before it jumps.
- A firmware device tree that the kernel's parser would refuse, and firmware that gives neither a device tree nor readable ACPI static tables, are refused by the loader before boot services end. It prints `k0-loader: refused: <reason>`, wipes what it allocated and returns an error to the firmware.
- When the firmware gives no device tree, the loader reads the ACPI static tables RSDP, XSDT, MADT, GTDT and SPCR and builds a minimal device tree from them. It never reads or runs AML. It reads a table only when the whole table lies inside one memory map descriptor that is memory, and it checks lengths and checksums. A device named by the tables (GIC frames, console) must not lie in RAM or in firmware memory, otherwise the loader refuses. A console it cannot use means a boot without console, not a refusal. The finished tree then takes the same checks as a firmware tree.
- The SHA-256 of the embedded kernel is a corruption check, not a signature. The loader has no signature code. Authenticity comes only from Secure Boot in the firmware, which verifies the whole loader file with the kernel inside it, using keys the owner enrols. This was tried once with test keys (see "Where it has run" in [INTRODUCTION.md](INTRODUCTION.md)). The Microsoft third-party route (shim) is not prepared.

Limits and residual risks of the loader:

1. The firmware runs before the loader at the same or a higher privilege. The loader guards against firmware that is wrong, not firmware that is hostile.
2. Only this loader may start the kernel under UEFI. A generic UEFI boot loader that passes the firmware device tree through hands firmware memory to user space.
3. The embedded hash does not prove who built the kernel (see above).
4. A refusal returns to the firmware, which tries its next boot option. A machine that must boot K0 or nothing needs a loader built with the feature `refuse-halt`, which parks instead. After the first attempt to exit boot services, every failure parks.
5. QEMU cannot test the cache flush and MMU-off sequence: TCG has no caches, and HVF did not expose a missing flush either. It rests on the architecture rules, on the sequence Linux used when it entered the kernel with the MMU off, and on `tools/pe-check.py`, which checks that the instructions and both flush calls are present, not that they are right on real hardware.
6. A pinned build (`K0_PIN`) under UEFI is untested. The pin line includes the RAM range, which under UEFI depends on the firmware build and the RAM size. A mismatch stops the kernel with nothing on the screen. The reason is in the kernel's boot log in memory, and the loader prints the address needed to read it (`k0-loader: image at 0x..., dtb at 0x...`).
7. Boot services memory is never used, but nothing stops a device that firmware left running from writing to other memory after the firmware services end. K0 has no IOMMU support.
8. The entropy the loader adds stays readable in the device tree in memory for the whole run. Without `EFI_RNG_PROTOCOL` the kernel gets only what the firmware device tree carried, and nothing at all when the tree was built from ACPI tables (the kernel then relies on RNDR if the CPU has it).
9. The kernel never calls UEFI runtime services: no variable access, no reset or power-off through firmware. Runtime regions stay unused.
10. Firmware that describes device registers as `EfiReservedMemoryType` (instead of leaving them out of the map or typing them as MMIO) makes the loader refuse, and one malformed descriptor makes it refuse the whole map. Both fail closed.
11. The paths that move the block to another place and that free memory after a refusal could not be exercised under QEMU.
12. The loader file is not reproducible bit for bit across build directories. The embedded kernel hash that the loader prints is.
13. The only ACPI machine ever booted is QEMU virt. Real ACPI machines, firmware other than EDK2 ArmVirtQemu, GICv4 and redistributors described by CPU entries are untested.
14. The SBSA UART and the 16550 have no identification check, and a 16550 with wider registers gives no console.
15. GICv2 frame sizes are not in the ACPI tables. The loader assumes 4 KiB and 8 KiB, as Linux does.
16. The check that a device named by a table does not lie in memory exists only in the loader. The kernel receives no firmware-owned ranges, so it cannot repeat it.
17. A table kept in memory of the reserved type is not read.

## Known advisories

The following advisories are tracked as GitHub Security Advisories in this repository. The first two were published on 2026-11-07. GHSA-hj3f-38h9-hrpj is still an unpublished draft, so its identifier does not resolve publicly yet, and this table is its public record until it is published. All of them affect version 0.1.0 and earlier, and none has a fixed version yet.

| Advisory | Severity | Summary |
| --- | --- | --- |
| [GHSA-vmpv-qc49-3p6r](https://github.com/Quant-Off/k0/security/advisories/GHSA-vmpv-qc49-3p6r) | High | All tasks share the root CNode and address space, so any spawned task holds full root authority. |
| [GHSA-q99x-2wrv-358p](https://github.com/Quant-Off/k0/security/advisories/GHSA-q99x-2wrv-358p) | Medium | A task that blocks on IPC while every other task is blocked halts the whole system. |
| GHSA-hj3f-38h9-hrpj | Low | DTB memory regions are not bounded by the physical address width. |

The current `main` branch contains changes that address the subject of some of these advisories. No release contains them yet, and advisory status is updated only at release.

## Reporting a vulnerability

Please do not open a public issue for a security problem.

- Preferred: open a draft security advisory at <https://github.com/Quant-Off/k0/security/advisories/new>.
- Alternative: email the maintainer at <qtfelix@qu4nt.space>. Include the affected commit hash, the platform (QEMU machine and CPU model, hypervisor, or board), the entry level (EL1 or EL2), the page size (4 KiB or 16 KiB), the `k0: platform ...` line from the boot log, and a reproduction if you have one.

Isolation gaps in `v0.1.0` and earlier commits, and the availability and resource limits listed under "What is still NOT provided on `main`", are already known and do not need to be filed again. Isolation bypasses on the current `main` branch are welcome, for example a task reaching another task's memory or capabilities, a revoked capability that keeps working, or a stale mapping that survives unmap, revoke or address space destruction. Reports about the UEFI loader are welcome too, for example a memory map or device tree that makes it hand the kernel memory that firmware still uses, or a refusal that does not happen. Reports about anything else, including boot-time memory map validation, exception handling, scheduler policy, and IPC state machines, are welcome.

## Scope

In scope: everything under `kernel/`, `crates/` (including `crates/k0-efi`), `loader/` and `userspace/root-task/`. The UEFI loader is part of the boot chain, but it is code of this repository, so its handling of the memory map, the device tree and the kernel image is in scope.

Out of scope: the rest of the boot chain before the kernel entry point (for example firmware, any other boot loader and the hypervisor), and the integrity of the kernel image itself. Kernel image signing is the boot chain's responsibility. Under UEFI that means Secure Boot in the firmware, because the loader's SHA-256 check is not a signature (see "The UEFI loader").

The kernel checks the embedded root task image against a SHA-256 hash at every boot. This is a corruption check, not a signature check. The reference hash lives in the same kernel image as the root task, so anyone who can modify the kernel image can change both. Reports that rely on modifying the kernel image, or the kernel embedded in the loader, are therefore out of scope.
