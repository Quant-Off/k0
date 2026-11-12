use core::mem::{offset_of, size_of};

use super::*;

fn assert_unique(values: &[i128]) {
    for (i, a) in values.iter().enumerate() {
        for b in &values[i + 1..] {
            assert_ne!(a, b, "duplicate ABI value {a}");
        }
    }
}

#[test]
fn syscall_numbers_are_unique_and_dense() {
    let nrs = [
        syscall::DEBUG_PUTC,
        syscall::YIELD,
        syscall::EXIT,
        syscall::RETYPE,
        syscall::MAP,
        syscall::TCB_CONFIGURE,
        syscall::TCB_RESUME,
        syscall::SEND,
        syscall::RECV,
        syscall::CALL,
        syscall::REPLY_RECV,
        syscall::UNMAP,
        syscall::CAP_COPY,
        syscall::CAP_MOVE,
        syscall::CAP_DELETE,
        syscall::CAP_REVOKE,
        syscall::CONSOLE_READ,
    ];
    assert_unique(&nrs.map(i128::from));
    for (i, nr) in nrs.iter().enumerate() {
        assert_eq!(*nr, i as u64);
    }
}

#[test]
fn error_codes_are_unique_and_negative() {
    let errs = [
        err::BAD_SLOT,
        err::NOT_UNTYPED,
        err::EXHAUSTED,
        err::OCCUPIED,
        err::BAD_TYPE,
        err::BAD_VA,
        err::BAD_PERM,
        err::ALREADY_MAPPED,
        err::MISSING_TABLE,
        err::OVERLAP,
        err::KERNEL_RESOURCE,
        err::BAD_CAP,
        err::BAD_STATE,
        err::WOULD_BLOCK,
        err::NO_REPLY,
        err::NO_RIGHTS,
        err::REVOKED,
        err::BAD_BADGE,
        err::TOO_DEEP,
        err::TIMEOUT,
        err::REPLY_TIMEOUT,
        err::BAD_FLAGS,
        err::BUSY,
        err::NO_DEVICE,
    ];
    assert_unique(&errs.map(i128::from));
    for e in errs {
        assert!(e < 0, "error code {e} must be negative");
        assert!((e as u64) > (i64::MAX as u64), "error code {e} must not look like a slot");
    }
}

#[test]
fn object_types_exclude_zero() {
    let kinds = [
        obj::FRAME,
        obj::PAGE_TABLE,
        obj::TCB,
        obj::ENDPOINT,
        obj::CNODE,
        obj::ADDR_SPACE,
    ];
    assert_unique(&kinds.map(i128::from));
    assert!(kinds.iter().all(|&k| k != 0));
}

#[test]
fn frame_permission_values_are_stable_and_distinct() {
    assert_eq!(perm::RO, 0);
    assert_eq!(perm::RW, 1);
    assert_eq!(perm::RX, 2);
    assert_unique(&[perm::RO, perm::RW, perm::RX].map(i128::from));
}

#[test]
fn rights_are_distinct_bits() {
    let bits = [rights::R, rights::W, rights::X];
    for b in bits {
        assert_eq!(b.count_ones(), 1);
    }
    assert_eq!(rights::ALL, rights::R | rights::W | rights::X);
    assert_eq!(rights::ALL, 7);
}

#[test]
fn fixed_boot_slots_skip_the_null_slot() {
    use bootinfo::slot::*;
    let slots = [TCB, ADDR_SPACE, CONSOLE, CNODE];
    assert_unique(&slots.map(i128::from));
    for (i, s) in slots.iter().enumerate() {
        assert_eq!(*s, i as u64 + 1);
    }
}

#[test]
fn nonblock_is_a_single_flag_bit() {
    assert_eq!(ipc::NONBLOCK.count_ones(), 1);
}

#[test]
fn timeout_occupies_only_the_upper_half_of_the_flags() {
    assert_eq!(ipc::timeout(0), 0);
    assert_eq!(ipc::timeout(1), 1 << 32);
    assert_eq!(ipc::timeout(u32::MAX) >> ipc::TIMEOUT_SHIFT, u64::from(u32::MAX));
    assert_eq!(ipc::timeout(u32::MAX) & 0xFFFF_FFFF, 0);
    assert_eq!(ipc::timeout(u32::MAX) & ipc::NONBLOCK, 0);
    assert_eq!(ipc::timeout(u32::MAX) & ipc::RESERVED, 0);
}

#[test]
fn flag_register_bits_are_all_accounted_for() {
    assert_eq!(ipc::NONBLOCK & ipc::RESERVED, 0);
    assert_eq!(ipc::NONBLOCK | ipc::RESERVED | ipc::timeout(u32::MAX), u64::MAX);
}

#[test]
fn console_read_flag_bits_are_all_accounted_for() {
    assert_eq!(console::NONBLOCK & console::FLUSH, 0);
    assert_eq!((console::NONBLOCK | console::FLUSH) & console::RESERVED, 0);
    assert_eq!(
        console::NONBLOCK | console::FLUSH | console::RESERVED | ipc::timeout(u32::MAX),
        u64::MAX
    );
    // 블록하지 않는 읽기는 IPC와 같은 비트를 씀
    assert_eq!(console::NONBLOCK, ipc::NONBLOCK);
    assert_eq!(console::LOST, 1);
}

#[test]
fn bootinfo_layout_is_stable() {
    assert_eq!(bootinfo::VERSION, 4);
    assert_eq!(bootinfo::SEED_RNG_SEED_BYTES & bootinfo::SEED_RNDR, 0);
    assert_eq!(size_of::<bootinfo::Header>(), 32);
    assert_eq!(offset_of!(bootinfo::Header, version), 0);
    assert_eq!(offset_of!(bootinfo::Header, frame_size), 8);
    assert_eq!(offset_of!(bootinfo::Header, cap_count), 16);
    assert_eq!(offset_of!(bootinfo::Header, cnode_slots), 24);
    assert_eq!(size_of::<bootinfo::CapDesc>(), 32);
    assert_eq!(offset_of!(bootinfo::CapDesc, kind), 0);
    assert_eq!(offset_of!(bootinfo::CapDesc, base), 8);
    assert_eq!(offset_of!(bootinfo::CapDesc, size), 16);
    assert_eq!(offset_of!(bootinfo::CapDesc, rights), 24);
}

#[test]
fn bootinfo_page_is_aligned_for_both_granules() {
    assert_ne!(bootinfo::VA, 0);
    assert_eq!(bootinfo::VA % (16 * 1024), 0);
    assert!(bootinfo::VA < 1 << 48);
}

#[test]
fn cap_kinds_are_unique() {
    use bootinfo::cap_kind::*;
    let kinds = [
        EMPTY, TCB, ADDR_SPACE, UNTYPED, FRAME, PAGE_TABLE, ENDPOINT, CONSOLE, CNODE,
    ];
    assert_unique(&kinds.map(i128::from));
    assert_eq!(EMPTY, 0);
}

#[test]
fn kernel_alias_covers_the_upper_half_only() {
    assert_eq!(KERNEL_VA_OFFSET & ((1 << 48) - 1), 0);
    assert_eq!(KERNEL_VA_OFFSET >> 48, 0xFFFF);
}

#[test]
fn the_image_sits_above_every_alias_address() {
    assert_eq!(PA_CEILING, 1 << 47);
    assert_eq!(KERNEL_IMAGE_VA >> 47, 0x1_FFFF);
    // 별칭이 닿는 가장 높은 주소도 이미지의 시작보다 아래
    assert!(KERNEL_VA_OFFSET + (PA_CEILING - 1) < KERNEL_IMAGE_VA);
}
