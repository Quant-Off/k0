use super::*;

const G: u64 = GRANULE as u64;

#[test]
fn user_permissions_obey_wx() {
    let perms = [UserPerm::TextUser, UserPerm::RoUser, UserPerm::RwUser];
    let mut executable = 0;
    for p in perms {
        let a = p.attrs();
        assert_eq!(a & 0b11, DESC_PAGE);
        assert_ne!(a & ATTR_AF, 0);
        assert_ne!(a & (1 << 6), 0, "user mappings are EL0-accessible");
        assert_ne!(a & PXN, 0, "the kernel never executes user pages");
        if a & (1 << 7) == 0 {
            assert_ne!(a & UXN, 0, "writable user mapping must be UXN");
        }
        if a & UXN == 0 {
            executable += 1;
            assert_eq!(a & (0b11 << 6), AP_RO_ALL);
        }
    }
    assert_eq!(executable, 1);
}

#[test]
fn frame_window_must_be_granule_aligned_and_non_empty() {
    for (s, e) in [(1, G), (0, G + 1), (G, G), (2 * G, G)] {
        assert_eq!(FrameAlloc::new(s..e).err(), Some(MmuError::Misaligned), "{s:#x}..{e:#x}");
    }
    let fa = FrameAlloc::new(G..4 * G).unwrap();
    assert_eq!(fa.used(), G..G);
}

#[test]
fn frame_alloc_rejects_requests_it_cannot_satisfy() {
    let mut fa = FrameAlloc::new(G..4 * G).unwrap();
    for n in [0, 4, u64::MAX, u64::MAX / G + 1] {
        assert_eq!(fa.alloc_contig(n), Err(MmuError::OutOfFrames), "n = {n}");
        assert_eq!(fa.used(), G..G);
    }
    let top_end = u64::MAX & !(G - 1);
    let mut top = FrameAlloc::new(top_end - G..top_end).unwrap();
    assert_eq!(top.alloc_contig(3), Err(MmuError::OutOfFrames));
    assert_eq!(top.used(), top_end - G..top_end - G);
}

#[test]
fn user_range_validation_runs_before_any_table_access() {
    let mut us = UserSpace { root: 0 };
    let mut fa = FrameAlloc::new(G..2 * G).unwrap();
    let cases = [
        (G + 1, 0, G, MmuError::Misaligned),
        (G, 1, G, MmuError::Misaligned),
        (G, 0, G - 1, MmuError::Misaligned),
        (G, 0, 0, MmuError::Misaligned),
        (0, 0, G, MmuError::NullPage),
        ((1 << 48) - G, 0, 2 * G, MmuError::Misaligned),
        (u64::MAX - G + 1, 0, G, MmuError::Misaligned),
    ];
    for (va, pa, len, want) in cases {
        assert_eq!(us.map_range(&mut fa, va, pa, len, UserPerm::RwUser), Err(want), "{va:#x}");
    }
    assert_eq!(fa.used(), G..G);
}

#[test]
fn runtime_walkers_validate_before_dereferencing() {
    let cases = [
        (G + 4, 0, MmuError::Misaligned),
        (G, 4, MmuError::Misaligned),
        (0, 0, MmuError::NullPage),
        (1 << 48, 0, MmuError::Misaligned),
        ((1 << 48) - G + G, 0, MmuError::Misaligned),
        (u64::MAX - G + 1, 0, MmuError::Misaligned),
    ];
    for (va, pa, want) in cases {
        // SAFETY: 이 입력들은 전부 검증 단계에서 거부되어 root_pa를 역참조하지 않음
        let r = unsafe { user_map_frame(0, va, pa, UserPerm::RwUser) };
        assert_eq!(r, Err(want), "{va:#x}");
    }
    for (va, table) in [(G, 4), (1 << 48, 0), (u64::MAX, 0)] {
        // SAFETY: 위와 동일
        let r = unsafe { user_install_table(0, va, table) };
        assert_eq!(r, Err(MmuError::Misaligned), "{va:#x}");
    }
}
