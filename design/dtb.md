# The device tree as untrusted input

[한국어](dtb_KR.md)

The bootloader passes K0 a flattened device tree (DTB) that describes the machine: how much RAM exists and where, which ranges the firmware has reserved, and some boot entropy. K0 treats this blob as hostile input. This note explains why, what the parser reads, and how it is tested. The parser is `crates/k0-boot/src/fdt.rs`.

## Why the DTB is not trusted

On many platforms the DTB does not come from the same place as the kernel image. It may be a separate file next to a signed kernel, an overlay applied by firmware, or a blob that a bootloader edits before it jumps. A genuine kernel can therefore receive a wrong or hostile DTB. K0 assumes it might.

The parser follows one principle. Values from the DTB may make the kernel refuse to boot or give it fewer resources, but they must never grant more authority than the hardware really offers.

- **Reserved ranges only remove memory.** A hostile reservation can at worst shrink the memory handed to user space.
- **Entropy is only one input among several.** The bytes from `/chosen` are mixed with other sources through SHA-256, so hostile entropy cannot weaken the PAC keys. A missing seed does lower their quality, and the kernel says so in the boot log (see [hardening.md](hardening.md)).
- **Memory nodes grant memory, so they are checked hardest.** Every memory region becomes untyped memory that user space can map. A region that claimed to be RAM but covered a device would hand EL0 direct access to that device. After parsing, the kernel compares every memory region with its own MMIO windows (the UART and, on QEMU, the GIC) and stops the boot on any overlap. The CNode bootstrap also rejects memory regions that overlap each other, so no frame can be retyped twice.

## Two stages

The blob has to be mapped before the kernel can read it with the MMU on, and the full parse needs relocated data that only works after the jump to the higher half (see [boot.md](boot.md)). So the parser runs in two stages.

**`dtb_span`, before the MMU is on.** It reads only the first eight bytes at the physical address from `x0`. The address must not be null and must be 8-byte aligned. The magic must be `0xd00dfeed`. The total size must be between 40 bytes (one header) and 2 MiB. The upper bound limits how many page tables the DTB mapping can consume. The result is the physical range that `enable_paging` maps read-only.

**`parse`, after the jump.** It reads through the kernel alias but reports physical addresses. It runs `dtb_span` again at the alias address and rejects the blob if its physical range overlaps the kernel image. Then it builds a byte slice of exactly the total size and hands it to `parse_blob`. From that point on, every access is a bounds-checked slice access in safe Rust, and every offset computation uses checked arithmetic. A malformed blob can make `parse_blob` return an error, but it cannot make it read outside the slice.

## What the parser reads

The header must announce version 17 or later and be backward compatible with version 17. The structure block must start on a 4-byte boundary and have a size that is a multiple of 4. The structure block and the strings block must both end inside the blob.

The parser reads exactly these items and skips everything else:

| Source | Item |
| --- | --- |
| memory reservation block | `(address, size)` pairs up to the terminating `(0, 0)` pair. Pairs with size 0 are skipped. The block must be 8-byte aligned. |
| root node | `#address-cells` and `#size-cells`. The defaults are 2 and 1. Only 1 and 2 are supported. |
| top-level `memory` or `memory@...` nodes | `reg`, decoded with the root cell sizes |
| children of `/reserved-memory` | `reg`, decoded with the cell sizes of `/reserved-memory` itself |
| `/chosen` | `rng-seed` and `kaslr-seed`, concatenated and capped at 72 bytes |

Nodes with these names at other depths are ignored. A node called `memory` inside `/soc` is not RAM. Reserved-memory children that give only a size and no `reg` are dynamic allocations that the firmware has not pinned to an address, so they are ignored too.

Fixed limits keep the parser's own state small and static:

| Limit | Value |
| --- | --- |
| total size | 2 MiB |
| nesting depth | 32 |
| memory regions | 8 |
| reserved regions, both sources together | 8 |
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
- no memory at all.

## How it is tested

The parser is pure logic over a byte slice, so it is tested on the host.

- **Unit tests** (`crates/k0-boot/src/fdt/tests.rs`) cover each rule above, including the header checks run in place through a raw pointer, the overlap check against the kernel image, and the read-through-an-alias path.
- **A model.** A test-only builder (`fdt/builder.rs`) writes DTBs from a small description of a board and computes what the parser should return. Many unit tests compare the parser's output with this model, not with hand-written values.
- **A deterministic mutation fuzzer** (`fdt/fuzz.rs`). Each case generates a random board, wraps it in random unrelated nodes, properties and `NOP` tokens, and checks the parse against the model. Then it applies up to four mutations, such as bit flips, header field rewrites, truncation, splicing and token insertion, and parses the result twice: once as a slice and once through the raw-pointer entry point backed by a 2 MiB zero-filled buffer. Any panic fails the test. Any accepted result must respect every limit in the table above. The fuzzer needs no extra crates and runs fully offline, but it has no coverage feedback.

The default test run executes 20,000 cases. CI runs five million cases every week with a new seed. When this note was written, five campaigns of five million cases each had found no parser failure. The only failure the fuzzer ever reported was in the test model itself, which had not known that a `(0, 0)` pair ends the memory reservation block.

## Known gaps

- Region addresses are not checked against the CPU's physical address width. A DTB that declares RAM above the implemented range can make the kernel panic at boot or during a later retype. This is advisory GHSA-hj3f-38h9-hrpj in [SECURITY.md](../SECURITY.md). Its impact is a denial of service by whoever controls the DTB.
- The MMIO overlap check knows only the windows the kernel maps for itself. A DTB that describes some other device range as RAM is not caught, because the kernel has no independent list of devices.
