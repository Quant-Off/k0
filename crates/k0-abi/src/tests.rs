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
        err::OUT_OF_SLOTS,
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
    ];
    assert_unique(&errs.map(i128::from));
    for e in errs {
        assert!(e < 0, "error code {e} must be negative");
        assert!((e as u64) > (i64::MAX as u64), "error code {e} must not look like a slot");
    }
}

#[test]
fn object_types_exclude_zero() {
    let kinds = [obj::FRAME, obj::PAGE_TABLE, obj::TCB, obj::ENDPOINT];
    assert_unique(&kinds.map(i128::from));
    assert!(kinds.iter().all(|&k| k != 0));
}

#[test]
fn frame_permissions_are_read_only_or_read_write() {
    assert_eq!(perm::RO, 0);
    assert_eq!(perm::RW, 1);
}

#[test]
fn nonblock_is_a_single_flag_bit() {
    assert_eq!(ipc::NONBLOCK.count_ones(), 1);
}

#[test]
fn bootinfo_layout_is_stable() {
    assert_eq!(size_of::<bootinfo::Header>(), 24);
    assert_eq!(offset_of!(bootinfo::Header, version), 0);
    assert_eq!(offset_of!(bootinfo::Header, frame_size), 8);
    assert_eq!(offset_of!(bootinfo::Header, cap_count), 16);
    assert_eq!(size_of::<bootinfo::CapDesc>(), 24);
    assert_eq!(offset_of!(bootinfo::CapDesc, kind), 0);
    assert_eq!(offset_of!(bootinfo::CapDesc, base), 8);
    assert_eq!(offset_of!(bootinfo::CapDesc, size), 16);
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
    let kinds = [EMPTY, TCB, ADDR_SPACE, UNTYPED, FRAME, PAGE_TABLE, ENDPOINT, CONSOLE];
    assert_unique(&kinds.map(i128::from));
    assert_eq!(EMPTY, 0);
}

#[test]
fn kernel_alias_covers_the_upper_half_only() {
    assert_eq!(KERNEL_VA_OFFSET & ((1 << 48) - 1), 0);
    assert_eq!(KERNEL_VA_OFFSET >> 48, 0xFFFF);
}
