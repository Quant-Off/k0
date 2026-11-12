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

/// 그래뉼 `pages`개를 다루는 등록부를 만드는 함수입니다. 구간은 가짜 물리
/// 주소 `first_pa`에서 시작하고, 항목 배열은 호스트 메모리에 둡니다(테스트
/// 빌드에서는 테이블 오프셋이 0이라 그 주소가 곧 PA 구실을 함).
fn registry(first_pa: u64, pages: u64) -> AddrSpaces {
    let mut r = AddrSpaces::new();
    let storage = Box::leak(vec![0u64; pages as usize].into_boxed_slice()).as_mut_ptr() as u64;
    // SAFETY: 방금 할당한 소거된 메모리이고 테스트가 끝날 때까지 살아 있음
    assert!(unsafe { r.add_range(first_pa, pages, storage) });
    r
}

const BASE: u64 = 0x4000_0000;

#[test]
fn every_granule_of_a_range_can_hold_an_address_space_at_once() {
    // 예전의 고정 표(256칸)보다 많은 주소 공간이 한꺼번에 살아 있을 수 있음.
    // 번호는 루트 테이블이 놓인 그래뉼의 번호
    let pages = 1000;
    let mut r = registry(BASE, pages);
    for i in 0..pages {
        let root = BASE + i * G;
        let asid = r.register(root).expect("every granule has an entry");
        assert_eq!(asid, root / G);
        assert_eq!(r.get(asid), Some((root, 0)));
    }
    // 이미 살아 있는 그래뉼은 다시 등록되지 않고 상태도 그대로
    assert_eq!(r.register(BASE), None);
    assert_eq!(r.get(BASE / G), Some((BASE, 0)));
}

#[test]
fn registry_rejects_what_it_does_not_cover() {
    let mut r = registry(BASE, 4);
    assert_eq!(r.register(0), None);
    assert_eq!(r.register(BASE - G), None, "below the range");
    assert_eq!(r.register(BASE + 4 * G), None, "past the range");
    assert_eq!(r.register(BASE + 8), None, "not a granule boundary");
    assert_eq!(r.get(0), None);
    assert_eq!(r.get(u64::MAX), None);
    assert_eq!(r.get(BASE / G + 4), None);
    assert_eq!(r.resolve(u64::MAX, 0), None);

    let a = r.register(BASE + G).unwrap();
    assert!(!r.release(0, BASE + G));
    assert!(!r.release(u64::MAX, BASE + G));
    assert!(!r.release(a, BASE), "release must name the registered root");
    assert!(!r.release(a, 0));
    assert!(!r.release(a + 1, BASE + 2 * G), "that granule holds nothing");
    assert_eq!(r.get(a), Some((BASE + G, 0)));
    assert!(r.release(a, BASE + G));
    assert!(!r.release(a, BASE + G), "double release is refused");
    assert_eq!(r.get(a), None);
}

#[test]
fn a_stale_generation_never_resolves_again() {
    let mut r = registry(BASE, 4);
    let a = r.register(BASE).unwrap();
    let (_, g) = r.get(a).unwrap();
    assert_eq!(r.resolve(a, g), Some(BASE));
    assert_eq!(r.resolve(a, g + 1), None);
    assert!(r.release(a, BASE));
    assert_eq!(r.resolve(a, g), None);
    // 같은 그래뉼에 새 주소 공간이 생기면 번호는 같지만 옛 기록은 일치하지 않음
    let b = r.register(BASE).unwrap();
    assert_eq!(b, a, "the number is the granule, so it comes back");
    let (_, g2) = r.get(b).unwrap();
    assert_ne!(g2, g);
    assert_eq!(r.resolve(a, g), None);
    assert_eq!(r.resolve(b, g2), Some(BASE));
}

#[test]
fn registry_survives_random_register_release_sequences() {
    let pages = 64u64;
    let mut r = registry(BASE, pages);
    let mut live: std::collections::BTreeMap<u64, (u64, u64)> = Default::default();
    let mut stale: Vec<(u64, u64)> = Vec::new();
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for step in 0..20_000u64 {
        if next() % 3 != 0 {
            let root = BASE + (next() % pages) * G;
            match r.register(root) {
                Some(a) => {
                    assert!(!live.contains_key(&a), "live number handed out again");
                    let (got, g) = r.get(a).unwrap();
                    assert_eq!(got, root);
                    live.insert(a, (root, g));
                }
                None => assert!(live.contains_key(&(root / G)), "a free granule was refused"),
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
fn a_granule_whose_generation_is_exhausted_is_retired() {
    let mut r = registry(BASE, 2);
    let a = r.register(BASE).unwrap();
    // 세대를 끝까지 올린 항목을 직접 만듦
    assert!(r.write(a, AS_LIVE | (AS_LIVE - 2)));
    assert!(r.release(a, BASE));
    assert_eq!(r.read(a), Some(AS_LIVE - 1));
    // 더 올릴 수 없는 그래뉼은 다시 주소 공간의 루트가 되지 못함. 그러지
    // 않으면 세대가 되감겨 아주 오래된 기록이 다시 일치하게 됨
    assert_eq!(r.register(BASE), None);
    assert_eq!(r.resolve(a, AS_LIVE - 2), None);
    assert_eq!(r.resolve(a, AS_LIVE - 1), None);
    // 다른 그래뉼은 영향이 없음
    assert!(r.register(BASE + G).is_some());
}

#[test]
fn the_root_task_address_space_has_its_own_entry() {
    let mut r = registry(BASE, 4);
    let boot_root = 0x7000_0000;
    assert_eq!(r.register_boot(0), None);
    assert_eq!(r.register_boot(boot_root + 8), None);
    let a = r.register_boot(boot_root).unwrap();
    assert_eq!(a, boot_root / G);
    assert_eq!(r.register_boot(boot_root + G), None, "only one boot root");
    let (root, g) = r.get(a).unwrap();
    assert_eq!(root, boot_root);
    assert_eq!(r.resolve(a, g), Some(boot_root));
    // 구간의 항목과 섞이지 않음
    let b = r.register(BASE).unwrap();
    assert_ne!(a, b);
    assert!(r.release(a, boot_root));
    assert_eq!(r.resolve(a, g), None);
    assert_eq!(r.get(b), Some((BASE, 0)));
}

#[test]
fn ranges_are_independent_and_the_table_of_ranges_is_bounded() {
    let mut r = AddrSpaces::new();
    let store = |n: u64| Box::leak(vec![0u64; n as usize].into_boxed_slice()).as_mut_ptr() as u64;
    // SAFETY: 각 항목 배열은 방금 할당한 소거된 메모리임
    unsafe {
        assert!(!r.add_range(BASE, 0, store(1)), "empty range");
        assert!(!r.add_range(BASE + 1, 1, store(1)), "unaligned range");
        for i in 0..MAX_AS_RANGES as u64 {
            assert!(r.add_range(BASE + i * 16 * G, 8, store(8)));
        }
        assert!(!r.add_range(BASE + 0x1000_0000, 8, store(8)), "no room for more ranges");
    }
    // 구간 사이의 빈 곳은 다루지 않음
    assert!(r.register(BASE + 7 * G).is_some());
    assert_eq!(r.register(BASE + 8 * G), None);
    let last = BASE + (MAX_AS_RANGES as u64 - 1) * 16 * G;
    let a = r.register(last).unwrap();
    assert_eq!(r.get(a), Some((last, 0)));
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
