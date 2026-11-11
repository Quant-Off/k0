# The device tree as untrusted input

<div align="center">

English | [한국어](../kr/dtb_KR.md)

</div>

The bootloader passes K0 a flattened device tree (DTB) that describes the machine: how much RAM exists and where, which ranges the firmware has reserved, some boot entropy, and now also where the console, the interrupt controller and the timer interrupt are. K0 treats this blob as hostile input. This note explains why, what the parsers read, and how they are tested. The memory parser is `crates/k0-boot/src/fdt.rs`, the shared structure walker is `crates/k0-boot/src/fdt/walk.rs`, and device discovery is `crates/k0-boot/src/platform.rs`.

## Why the DTB is not trusted

On many platforms the DTB does not come from the same place as the kernel image. It may be a separate file next to a signed kernel, an overlay applied by firmware, or a blob that a bootloader edits before it jumps. A genuine kernel can therefore receive a wrong or hostile DTB. K0 assumes it might.

The parsers follow one principle. Values from the DTB may make the kernel refuse to boot or give it fewer resources, but they must never grant more authority than the hardware really offers.

- **Reserved ranges only remove memory.** A hostile reservation can at worst shrink the memory handed to user space.
- **Entropy is only one input among several.** The bytes from `/chosen` are mixed with other sources through SHA-256, so hostile entropy cannot weaken the PAC keys. A missing seed does lower their quality, and the kernel says so in the boot log (see [hardening.md](hardening.md)).
- **Memory nodes grant memory, so they are checked hardest.** Every memory region becomes untyped memory that user space can map. A region that claimed to be RAM but covered a device would hand EL0 direct access to that device. After parsing, the kernel compares every memory region with the device ranges it found in the same DTB (see below) and stops the boot on any overlap. It also stops the boot if a RAM region reaches 2^47 or a device range extends beyond the CPU's physical address width (`k0_mm::pa_bits`), because such a region could not be mapped. The CNode bootstrap also rejects memory regions that overlap each other, so no frame can be retyped twice.
- **Device addresses come from the DTB, so they are checked the hardest.** A wrong DTB can point the console at another device. The kernel does not map a device page until the descriptor has passed the memory map check, it verifies the hardware identification registers where they exist, its console waits are bounded in time, and a pinned build can fix the expected platform (see "The platform descriptor").

## Two stages

The blob has to be mapped before the kernel can read it with the MMU on, and the full parse needs relocated data that only works after the jump to the higher half (see [boot.md](boot.md)). `kernel_init` therefore does not touch the DTB at all, and all parsing happens in `kernel_main`.

**`dtb_span`, before the parse.** `map_dtb` in `kernel/src/main.rs` first rejects a null or non-8-byte-aligned pointer and maps one header's worth of the DTB read-only through the kernel alias. `dtb_span` reads only the first eight bytes. The magic must be `0xd00dfeed`. The total size must be between 40 bytes (one header) and 2 MiB. The upper bound limits how many page tables the DTB mapping can consume. The kernel then maps the whole blob read-only.

**`parse`, `discover`, after the mapping.** Both read through the kernel alias but report physical addresses. `parse` runs `dtb_span` again at the alias address and rejects the blob if its physical range overlaps the kernel image. Then it builds a byte slice of exactly the total size and hands it to `parse_blob`. From that point on, every access is a bounds-checked slice access in safe Rust, and every offset computation uses checked arithmetic. A malformed blob can make the parser return an error, but it cannot make it read outside the slice. `discover` reads the same blob the same way (see "The platform descriptor").

## What the memory parser reads

The header must announce version 17 or later and be backward compatible with version 17. The structure block must start on a 4-byte boundary and have a size that is a multiple of 4. The structure block and the strings block must both end inside the blob. The walker also rejects a property that appears twice in a node when the kernel interprets that property.

`parse` reads exactly these items and skips everything else:

| Source | Item |
| --- | --- |
| memory reservation block | `(address, size)` pairs up to the terminating `(0, 0)` pair. Pairs with size 0 are skipped. The block must be 8-byte aligned. |
| root node | `#address-cells` and `#size-cells`. The defaults are 2 and 1. Only 1 and 2 are supported. |
| top-level `memory` or `memory@...` nodes | `reg`, decoded with the root cell sizes. The node must also have `device_type = "memory"` and an enabled `status`. A `linux,usable-memory` property makes the kernel refuse to boot. |
| children of `/reserved-memory` | `reg`, decoded with the cell sizes of `/reserved-memory` itself. A non-empty `ranges` on `/reserved-memory` is refused. |
| `/chosen` | `rng-seed` and `kaslr-seed`, concatenated and capped at 72 bytes. The initrd range, if both `linux,initrd-start` and `linux,initrd-end` exist, is added to the reserved ranges (the kernel does not use an initrd). A `linux,usable-memory-range` property makes the kernel refuse to boot. |

Nodes with these names at other depths are ignored. A node called `memory` inside `/soc` is not RAM. Reserved-memory children that give only a size and no `reg` are dynamic allocations that the firmware has not pinned to an address, so they are ignored too.

Fixed limits keep the parser's own state small and static:

| Limit | Value |
| --- | --- |
| total size | 2 MiB |
| nesting depth | 32 |
| memory regions | 8 |
| reserved regions, all sources together | 32 |
| entropy bytes | 72 |

## What counts as an error

Every violation returns a `BootError`, and the kernel then prints `k0: dtb rejected: ...` and parks. The errors include:

- a bad magic, alignment, size, version or header offset,
- an unknown token, an end-node token at depth 0, a property outside any node, a root node with a name, a node name without a terminating zero byte, or a structure block that ends before the end token,
- a property name offset outside the strings block, or a name without a terminating zero byte,
- a property value that runs past the structure block,
- unsupported cell sizes, or a cell-size property that is not exactly 4 bytes,
- a `reg` value that is empty or not a whole number of entries,
- any region whose base plus size overflows 64 bits,
- more regions than the limits allow,
- no memory at all,
- a memory-limiting property (`linux,usable-memory`, `linux,usable-memory-range`), a non-empty `ranges` on `/reserved-memory`, a half-present or inverted initrd range, or a repeated property that the kernel reads.

## The platform descriptor

Besides memory, the kernel finds exactly three devices in the DTB and nothing else (`discover` in `crates/k0-boot/src/platform.rs`):

- **The console.** Only the node named by `/chosen/stdout-path` (an absolute path, or an alias that resolves to one). The kernel never guesses "the first UART". The node must be enabled, and its `compatible` must name a PL011 (`arm,pl011`), an SBSA UART (`arm,sbsa-uart`) or a 16550-family UART (`ns16550a`, `ns16550`, `snps,dw-apb-uart`, `brcm,bcm2835-aux-uart`). Any other UART, a missing or unresolvable `stdout-path`, or a node the kernel cannot translate to a CPU physical address gives a boot with no console. That is not an error. The kernel prints `k0: no console (...)` with the reason, and the boot log buffer is the only record (see [boot.md](boot.md)). The register range the driver touches must lie inside the declared `reg` and inside one granule.
- **The interrupt controller.** The node that the timer node's interrupt parent points to (`arm,armv8-timer` or `arm,armv7-timer`, exactly one enabled node). It must be a GICv3 (`arm,gic-v3`) or a GICv2 (`arm,gic-400`, `arm,cortex-a15-gic`, `arm,cortex-a7-gic`) with the expected cell counts and frame alignment. Up to four GICv3 redistributor ranges are accepted. A duplicate phandle, a disabled node, an `interrupt-map`, or a GICv2 without per-CPU banked interfaces (`cpu-offset`) is refused.
- **The EL1 virtual timer interrupt number.** Taken from the timer node's `interrupts` (the `virt` entry when `interrupt-names` exists, else the third entry) as a PPI number plus 16. A timer node that declares a timer erratum (`fsl,erratum-a008585`, `hisilicon,erratum-161010101` or `allwinner,erratum-unknown1`) is refused, because the kernel has no workaround code.

Register addresses are translated through the `ranges` of the parent buses to CPU physical addresses. A bus without `ranges` cannot be translated.

The result is one immutable `Platform` value. The kernel prints it as the single line `k0: platform con=... gic=... tmr=...`, in a fixed format. Before any device page is mapped, `Platform::validate` checks it against the memory map: RAM must lie below 2^47, the kernel image and the DTB must lie inside declared memory, no device range (widened to whole granules) may overlap memory or a reserved range, the device ranges must not overlap each other, every device must lie below the CPU's physical address width, and the console registers must not cross a granule. Then the hardware has to agree with the DTB. The GIC driver checks the GIC identification registers, and the PL011 identification registers are checked before the first write when the DTB declares the whole 4 KiB register frame (a shorter declared range, an SBSA UART and a 16550 are not checked: the first two because the registers are not guaranteed to be there, the 16550 because it has none, and the 16550 driver has never run on any machine). A mismatch makes the kernel refuse or, for the console, continue without one.

### Pinned mode

The platform line, the entry exception level and the RAM range form the string `<platform line> el=<n> ram=<lo>-<hi>`. If `K0_PIN` was set at build time to the text after `k0: platform ` from a boot log of the target, the image refuses to boot when this string differs. It catches a wrong board or a swapped DTB. It does not defend against a hostile boot chain, because the compared values are derived from the DTB.

## How it is tested

The parser is pure logic over a byte slice, so it is tested on the host.

- **Unit tests** (`crates/k0-boot/src/fdt/tests.rs`, and `crates/k0-boot/src/platform/tests.rs` for device discovery) cover each rule above, including the header checks run in place through a raw pointer, the overlap check against the kernel image, and the read-through-an-alias path.
- **A model.** A test-only builder (`fdt/builder.rs`) writes DTBs from a small description of a board and computes what the parser should return. Many unit tests compare the memory parser's output with this model, not with hand-written values.
- **A deterministic mutation fuzzer** (`fdt/fuzz.rs`). Each case generates a random board, wraps it in random unrelated nodes, properties and `NOP` tokens, and checks the parse against the model. Then it applies up to four mutations, such as bit flips, header field rewrites, truncation, splicing and token insertion, and parses the result twice: once as a slice and once through the raw-pointer entry point backed by a 2 MiB zero-filled buffer. Any panic fails the test. Any accepted result must respect every limit in the table above. The fuzzer needs no extra crates and runs fully offline, but it has no coverage feedback.
- **A second mutation fuzzer for discovery** (`fuzz_discover_with_mutated_trees` in `platform/tests.rs`). It starts from four trees (QEMU virt with a GICv3 and with a GICv2, and trees shaped like a Raspberry Pi 4 and an RK3399 board), applies the same mutations and runs `discover` on the result. Any panic fails the test. Every accepted descriptor must keep its invariants: no empty or overflowing range, a timer interrupt number from 16 to 31, console registers inside the declared range, and a platform line that formats.

The default test run executes 20,000 cases of each fuzzer. CI runs five million cases of each every week and takes the base seed from the run number. The base seed is hashed before use, so two different base seeds give unrelated streams of cases. On 2026-11-11, after the walker was rewritten and discovery was added, eight campaigns of five million cases each, with eight base seeds, found no failure in either fuzzer. No campaign has found a parser failure so far. The one failure a campaign reported was in the test model itself, which had not known that a `(0, 0)` pair ends the memory reservation block. When the parser became stricter, the generator was changed with it, so it no longer writes trees that the parser now refuses.

## Known gaps

- The memory parser itself does not know the CPU's physical address width. The check against it is in `Platform::validate`, with the limit passed in from `kernel/src/main.rs`. See advisory GHSA-hj3f-38h9-hrpj in [SECURITY.md](../../SECURITY.md) for the status of the earlier memory-map finding. A DTB controlled by an attacker can still stop the boot, which is the intended fail-secure result.
- The DTB pointer is used before anything can validate it. `map_dtb` in `kernel/src/main.rs` refuses a null or misaligned pointer, an address at or above 2^47 and a range that overlaps the kernel image, and nothing else, because the memory map comes from the blob itself. If a boot loader passes the address of a device, the kernel maps that page as read-only normal memory and reads the first eight bytes. Reading a device register can have side effects, and because the mapping is cacheable the processor may fetch more of the page than those eight bytes. The kernel reads further only if the bytes hold the DTB magic and a valid size, and it never writes through the mapping. `Platform::validate` later checks that the DTB lies inside declared memory, but that memory map is the DTB's own claim. A pinned build could refuse a pointer outside its pinned RAM range before the first read. That check does not exist yet.
- The overlap checks know only the devices the kernel itself found. A DTB that describes some other device range as RAM is not caught, because the kernel has no independent list of devices.
- A wrong DTB can still point the console at a different device that passes the checks. The 16550 family has no identification register, so nothing confirms that the hardware is a 16550. Pinned mode narrows this but compares DTB-derived values.
- Isolation covers confidentiality and integrity, not availability. A hostile DTB can always stop the boot.
- Discovery is exercised on QEMU virt, `xlnx-versal-virt` and the host tests. No real board DTB has been run.
