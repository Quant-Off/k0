# Kernel page tables and W^X

[한국어](memory_KR.md)

This note describes how the kernel maps itself: the translation setup, the static table pool, the permission scheme, and the checks that confirm the result at boot. The code is in `crates/k0-mm/src/paging.rs`. User address spaces are covered in [user-address-space.md](user-address-space.md).

## Two halves

AArch64 splits the virtual address space between two table roots.

- **`TTBR0_EL1`** covers the lower half. During boot it holds an identity map, where each virtual address equals its physical address. The kernel needs it to keep running at the moment the MMU turns on. Later, `TTBR0_EL1` is switched to the root task's tables and the identity map disappears.
- **`TTBR1_EL1`** covers the upper half. It holds the kernel's alias of physical memory at a fixed offset: virtual address = physical address + `0xFFFF_0000_0000_0000`. After the jump to the higher half, the kernel uses only this alias.

Both halves use 48-bit virtual addresses (`T0SZ = T1SZ = 16`) and four levels of tables. The granule is 4 KiB on QEMU virt and 16 KiB on Apple Silicon, selected by the `plat-virt` or `plat-apple` feature. `TG0` and `TG1` encode the same granule with different values, and the code handles that explicitly. The physical address size (`IPS`) comes from the CPU's reported range, capped at 48 bits.

The kernel maps only pages, never blocks. Every mapping goes through the same walk to the last level, so there is one code path to reason about.

## The static table pool

The kernel has no heap. Its page tables come from a pool of 32 tables in `.bss`, handed out by a counter that only goes up. Two tables become the roots of the two halves. The rest become intermediate tables as mappings are added. On QEMU virt, 17 of the 32 are in use once boot finishes.

When the walk follows an existing table entry, `index_of` converts the address in that entry back into a pool index. It rejects any address that does not land exactly on an allocated table in the pool. A corrupted or forged table address therefore stops the walk with `BadTable`. It is never followed into arbitrary memory.

### Failed mappings are rolled back

`map_page` remembers every link it creates on the way down. If the walk fails, for example because the pool runs out or the leaf entry is already in use, it clears those links and restores the counter. The pool ends up exactly as it was before the call.

This matters after boot. Each `RETYPE` system call maps the new object into the kernel alias through this pool. Without the rollback, a user task could issue retypes that fail halfway and drain the pool a few tables at a time, until the kernel itself could no longer map anything. With the rollback, a failed retype costs nothing. Two host tests show this: one fails a walk midway and checks that the pool is unchanged, and the other fails a thousand times in a row on a nearly full pool.

## Permissions

All kernel mappings follow one rule: nothing is writable and executable at the same time, and nothing the kernel maps for itself is accessible from EL0.

| Kind | Used for | EL1 access | EL1 execute | EL0 access |
| --- | --- | --- | --- | --- |
| `Text` | kernel code | read | yes | none |
| `Ro` | read-only data, the DTB | read | no | none |
| `Rw` | data, `.bss`, stacks, frame windows | read and write | no | none |
| `Device` | UART, GIC | read and write | no | none |

Every kernel mapping sets `UXN`, and every one except `Text` sets `PXN`. Memory attribute index 0 is normal write-back memory and index 1 is Device-nGnRE.

`SCTLR_EL1.WXN` is set when the MMU turns on. With `WXN`, the hardware refuses to execute from any writable page, whatever its execute bits say. A bug that produced a writable and executable entry would still not give an executable writable page.

The linker script supports this split. It aligns text, read-only data and data to granule boundaries, and its ELF program headers carry the same R+X, R and R+W flags.

## Guard pages

The boot stack is 64 KiB and sits after `.bss`. The granule just below it is left out of both halves, so a stack overflow faults immediately instead of overwriting `.bss`. The exception vectors have their own 8 KiB stack in `.bss`. The fatal path switches to it, so the kernel can still print a diagnosis when the fault was itself a stack overflow.

## Turning the MMU on

`switch_on` writes `MAIR_EL1`, `TCR_EL1` and both table roots, then issues barriers, invalidates the TLB and invalidates the whole instruction cache before it sets `SCTLR_EL1.M`, `C`, `I` and `WXN`. The instruction cache invalidation is there because the kernel does not trust anything the bootloader may have left in the cache.

`enable_paging` can run only once. A second call returns `AlreadyEnabled`. On success it returns an `Mmu` token that is meant to prove paging is on. No later step consumes it yet, because the token cannot be carried across the jump to the higher half, so the order of `kernel_main` enforces that sequence instead.

## Adding mappings later

`map_kernel_window` adds read-write ranges to the higher half after boot. It is used twice:

- for the 2 MiB boot frame window, before the root task is loaded, and
- for every object created by `RETYPE`, so that the kernel can zero and use it through the alias.

These calls only turn invalid entries into valid ones. Existing translations never change, so no break-before-make sequence is needed. A store barrier and an instruction barrier make the new entries visible to the table walker before the memory is used.

## Self-checks at boot

Right after the jump, `check_wx` in `kernel/src/main.rs` asks the hardware with address translation instructions (`AT S1E1R`, `AT S1E1W`) whether each case behaves as intended:

| Check | Expected |
| --- | --- |
| kernel text readable | yes |
| kernel text writable | no |
| read-only data writable | no |
| boot stack guard page readable | no |
| boot stack writable | yes |
| identity map of kernel text readable | yes, until the user tables replace it |

Any mismatch stops the boot with the name of the check. These checks test the tables the hardware actually walks, not the code that built them.

## How it is tested

- Host unit tests in `crates/k0-mm/src/paging/tests.rs` check the index arithmetic for both granules, the W^X properties of every kernel permission, the rollback after a failed walk, and the rejection of forged or block entries. They run on AArch64 hosts.
- `tools/qemu-boot-test.sh` requires the line `k0: w^x checks pass`.

## Known gaps

- The pool size is fixed at 32 tables. If retypes spread over enough distinct address ranges, the pool runs out and `RETYPE` returns `KERNEL_RESOURCE`. Nothing leaks, but the pool is never reclaimed.
- Mappings are never removed. Without `revoke`, a retyped object stays mapped in the kernel alias forever.
- Address space identifiers (ASIDs) are not used. Every switch to a different user address space invalidates the whole TLB.
