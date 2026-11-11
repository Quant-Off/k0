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

#[test]
fn registry_hands_out_distinct_ids_and_never_zero() {
    let mut r = AddrSpaces::new();
    let mut seen = std::collections::BTreeSet::new();
    for i in 0..MAX_ADDR_SPACES as u64 - 1 {
        let asid = r.register(0x1000 * (i + 1)).expect("room left");
        assert_ne!(asid, 0);
        assert!(usize::from(asid) < MAX_ADDR_SPACES);
        assert!(seen.insert(asid), "asid {asid} handed out twice");
        assert_eq!(r.get(asid).map(|e| e.0), Some(0x1000 * (i + 1)));
    }
    // 가득 차면 거부하고 상태는 그대로
    assert_eq!(r.register(0xdead_0000), None);
    assert_eq!(r.register(0xdead_0000), None);
    assert_eq!(seen.len(), MAX_ADDR_SPACES - 1);
}

#[test]
fn registry_rejects_null_roots_and_bad_ids() {
    let mut r = AddrSpaces::new();
    assert_eq!(r.register(0), None);
    let a = r.register(0x4000).unwrap();
    assert_eq!(r.get(0), None);
    assert_eq!(r.get(u16::MAX), None);
    assert_eq!(r.get(MAX_ADDR_SPACES as u16), None);
    assert!(!r.release(0, 0x4000));
    assert!(!r.release(u16::MAX, 0x4000));
    assert!(!r.release(a, 0x5000), "release must name the registered root");
    assert!(!r.release(a, 0));
    assert_eq!(r.get(a).map(|e| e.0), Some(0x4000));
    assert!(r.release(a, 0x4000));
    assert!(!r.release(a, 0x4000), "double release is refused");
    assert_eq!(r.get(a), None);
}

#[test]
fn a_stale_generation_never_resolves_again() {
    let mut r = AddrSpaces::new();
    let a = r.register(0x4000).unwrap();
    let (_, g) = r.get(a).unwrap();
    assert_eq!(r.resolve(a, g), Some(0x4000));
    assert_eq!(r.resolve(a, g + 1), None);
    assert!(r.release(a, 0x4000));
    assert_eq!(r.resolve(a, g), None);
    // 같은 번호가 같은 루트 PA로 재사용돼도 옛 기록은 일치하지 않음
    let b = r.register(0x4000).unwrap();
    assert_eq!(b, a, "the freed id is reused first");
    let (_, g2) = r.get(b).unwrap();
    assert_ne!(g2, g);
    assert_eq!(r.resolve(a, g), None);
    assert_eq!(r.resolve(b, g2), Some(0x4000));
}

#[test]
fn registry_survives_random_register_release_sequences() {
    let mut r = AddrSpaces::new();
    let mut live: std::collections::BTreeMap<u16, (u64, u64)> = Default::default();
    let mut stale: Vec<(u16, u64)> = Vec::new();
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for step in 0..20_000u64 {
        if next() % 3 != 0 {
            let root = 0x1000 * (step + 1);
            match r.register(root) {
                Some(a) => {
                    assert!(!live.contains_key(&a), "live asid handed out again");
                    let (got, g) = r.get(a).unwrap();
                    assert_eq!(got, root);
                    live.insert(a, (root, g));
                }
                None => assert_eq!(live.len(), MAX_ADDR_SPACES - 1),
            }
        } else if let Some((&a, &(root, g))) = live.iter().nth((next() % 7) as usize) {
            assert!(r.release(a, root));
            live.remove(&a);
            stale.push((a, g));
        }
        if step % 97 == 0 {
            for (&a, &(root, g)) in &live {
                assert_eq!(r.resolve(a, g), Some(root));
            }
            for &(a, g) in &stale {
                assert_eq!(r.resolve(a, g), None, "stale record resolved");
            }
        }
    }
}

#[test]
fn an_entry_whose_generation_is_exhausted_is_retired() {
    let mut r = AddrSpaces::new();
    let a = r.register(0x4000).unwrap();
    r.entries[usize::from(a)].generation = u64::MAX;
    assert!(r.release(a, 0x4000));
    // 세대를 더 올릴 수 없는 항목은 다시 나오지 않음
    for i in 0..MAX_ADDR_SPACES as u64 {
        if let Some(b) = r.register(0x8000 + 0x1000 * i) {
            assert_ne!(b, a);
        }
    }
    assert_eq!(r.resolve(a, u64::MAX), None);
}

#[test]
fn unmap_walkers_reject_bad_arguments_before_touching_tables() {
    // 정렬이 어긋났거나 범위를 벗어난 기록은 테이블을 걷기 전에 걸러짐
    unsafe {
        assert_eq!(user_unmap_frame(0, G + 1, 0x8000_0000), Ok(false));
        assert_eq!(user_unmap_frame(0, 1 << 48, 0x8000_0000), Ok(false));
        assert_eq!(user_unmap_frame(0, G, 0x8000_0001), Ok(false));
        assert_eq!(user_unmap_frame(0, G, 1 << 48), Ok(false));
        assert_eq!(user_uninstall_table(0, G, 3, 0x8000_0000), Err(MmuError::BadTable));
        assert_eq!(user_uninstall_table(0, 1 << 48, 0, 0x8000_0000), Ok(false));
        assert_eq!(user_uninstall_table(0, G, 0, 0x8000_0001), Ok(false));
    }
}

/// 호스트 메모리에 소거된 테이블 한 장을 만들어 그 주소를 PA처럼 쓰는 함수
/// (테스트 빌드에서는 테이블 오프셋이 0)
fn table() -> u64 {
    let layout = std::alloc::Layout::from_size_align(GRANULE, GRANULE).unwrap();
    // SAFETY: 크기가 0이 아닌 유효한 레이아웃
    let p = unsafe { std::alloc::alloc_zeroed(layout) } as u64;
    assert!(p != 0 && p & !ADDR_MASK == 0, "host table address must fit a descriptor");
    p
}

fn entry(table_pa: u64, va: u64, level: u32) -> u64 {
    // SAFETY: table()이 준 살아 있는 테이블 안의 엔트리
    unsafe { entry_ptr(table_pa, index(va, level)).read() }
}

/// va 경로를 따라 내려가 리프 엔트리를 읽는 함수(경로가 끊겨 있으면 None)
fn leaf(root: u64, va: u64) -> Option<u64> {
    let mut t = root;
    for level in 0..3 {
        let e = entry(t, va, level);
        if e == 0 {
            return None;
        }
        t = e & ADDR_MASK;
    }
    Some(entry(t, va, 3))
}

const VA: u64 = 0x0000_0040_0000_0000;

#[test]
fn tables_install_top_down_and_frames_need_the_full_path() {
    let root = table();
    let tables = [table(), table(), table()];
    let frame = 0x8000_0000u64;
    unsafe {
        assert_eq!(user_map_frame(root, VA, frame, UserPerm::RwUser), Err(MmuError::MissingTable));
        for (level, &t) in tables.iter().enumerate() {
            assert_eq!(user_install_table(root, VA, t), Ok(level as u32));
            assert_eq!(leaf(root, VA).is_some(), level == 2);
        }
        // 경로가 완성되면 더 설치할 자리가 없음
        assert_eq!(user_install_table(root, VA, table()), Err(MmuError::Overlap));
        assert_eq!(user_map_frame(root, VA, frame, UserPerm::RwUser), Ok(()));
        assert_eq!(leaf(root, VA), Some(frame | UserPerm::RwUser.attrs()));
        assert_eq!(user_map_frame(root, VA, frame + G, UserPerm::RoUser), Err(MmuError::Overlap));
        assert_eq!(leaf(root, VA), Some(frame | UserPerm::RwUser.attrs()));
        // 이웃 페이지는 같은 테이블을 씀
        assert_eq!(user_map_frame(root, VA + G, frame + G, UserPerm::TextUser), Ok(()));
        assert_eq!(leaf(root, VA + G), Some((frame + G) | UserPerm::TextUser.attrs()));
    }
}

#[test]
fn unmap_clears_only_the_leaf_that_maps_this_very_frame() {
    let root = table();
    for _ in 0..3 {
        unsafe { user_install_table(root, VA, table()) }.unwrap();
    }
    let (a, b) = (0x8000_0000u64, 0x9000_0000u64);
    unsafe {
        user_map_frame(root, VA, a, UserPerm::RwUser).unwrap();
        // 그 자리에 다른 프레임이 있다고 기록된 낡은 케이퍼빌리티는 아무것도 지우지 못함
        assert_eq!(user_unmap_frame(root, VA, b), Ok(false));
        assert_eq!(leaf(root, VA), Some(a | UserPerm::RwUser.attrs()));
        // 비어 있는 자리와 테이블이 없는 자리도 마찬가지
        assert_eq!(user_unmap_frame(root, VA + G, a), Ok(false));
        assert_eq!(user_unmap_frame(root, VA + (1 << 39), a), Ok(false));
        assert_eq!(user_unmap_frame(root, VA, a), Ok(true));
        assert_eq!(leaf(root, VA), Some(0));
        assert_eq!(user_unmap_frame(root, VA, a), Ok(false));
        // 비워진 자리는 다시 쓸 수 있음
        assert_eq!(user_map_frame(root, VA, b, UserPerm::RoUser), Ok(()));
    }
}

#[test]
fn uninstall_cuts_exactly_one_link_and_a_zeroed_table_carries_nothing() {
    let root = table();
    let t: Vec<u64> = (0..3).map(|_| table()).collect();
    for &x in &t {
        unsafe { user_install_table(root, VA, x) }.unwrap();
    }
    let frame = 0x8000_0000u64;
    unsafe {
        user_map_frame(root, VA, frame, UserPerm::RwUser).unwrap();
        // 레벨이나 테이블이 기록과 다르면 아무것도 끊지 않음
        assert_eq!(user_uninstall_table(root, VA, 1, t[0]), Ok(false));
        assert_eq!(user_uninstall_table(root, VA, 0, t[1]), Ok(false));
        assert_eq!(user_uninstall_table(root, VA, 2, table()), Ok(false));
        assert!(leaf(root, VA).is_some());
        // 가운데 테이블을 떼면 그 아래는 전부 도달 불가능해짐
        assert_eq!(user_uninstall_table(root, VA, 1, t[1]), Ok(true));
        assert_eq!(leaf(root, VA), None);
        assert_eq!(user_unmap_frame(root, VA, frame), Ok(false));
        assert_eq!(user_uninstall_table(root, VA, 2, t[2]), Ok(false));
        assert_eq!(user_uninstall_table(root, VA, 1, t[1]), Ok(false));
        // 떼어 낸 테이블은 아직 옛 링크를 품고 있으므로 소거한 뒤에만 다시 설치함.
        // 소거 후에는 다른 자리에 설치해도 옛 매핑이 따라오지 않음
        assert_ne!(entry(t[1], VA, 2), 0);
        zero_table(t[1]);
        assert_eq!(entry(t[1], VA, 2), 0);
        // 두 그래뉼 모두에서 최상위 엔트리가 달라지는 주소(비트 47)
        let other = VA + (1 << 47);
        assert_eq!(user_install_table(root, other, table()), Ok(0));
        assert_eq!(user_install_table(root, other, t[1]), Ok(1));
        assert_eq!(leaf(root, other), None);
        assert_eq!(user_map_frame(root, other, frame, UserPerm::RwUser), Err(MmuError::MissingTable));
    }
}

#[test]
fn a_block_or_leaf_shaped_entry_above_the_last_level_stops_the_walk() {
    let root = table();
    unsafe {
        // 커널은 블록 디스크립터를 쓰지 않으므로 보이면 손상임
        entry_ptr(root, index(VA, 0)).write(0x8000_0000 | 0b01);
        assert_eq!(user_map_frame(root, VA, 0x9000_0000, UserPerm::RwUser), Err(MmuError::BadTable));
        assert_eq!(user_install_table(root, VA, table()), Err(MmuError::BadTable));
        assert_eq!(user_unmap_frame(root, VA, 0x9000_0000), Err(MmuError::BadTable));
        assert_eq!(user_uninstall_table(root, VA, 1, 0x9000_0000), Err(MmuError::BadTable));
    }
}

#[test]
fn random_map_and_unmap_agree_with_a_model() {
    let root = table();
    for _ in 0..3 {
        unsafe { user_install_table(root, VA, table()) }.unwrap();
    }
    let mut model: std::collections::BTreeMap<u64, u64> = Default::default();
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..20_000 {
        let va = VA + (next() % 48) * G;
        let pa = 0x8000_0000 + (next() % 6) * G;
        if next() % 2 == 0 {
            let got = unsafe { user_map_frame(root, va, pa, UserPerm::RwUser) };
            match model.get(&va) {
                Some(_) => assert_eq!(got, Err(MmuError::Overlap)),
                None => {
                    assert_eq!(got, Ok(()));
                    model.insert(va, pa);
                }
            }
        } else {
            let got = unsafe { user_unmap_frame(root, va, pa) };
            let hit = model.get(&va) == Some(&pa);
            assert_eq!(got, Ok(hit));
            if hit {
                model.remove(&va);
            }
        }
    }
    for i in 0..48 {
        let va = VA + i * G;
        let want = model.get(&va).map_or(0, |pa| pa | UserPerm::RwUser.attrs());
        assert_eq!(leaf(root, va), Some(want));
    }
}

#[test]
fn the_top_level_covers_the_whole_48_bit_user_range() {
    // 4KiB 그래뉼은 최상위 512칸, 16KiB 그래뉼은 2칸: 가장 높은 사용자 주소도
    // 루트 테이블 안의 엔트리로 떨어져야 함
    let root = table();
    let top = (1u64 << 48) - G;
    unsafe {
        assert_eq!(user_install_table(root, top, table()), Ok(0));
        assert!(index(top, 0) * 8 < GRANULE);
        assert_ne!(entry(root, top, 0), 0);
        assert_eq!(user_install_table(root, 1 << 48, table()), Err(MmuError::Misaligned));
    }
}
