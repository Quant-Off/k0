use super::*;

use std::collections::{BTreeMap, BTreeSet};

const G: u64 = 4096;

const BOOT: BootObjects = BootObjects {
    tcb: 0x7000_0000,
    addr_space_root: 0x7000_1000,
    addr_space_asid: 1,
    cnode: 0x7000_2000,
};

fn r(base: u64, size: u64) -> PhysRegion {
    PhysRegion { base, size }
}

/// 테스트 동안 주소가 고정된 슬롯 배열(해제하지 않음)
fn node(n: usize) -> *mut Slot {
    let v: Vec<Slot> = (0..n).map(|_| Slot::EMPTY).collect();
    Box::into_raw(v.into_boxed_slice()) as *mut Slot
}

fn cref(base: *mut Slot, n: usize) -> CNodeRef {
    unsafe { CNodeRef::new(base, n) }
}

fn get(base: *mut Slot, i: usize) -> Option<Entry> {
    unsafe { read(base.add(i)) }.expect("slot decodes")
}

fn cap_at(base: *mut Slot, i: usize) -> Cap {
    get(base, i).expect("slot is filled").cap
}

fn filled_n(n: usize, memory: &[PhysRegion], reserved: &[PhysRegion]) -> (*mut Slot, u64) {
    let base = node(n);
    let next = unsafe { bootstrap(&cref(base, n), &BOOT, memory, reserved) }.expect("bootstrap");
    (base, next)
}

fn filled(memory: &[PhysRegion], reserved: &[PhysRegion]) -> (*mut Slot, u64) {
    filled_n(64, memory, reserved)
}

fn untypeds(base: *mut Slot, n: usize) -> Vec<(u64, u64, u64)> {
    (1..n)
        .filter_map(|i| match get(base, i) {
            Some(Entry {
                cap: Cap::Untyped { base, size, used },
                ..
            }) => Some((base, size, used)),
            _ => None,
        })
        .collect()
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// 제거 순서만 기록하는 단순한 환경
#[derive(Default)]
struct Log {
    removed: Vec<(Cap, bool, bool)>,
    containers: BTreeMap<u64, (*mut Slot, usize)>,
}

impl Env for Log {
    fn removed(&mut self, r: &Removed) {
        assert!(unsafe { read(r.slot) }.unwrap().is_none(), "slot is emptied before the hook");
        self.removed.push((r.cap, r.original, r.bound));
    }

    fn container(&mut self, cap: &Cap) -> Option<(*mut Slot, usize)> {
        match cap {
            Cap::CNode { base, .. } | Cap::Tcb { base } => self.containers.remove(base),
            _ => None,
        }
    }
}

fn ok_prep(pa: u64) -> Option<u64> {
    // 커널의 등록부처럼 루트가 놓인 그래뉼의 번호를 줌(0은 유효한 번호가 아님)
    Some(pa / G)
}

#[test]
fn slot_is_64_bytes_and_zero_is_empty() {
    assert_eq!(size_of::<Slot>(), 64);
    let n = node(4);
    for i in 0..4 {
        assert_eq!(get(n, i), None);
    }
}

#[test]
fn every_kind_round_trips_through_the_raw_words() {
    let map = Some(Mapping {
        asid: 255,
        generation: u64::MAX,
        va: 0x0000_FFFF_FFFF_F000,
        level: 3,
    });
    let caps = [
        Cap::Untyped {
            base: 0x1801,
            size: u64::MAX - 0x1801,
            used: 77,
        },
        Cap::Frame {
            base: 0x4000_1000,
            rights: rights::R | rights::X,
            map,
        },
        Cap::Frame {
            base: 0x0000_FFFF_FFFF_F000,
            rights: rights::R,
            map: None,
        },
        Cap::PageTable {
            base: 0x4000_2000,
            map,
        },
        Cap::PageTable {
            base: 0x4000_2000,
            map: None,
        },
        Cap::Tcb { base: 0x4000_3000 },
        Cap::Endpoint {
            base: 0x4000_4000,
            rights: rights::W,
            badge: u64::MAX,
        },
        Cap::CNode {
            base: 0x4000_5000,
            slots: 256,
        },
        Cap::AddrSpace {
            root_pa: 0x4000_6000,
            asid: 200,
        },
        Cap::Console,
    ];
    for cap in caps {
        for flags in [0, FLAG_ORIGINAL, FLAG_BOUND, FLAG_ORIGINAL | FLAG_BOUND] {
            let mut w = encode(&cap);
            w[0] |= flags;
            let e = decode(&w).unwrap().unwrap();
            assert_eq!(e.cap, cap);
            assert_eq!(e.original, flags & FLAG_ORIGINAL != 0);
            assert_eq!(e.bound, flags & FLAG_BOUND != 0);
        }
    }
}

#[test]
fn malformed_words_are_rejected_not_interpreted() {
    let bad: [[u64; 4]; 14] = [
        [0, 1, 0, 0],
        [0, 0, 0, 9],
        [9, 0, 0, 0],
        [15, 0, 0, 0],
        [KIND_TCB | 1 << 11, 0, 0, 0],
        [KIND_TCB | 1 << 48, 0, 0, 0],
        [KIND_TCB | 0x1000, 5, 0, 0],
        [KIND_UNTYPED, 0, 0, 0x1000],
        [KIND_UNTYPED, 0x1000, 0x1001, 0x1000],
        [KIND_UNTYPED, 2, 0, u64::MAX],
        [KIND_FRAME | 0x1000, 0, 0, 0],
        [KIND_FRAME | 0x1000 | 1 << RIGHTS_SHIFT, 0x2000, 0, 0],
        [KIND_ENDPOINT | 0x1000 | (rights::X as u64) << RIGHTS_SHIFT, 0, 0, 0],
        [KIND_CONSOLE | 0x1000, 0, 0, 0],
    ];
    for w in bad {
        assert_eq!(decode(&w), Err(CapError::Corrupt), "{w:x?}");
    }
    // 주소 공간 번호는 그래뉼 번호라서 0과, 슬롯이 담을 수 있는 PA 밖의 값은
    // 나올 수 없음
    for asid in [0, (BASE_MASK >> 12) + 1, u64::MAX] {
        let mapped_bad_asid = [
            KIND_FRAME | 0x1000 | 1 << RIGHTS_SHIFT | FLAG_MAPPED,
            0x2000,
            asid,
            0,
        ];
        assert_eq!(decode(&mapped_bad_asid), Err(CapError::Corrupt), "{asid:#x}");
        let aspace_bad_asid = [KIND_ADDR_SPACE | 0x1000, asid, 0, 0];
        assert_eq!(decode(&aspace_bad_asid), Err(CapError::Corrupt), "{asid:#x}");
    }
}

#[test]
fn address_space_numbers_wider_than_16_bits_round_trip() {
    for asid in [1, 0xFFFF, 0x1_0000, 0x4_0000, BASE_MASK >> 12] {
        let m = Mapping {
            asid,
            generation: u64::MAX,
            va: 0x7000_0000,
            level: 3,
        };
        let frame = Cap::Frame {
            base: 0x1000,
            rights: rights::R,
            map: Some(m),
        };
        assert_eq!(decode(&encode(&frame)).unwrap().unwrap().cap, frame);
        let aspace = Cap::AddrSpace {
            root_pa: 0x1000,
            asid,
        };
        assert_eq!(decode(&encode(&aspace)).unwrap().unwrap().cap, aspace);
    }
}

#[test]
fn an_empty_slot_that_is_still_linked_is_corrupt() {
    let n = node(2);
    unsafe {
        (*n.add(1)).depth = 1;
        assert_eq!(read(n.add(1)), Err(CapError::Corrupt));
    }
}

#[test]
fn fixed_slots_come_first() {
    let (c, next) = filled(&[r(0x4000_0000, 0x10_0000)], &[]);
    assert_eq!(get(c, 0), None);
    assert_eq!(cap_at(c, 1), Cap::Tcb { base: BOOT.tcb });
    assert_eq!(
        cap_at(c, 2),
        Cap::AddrSpace {
            root_pa: BOOT.addr_space_root,
            asid: 1
        }
    );
    assert_eq!(cap_at(c, 3), Cap::Console);
    assert_eq!(
        cap_at(c, 4),
        Cap::CNode {
            base: BOOT.cnode,
            slots: 64
        }
    );
    assert_eq!(untypeds(c, 64), vec![(0x4000_0000, 0x10_0000, 0)]);
    assert_eq!(next, 6);
    for i in 1..6 {
        let e = get(c, i).unwrap();
        assert!(e.original && !e.bound);
        unsafe {
            assert!(!has_descendants(c.add(i)));
            assert_eq!(((*c.add(i)).prev, (*c.add(i)).next, (*c.add(i)).depth), (0, 0, 0));
        }
    }
}

#[test]
fn reserved_region_splits_memory() {
    let (c, _) = filled(&[r(0x1000, 0x9000)], &[r(0x4000, 0x2000)]);
    assert_eq!(untypeds(c, 64), vec![(0x1000, 0x3000, 0), (0x6000, 0x4000, 0)]);
}

#[test]
fn reserved_edges_and_full_cover() {
    let u = |m: &[PhysRegion], x: &[PhysRegion]| untypeds(filled(m, x).0, 64);
    assert_eq!(u(&[r(0x1000, 0x4000)], &[r(0x1000, 0x1000)]), vec![(0x2000, 0x3000, 0)]);
    assert_eq!(u(&[r(0x1000, 0x4000)], &[r(0x4000, 0x1000)]), vec![(0x1000, 0x3000, 0)]);
    assert_eq!(u(&[r(0x1000, 0x4000)], &[r(0x0, 0x9000)]), vec![]);
}

#[test]
fn reserved_straddling_region_start_is_clipped() {
    let (c, _) = filled(&[r(0x4000, 0x4000)], &[r(0x2000, 0x3000)]);
    assert_eq!(untypeds(c, 64), vec![(0x5000, 0x3000, 0)]);
}

#[test]
fn reserved_order_and_overlap_do_not_matter() {
    let mem = [r(0x0, 0x10000)];
    let a = untypeds(filled(&mem, &[r(0x8000, 0x1000), r(0x2000, 0x2000), r(0x3000, 0x2000)]).0, 64);
    let b = untypeds(filled(&mem, &[r(0x2000, 0x3000), r(0x8000, 0x1000)]).0, 64);
    assert_eq!(a, b);
    assert_eq!(a, vec![(0x0, 0x2000, 0), (0x5000, 0x3000, 0), (0x9000, 0x7000, 0)]);
}

#[test]
fn reserved_outside_memory_is_ignored() {
    let (c, _) = filled(&[r(0x10000, 0x1000)], &[r(0x0, 0x1000), r(0x20000, 0x1000)]);
    assert_eq!(untypeds(c, 64), vec![(0x10000, 0x1000, 0)]);
}

#[test]
fn bad_regions_are_rejected() {
    let cases: [(&[PhysRegion], &[PhysRegion]); 6] = [
        (&[r(0x1000, 0)], &[]),
        (&[r(u64::MAX - 0xFFF, 0x1000)], &[]),
        (&[r(0x0, 0x2000), r(0x1000, 0x2000)], &[]),
        (&[r(0x1000, 0x1000), r(0x0, 0x10000)], &[]),
        (&[r(0x0, 0x2000)], &[r(0x1000, 0)]),
        (&[r(0x0, 0x2000)], &[r(u64::MAX - 0xFFF, 0x1000)]),
    ];
    for (memory, reserved) in cases {
        let c = node(64);
        assert_eq!(
            unsafe { bootstrap(&cref(c, 64), &BOOT, memory, reserved) },
            Err(CapError::BadRegion)
        );
        for i in 0..64 {
            assert_eq!(get(c, i), None, "a rejected map must not leave caps behind");
        }
    }
}

#[test]
fn adjacent_memory_regions_are_accepted() {
    let (c, _) = filled(&[r(0x0, 0x1000), r(0x1000, 0x1000)], &[]);
    assert_eq!(untypeds(c, 64), vec![(0x0, 0x1000, 0), (0x1000, 0x1000, 0)]);
}

#[test]
fn too_many_untypeds_run_out_of_slots() {
    let holes: Vec<PhysRegion> = (0..40).map(|i| r(i * 2 * G + G, G)).collect();
    let c = node(32);
    assert_eq!(
        unsafe { bootstrap(&cref(c, 32), &BOOT, &[r(0, 80 * G)], &holes) },
        Err(CapError::OutOfSlots)
    );
}

#[test]
fn a_cnode_too_small_for_the_fixed_slots_is_rejected() {
    let c = node(4);
    assert_eq!(
        unsafe { bootstrap(&cref(c, 4), &BOOT, &[r(0, G)], &[]) },
        Err(CapError::OutOfSlots)
    );
}

#[test]
fn untypeds_cover_exactly_memory_minus_reserved() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..2000 {
        let mut memory = Vec::new();
        let mut cursor = rng.below(16);
        for _ in 0..1 + rng.below(3) {
            let size = 1 + rng.below(64);
            memory.push(r(cursor, size));
            cursor += size + rng.below(16);
        }
        let reserved: Vec<PhysRegion> = (0..rng.below(6))
            .map(|_| r(rng.below(cursor + 8), 1 + rng.below(24)))
            .collect();

        let c = node(32);
        match unsafe { bootstrap(&cref(c, 32), &BOOT, &memory, &reserved) } {
            Ok(_) => {}
            Err(CapError::OutOfSlots) => continue,
            Err(e) => panic!("unexpected {e:?} for {memory:?} {reserved:?}"),
        }
        let u = untypeds(c, 32);
        for p in 0..cursor + 32 {
            let in_mem = memory.iter().any(|m| p >= m.base && p < m.end());
            let in_rsv = reserved.iter().any(|x| p >= x.base && p < x.end());
            let hits = u.iter().filter(|&&(b, s, _)| p >= b && p < b + s).count();
            assert_eq!(hits, usize::from(in_mem && !in_rsv), "point {p} in {memory:?} {reserved:?} -> {u:?}");
        }
        assert!(u.iter().all(|&(_, s, used)| s > 0 && used == 0));
    }
}

#[test]
fn slot_lookup_rejects_null_and_out_of_range() {
    let c = cref(node(8), 8);
    assert!(c.slot(0).is_none());
    assert!(c.slot(8).is_none());
    assert!(c.slot(u64::MAX).is_none());
    assert!(c.slot(1).is_some());
    assert!(c.slot(7).is_some());
    assert_eq!(c.len(), 8);
}

#[test]
fn retype_carves_aligned_objects_in_order() {
    let (c, next) = filled(&[r(0x4000_0000, 8 * G)], &[]);
    let ut = unsafe { c.add(5) };
    let mut seen = Vec::new();
    let kinds = [
        ObjKind::Frame,
        ObjKind::PageTable,
        ObjKind::Tcb,
        ObjKind::Endpoint,
        ObjKind::CNode,
        ObjKind::AddrSpace,
    ];
    for (i, kind) in kinds.into_iter().enumerate() {
        let dest = next as usize + i;
        unsafe {
            retype(ut, c.add(dest), kind, G, |pa| {
                seen.push(pa);
                Some(42)
            })
        }
        .expect("retype");
        let pa = *seen.last().unwrap();
        let e = get(c, dest).unwrap();
        assert!(e.original && !e.bound);
        let want = match kind {
            ObjKind::Frame => Cap::Frame {
                base: pa,
                rights: rights::R | rights::W,
                map: None,
            },
            ObjKind::PageTable => Cap::PageTable { base: pa, map: None },
            ObjKind::Tcb => Cap::Tcb { base: pa },
            ObjKind::Endpoint => Cap::Endpoint {
                base: pa,
                rights: rights::R | rights::W,
                badge: 0,
            },
            ObjKind::CNode => Cap::CNode { base: pa, slots: 64 },
            ObjKind::AddrSpace => Cap::AddrSpace {
                root_pa: pa,
                asid: 42,
            },
        };
        assert_eq!(e.cap, want);
    }
    assert_eq!(
        seen,
        (0..6).map(|i| 0x4000_0000 + i * G).collect::<Vec<_>>()
    );
    assert_eq!(untypeds(c, 64), vec![(0x4000_0000, 8 * G, 6 * G)]);
    assert!(unsafe { has_descendants(ut) });
}

#[test]
fn retyped_frames_never_carry_execute() {
    let (c, next) = filled(&[r(0x4000_0000, 8 * G)], &[]);
    unsafe { retype(c.add(5), c.add(next as usize), ObjKind::Frame, G, ok_prep) }.unwrap();
    let Cap::Frame { rights: got, .. } = cap_at(c, next as usize) else {
        panic!()
    };
    assert_eq!(got & rights::X, 0);
    // 복사로도 권한은 늘지 않음
    unsafe { copy(c.add(next as usize), c.add(40), rights::ALL, 0, false) }.unwrap();
    let Cap::Frame { rights: got, .. } = cap_at(c, 40) else {
        panic!()
    };
    assert_eq!(got, rights::R | rights::W);
}

#[test]
fn retype_rounds_unaligned_watermark_up() {
    let (c, next) = filled(&[r(0x1800, 0x4000)], &[]);
    let n = next as usize;
    let ut = unsafe { c.add(5) };
    let mut at = 0;
    unsafe {
        retype(ut, c.add(n), ObjKind::Frame, G, |pa| {
            at = pa;
            Some(0)
        })
    }
    .unwrap();
    assert_eq!(at, 0x2000);
    assert_eq!(untypeds(c, 64), vec![(0x1800, 0x4000, 0x1800)]);
    let at = |want| move |pa| (pa == want).then_some(0);
    assert_eq!(unsafe { retype(ut, c.add(n + 1), ObjKind::Frame, G, at(0x3000)) }, Ok(()));
    assert_eq!(unsafe { retype(ut, c.add(n + 2), ObjKind::Frame, G, at(0x4000)) }, Ok(()));
    assert_eq!(
        unsafe { retype(ut, c.add(n + 3), ObjKind::Frame, G, |_| panic!("prep must not run")) },
        Err(CapError::Exhausted)
    );
    assert_eq!(untypeds(c, 64), vec![(0x1800, 0x4000, 0x3800)]);
    assert_eq!(get(c, n + 3), None);
}

#[test]
fn failed_prep_leaves_state_unchanged() {
    let (c, next) = filled(&[r(0x10000, 4 * G)], &[]);
    let n = next as usize;
    let ut = unsafe { c.add(5) };
    assert_eq!(
        unsafe { retype(ut, c.add(n), ObjKind::Tcb, G, |_| None) },
        Err(CapError::PrepFailed)
    );
    assert_eq!(get(c, n), None);
    assert_eq!(untypeds(c, 64), vec![(0x10000, 4 * G, 0)]);
    assert!(!unsafe { has_descendants(ut) });
    let mut at = 0;
    unsafe {
        retype(ut, c.add(n), ObjKind::Tcb, G, |pa| {
            at = pa;
            Some(0)
        })
    }
    .unwrap();
    assert_eq!(at, 0x10000);
}

#[test]
fn retype_rejects_bad_sources_and_destinations_without_prep() {
    let (c, next) = filled(&[r(0x10000, 4 * G)], &[]);
    let n = next as usize;
    unsafe { retype(c.add(5), c.add(n), ObjKind::Frame, G, ok_prep) }.unwrap();
    let never = |_| -> Option<u64> { panic!("prep must not run") };
    for src in [1, 2, 3, 4, n, n + 1] {
        assert_eq!(
            unsafe { retype(c.add(src), c.add(n + 2), ObjKind::Frame, G, never) },
            Err(CapError::NotUntyped),
            "source {src}"
        );
    }
    for dest in [1, 5, n] {
        assert_eq!(
            unsafe { retype(c.add(5), c.add(dest), ObjKind::Frame, G, never) },
            Err(CapError::Occupied),
            "dest {dest}"
        );
    }
    assert_eq!(untypeds(c, 64), vec![(0x10000, 4 * G, G)]);
}

#[test]
fn retype_near_address_space_top_does_not_overflow() {
    let c = node(8);
    unsafe {
        insert_root(
            c.add(1),
            Cap::Untyped {
                base: u64::MAX - G + 2,
                size: G - 2,
                used: 0,
            },
        )
        .unwrap();
        assert_eq!(
            retype(c.add(1), c.add(2), ObjKind::Frame, G, |_| panic!("prep must not run")),
            Err(CapError::Exhausted)
        );
    }
}

#[test]
fn objects_cannot_be_carved_beyond_the_encodable_physical_range() {
    let c = node(8);
    unsafe {
        insert_root(
            c.add(1),
            Cap::Untyped {
                base: 1 << 48,
                size: 16 * G,
                used: 0,
            },
        )
        .unwrap();
        assert_eq!(
            retype(c.add(1), c.add(2), ObjKind::Frame, G, |_| panic!("prep must not run")),
            Err(CapError::Exhausted)
        );
        assert_eq!(get(c, 2), None);
    }
}

#[test]
fn random_retypes_never_overlap() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for granule in [4096u64, 16384] {
        for _ in 0..200 {
            let memory: Vec<PhysRegion> = (0..1 + rng.below(3))
                .map(|i| r(i * 0x100_0000 + rng.below(4) * 0x800, granule * (1 + rng.below(12))))
                .collect();
            let (c, next) = filled(&memory, &[]);
            let sources: Vec<usize> = (5..next as usize).collect();
            let mut dest = next as usize;
            let mut carved: Vec<u64> = Vec::new();
            for _ in 0..40 {
                let src = sources[rng.below(sources.len() as u64) as usize];
                let Cap::Untyped { base, size, used } = cap_at(c, src) else {
                    unreachable!()
                };
                let mut at = None;
                match unsafe {
                    retype(c.add(src), c.add(dest), ObjKind::Frame, granule, |pa| {
                        at = Some(pa);
                        Some(0)
                    })
                } {
                    Ok(()) => {
                        let pa = at.unwrap();
                        assert_eq!(pa % granule, 0);
                        assert!(pa >= base + used && pa + granule <= base + size);
                        assert!(carved.iter().all(|&o| o + granule <= pa || pa + granule <= o));
                        carved.push(pa);
                        dest += 1;
                    }
                    Err(CapError::Exhausted) => assert!(at.is_none()),
                    Err(e) => panic!("unexpected {e:?}"),
                }
            }
        }
    }
}

#[test]
fn copy_rules_per_kind() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    let ut = unsafe { c.add(5) };
    unsafe {
        retype(ut, c.add(n), ObjKind::Frame, G, ok_prep).unwrap();
        retype(ut, c.add(n + 1), ObjKind::PageTable, G, ok_prep).unwrap();
        retype(ut, c.add(n + 2), ObjKind::Endpoint, G, ok_prep).unwrap();
        retype(ut, c.add(n + 3), ObjKind::Tcb, G, ok_prep).unwrap();

        // untyped와 페이지 테이블은 복사 불가
        assert_eq!(copy(ut, c.add(30), rights::ALL, 0, false), Err(CapError::NotCopyable));
        assert_eq!(copy(c.add(n + 1), c.add(30), rights::ALL, 0, false), Err(CapError::NotCopyable));
        // 빈 원본, 점유된 목적지, 자기 자신으로의 복사
        assert_eq!(copy(c.add(50), c.add(30), rights::ALL, 0, false), Err(CapError::Empty));
        assert_eq!(copy(c.add(n), c.add(n + 2), rights::ALL, 0, false), Err(CapError::Occupied));
        assert_eq!(copy(c.add(n), c.add(n), rights::ALL, 0, false), Err(CapError::Occupied));
        // 권한은 줄기만 하고, 다 사라지는 복사는 거부
        assert_eq!(copy(c.add(n), c.add(30), rights::X, 0, false), Err(CapError::NoRights));
        copy(c.add(n), c.add(30), rights::R, 0, false).unwrap();
        assert!(matches!(cap_at(c, 30), Cap::Frame { rights: 1, map: None, .. }));
        assert_eq!(copy(c.add(30), c.add(31), rights::W, 0, false), Err(CapError::NoRights));
        copy(c.add(30), c.add(31), rights::ALL, 0, false).unwrap();
        assert!(matches!(cap_at(c, 31), Cap::Frame { rights: 1, .. }));
        // 배지는 엔드포인트에만, 한 번만
        assert_eq!(copy(c.add(n), c.add(32), rights::ALL, 7, false), Err(CapError::BadBadge));
        assert_eq!(copy(c.add(n + 3), c.add(32), rights::ALL, 7, false), Err(CapError::BadBadge));
        copy(c.add(n + 2), c.add(32), rights::W, 7, false).unwrap();
        assert_eq!(
            cap_at(c, 32),
            Cap::Endpoint {
                base: 0x4000_2000,
                rights: rights::W,
                badge: 7
            }
        );
        assert_eq!(copy(c.add(32), c.add(33), rights::ALL, 8, false), Err(CapError::BadBadge));
        copy(c.add(32), c.add(33), rights::ALL, 0, false).unwrap();
        copy(c.add(32), c.add(34), rights::ALL, 7, false).unwrap();
        for i in [33, 34] {
            assert!(matches!(cap_at(c, i), Cap::Endpoint { rights: 2, badge: 7, .. }));
        }
        assert_eq!(copy(c.add(32), c.add(35), rights::R, 0, false), Err(CapError::NoRights));
        // 복사본은 원본이 아니고, 실패한 복사는 흔적을 남기지 않음
        for i in [30, 31, 32, 33, 34] {
            assert!(!get(c, i).unwrap().original);
        }
        assert_eq!(get(c, 35), None);
    }
}

#[test]
fn a_copy_never_inherits_the_mapping_record() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    let m = Some(Mapping {
        asid: 3,
        generation: 9,
        va: 0x20_0000,
        level: 3,
    });
    unsafe {
        retype(c.add(5), c.add(n), ObjKind::Frame, G, ok_prep).unwrap();
        update(
            c.add(n),
            Cap::Frame {
                base: 0x4000_0000,
                rights: rights::R | rights::W,
                map: m,
            },
        )
        .unwrap();
        assert!(get(c, n).unwrap().original);
        copy(c.add(n), c.add(n + 1), rights::ALL, 0, false).unwrap();
        assert!(matches!(cap_at(c, n + 1), Cap::Frame { map: None, .. }));
        assert!(matches!(cap_at(c, n), Cap::Frame { map: Some(_), .. }));
        // 이동은 기록과 원본 표시를 그대로 옮김
        move_cap(c.add(n), c.add(n + 2)).unwrap();
        assert_eq!(get(c, n), None);
        let e = get(c, n + 2).unwrap();
        assert!(e.original);
        assert!(matches!(e.cap, Cap::Frame { map, .. } if map == m));
    }
}

#[test]
fn update_only_changes_watermark_and_mapping() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    unsafe {
        retype(c.add(5), c.add(n), ObjKind::Frame, G, ok_prep).unwrap();
        retype(c.add(5), c.add(n + 1), ObjKind::Endpoint, G, ok_prep).unwrap();
        let frame = |base, rights| Cap::Frame {
            base,
            rights,
            map: None,
        };
        assert_eq!(update(c.add(n), frame(0x4000_1000, 3)), Err(CapError::Corrupt));
        assert_eq!(update(c.add(n), frame(0x4000_0000, 7)), Err(CapError::Corrupt));
        assert_eq!(update(c.add(n), Cap::Tcb { base: 0x4000_0000 }), Err(CapError::Corrupt));
        assert_eq!(
            update(
                c.add(n + 1),
                Cap::Endpoint {
                    base: 0x4000_1000,
                    rights: 3,
                    badge: 1
                }
            ),
            Err(CapError::Corrupt)
        );
        assert_eq!(update(c.add(50), frame(0, 1)), Err(CapError::Empty));
        let ut = |used| Cap::Untyped {
            base: 0x4000_0000,
            size: 16 * G,
            used,
        };
        assert_eq!(update(c.add(5), ut(17 * G)), Err(CapError::Corrupt));
        update(c.add(5), ut(3 * G)).unwrap();
        assert_eq!(untypeds(c, 64), vec![(0x4000_0000, 16 * G, 3 * G)]);
    }
}

#[test]
fn move_keeps_the_subtree_attached() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    let other = node(16);
    unsafe {
        retype(c.add(5), c.add(n), ObjKind::Endpoint, G, ok_prep).unwrap();
        copy(c.add(n), c.add(20), rights::ALL, 1, false).unwrap();
        copy(c.add(20), c.add(21), rights::ALL, 0, false).unwrap();
        // 가운데 노드를 다른 CNode로 옮김
        move_cap(c.add(20), other.add(3)).unwrap();
        assert_eq!(move_cap(c.add(20), other.add(4)), Err(CapError::Empty));
        assert_eq!(move_cap(c.add(n), other.add(3)), Err(CapError::Occupied));
        // untyped도 옮길 수는 있고, 옮긴 뒤에도 자손이 붙어 있음
        move_cap(c.add(5), other.add(5)).unwrap();
        assert!(has_descendants(other.add(5)));

        let mut log = Log::default();
        revoke(other.add(3), &mut log).unwrap();
        assert_eq!(log.removed.len(), 1);
        assert_eq!(get(c, 21), None);
        assert!(get(other, 3).is_some());

        revoke(other.add(5), &mut log).unwrap();
        assert_eq!(get(c, n), None);
        assert_eq!(get(other, 3), None);
        assert!(!has_descendants(other.add(5)));
        assert_eq!(log.removed.iter().filter(|x| x.1).count(), 1, "one object finalized");
    }
}

#[test]
fn delete_removes_descendants_and_finalizes_only_with_the_original() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    unsafe {
        retype(c.add(5), c.add(n), ObjKind::Endpoint, G, ok_prep).unwrap();
        copy(c.add(n), c.add(20), rights::ALL, 0, false).unwrap();
        copy(c.add(20), c.add(21), rights::ALL, 0, false).unwrap();
        copy(c.add(n), c.add(22), rights::ALL, 0, false).unwrap();

        let mut log = Log::default();
        // 복사본 삭제: 그 자손만 함께 사라지고 오브젝트는 살아 있음
        delete(c.add(20), &mut log).unwrap();
        assert_eq!(log.removed.len(), 2);
        assert!(log.removed.iter().all(|x| !x.1));
        assert_eq!(get(c, 20), None);
        assert_eq!(get(c, 21), None);
        assert!(get(c, 22).is_some());
        assert!(get(c, n).is_some());

        // 빈 슬롯 삭제와 회수는 아무 일도 하지 않음
        delete(c.add(20), &mut log).unwrap();
        revoke(c.add(20), &mut log).unwrap();
        assert_eq!(log.removed.len(), 2);

        // 원본 삭제: 남은 복사본이 먼저 사라지고 마지막에 원본이 제거됨
        delete(c.add(n), &mut log).unwrap();
        assert_eq!(log.removed.len(), 4);
        assert!(!log.removed[2].1);
        assert!(log.removed[3].1, "the original is the last cap of its object");
        assert_eq!(get(c, 22), None);
        assert!(!has_descendants(c.add(5)));
    }
}

#[test]
fn revoking_an_ancestor_may_report_the_original_before_its_copies() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    let mut log = Log::default();
    unsafe {
        retype(c.add(5), c.add(n), ObjKind::Endpoint, G, ok_prep).unwrap();
        copy(c.add(n), c.add(20), rights::ALL, 3, false).unwrap();
        copy(c.add(20), c.add(21), rights::ALL, 0, false).unwrap();
        // 조상(untyped)의 회수는 리스트 순서대로 지움: 원본이 먼저, 복사본이 나중.
        // 콜백 구현은 이 순서를 견뎌야 하고, 반환 시점에는 전부 사라져 있어야 함
        revoke(c.add(5), &mut log).unwrap();
    }
    let order: Vec<bool> = log.removed.iter().map(|x| x.1).collect();
    assert_eq!(order, vec![true, false, false]);
    for i in [n, 20, 21] {
        assert_eq!(get(c, i), None);
    }
    assert!(!unsafe { has_descendants(c.add(5)) });
}

#[test]
fn untyped_memory_is_reused_only_when_nothing_is_left() {
    let (c, next) = filled(&[r(0x4000_0000, 4 * G)], &[]);
    let n = next as usize;
    let ut = unsafe { c.add(5) };
    let mut log = Log::default();
    unsafe {
        for i in 0..4 {
            retype(ut, c.add(n + i), ObjKind::Frame, G, ok_prep).unwrap();
        }
        assert_eq!(retype(ut, c.add(40), ObjKind::Frame, G, ok_prep), Err(CapError::Exhausted));

        // 하나라도 살아 있으면 워터마크는 그대로
        for i in 0..3 {
            delete(c.add(n + i), &mut log).unwrap();
        }
        assert_eq!(retype(ut, c.add(40), ObjKind::Frame, G, ok_prep), Err(CapError::Exhausted));
        assert_eq!(untypeds(c, 64), vec![(0x4000_0000, 4 * G, 4 * G)]);

        // 전부 사라지면 처음부터 다시 잘라냄
        delete(c.add(n + 3), &mut log).unwrap();
        let mut at = 0;
        retype(ut, c.add(40), ObjKind::Tcb, G, |pa| {
            at = pa;
            Some(0)
        })
        .unwrap();
        assert_eq!(at, 0x4000_0000);
        assert_eq!(untypeds(c, 64), vec![(0x4000_0000, 4 * G, G)]);

        // 회수도 같은 효과
        retype(ut, c.add(41), ObjKind::Frame, G, ok_prep).unwrap();
        revoke(ut, &mut log).unwrap();
        assert_eq!(get(c, 40), None);
        assert_eq!(get(c, 41), None);
        retype(ut, c.add(41), ObjKind::Frame, G, |pa| (pa == 0x4000_0000).then_some(0)).unwrap();
    }
}

#[test]
fn destroying_a_cnode_deletes_what_it_holds() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G), r(0x5000_0000, 16 * G)], &[]);
    let n = next as usize;
    let inner = node(64);
    let mut log = Log::default();
    unsafe {
        // 첫 untyped에서 CNode, 둘째 untyped에서 엔드포인트
        retype(c.add(5), c.add(n), ObjKind::CNode, G, ok_prep).unwrap();
        retype(c.add(6), c.add(n + 1), ObjKind::Endpoint, G, ok_prep).unwrap();
        log.containers.insert(0x4000_0000, (inner, 64));
        // 엔드포인트의 원본을 CNode 안으로 옮기고 바깥에 복사본을 둠
        move_cap(c.add(n + 1), inner.add(9)).unwrap();
        copy(inner.add(9), c.add(30), rights::ALL, 5, false).unwrap();
        // CNode 안에 다른 것의 복사본도 넣음
        copy(c.add(3), inner.add(10), rights::ALL, 0, false).unwrap();

        // 첫 untyped만 회수: CNode가 종료되면서 안에 든 케이퍼빌리티가 전부
        // 삭제되고, 그중 원본이던 엔드포인트도 종료되어 바깥 복사본이 사라짐
        revoke(c.add(5), &mut log).unwrap();
        assert_eq!(get(c, n), None);
        assert_eq!(get(c, 30), None);
        for i in 0..64 {
            assert_eq!(get(inner, i), None);
            assert_eq!(((*inner.add(i)).next, (*inner.add(i)).aux), (0, 0));
        }
        assert!(get(c, 3).is_some(), "the console original outside survives");
        assert!(!has_descendants(c.add(3)));
        assert!(!has_descendants(c.add(6)), "the endpoint in the other untyped is gone");
        let originals: Vec<Cap> = log.removed.iter().filter(|x| x.1).map(|x| x.0).collect();
        assert_eq!(originals.len(), 2);
        assert!(matches!(originals[0], Cap::CNode { .. }));
        assert!(matches!(originals[1], Cap::Endpoint { .. }));
    }
}

#[test]
fn bound_copies_are_reported_and_cannot_move() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    let tcb = node(3);
    let mut log = Log::default();
    unsafe {
        retype(c.add(5), c.add(n), ObjKind::CNode, G, ok_prep).unwrap();
        copy(c.add(n), tcb.add(1), rights::ALL, 0, true).unwrap();
        assert!(get(tcb, 1).unwrap().bound);
        assert_eq!(move_cap(tcb.add(1), c.add(40)), Err(CapError::NotCopyable));
        // 묶인 케이퍼빌리티의 복사본은 묶이지 않음
        copy(tcb.add(1), c.add(40), rights::ALL, 0, false).unwrap();
        assert!(!get(c, 40).unwrap().bound);

        log.containers.insert(0x4000_0000, (node(64), 64));
        revoke(c.add(n), &mut log).unwrap();
        assert_eq!(log.removed.len(), 2);
        assert!(log.removed.iter().any(|x| x.2), "the bound copy is flagged for its TCB");
        assert_eq!(get(tcb, 1), None);
    }
}

#[test]
fn a_pair_copy_is_all_or_nothing() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    let tcb = node(3);
    unsafe {
        retype(c.add(5), c.add(n), ObjKind::CNode, G, ok_prep).unwrap();
        retype(c.add(5), c.add(n + 1), ObjKind::AddrSpace, G, ok_prep).unwrap();
        retype(c.add(5), c.add(n + 2), ObjKind::PageTable, G, ok_prep).unwrap();

        // 둘째가 복사 불가, 비어 있음, 깊이 초과인 경우 첫째도 들어가지 않음
        assert_eq!(
            copy_pair_bound(c.add(n), tcb.add(1), c.add(n + 2), tcb.add(2)),
            Err(CapError::NotCopyable)
        );
        assert_eq!(
            copy_pair_bound(c.add(n), tcb.add(1), c.add(50), tcb.add(2)),
            Err(CapError::Empty)
        );
        (*c.add(n + 1)).depth = MAX_DEPTH;
        assert_eq!(
            copy_pair_bound(c.add(n), tcb.add(1), c.add(n + 1), tcb.add(2)),
            Err(CapError::TooDeep)
        );
        (*c.add(n + 1)).depth = 1;
        // 겹치는 목적지
        assert_eq!(
            copy_pair_bound(c.add(n), tcb.add(1), c.add(n + 1), tcb.add(1)),
            Err(CapError::Occupied)
        );
        for i in 0..3 {
            assert_eq!(get(tcb, i), None);
        }
        assert!(!has_descendants(c.add(n)));
        assert!(!has_descendants(c.add(n + 1)));

        copy_pair_bound(c.add(n), tcb.add(1), c.add(n + 1), tcb.add(2)).unwrap();
        for i in [1, 2] {
            let e = get(tcb, i).unwrap();
            assert!(e.bound && !e.original);
        }
        assert!(matches!(cap_at(tcb, 1), Cap::CNode { .. }));
        assert!(matches!(cap_at(tcb, 2), Cap::AddrSpace { .. }));
        // 이미 채워진 내장 슬롯에 다시 구성할 수 없음
        assert_eq!(
            copy_pair_bound(c.add(n), tcb.add(1), c.add(n + 1), tcb.add(2)),
            Err(CapError::Occupied)
        );
    }
}

#[test]
fn derivation_depth_is_bounded() {
    let (c, _) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    unsafe {
        (*c.add(3)).depth = MAX_DEPTH;
        assert_eq!(copy(c.add(3), c.add(20), rights::ALL, 0, false), Err(CapError::TooDeep));
        assert_eq!(get(c, 20), None);
        (*c.add(5)).depth = MAX_DEPTH;
        assert_eq!(
            retype(c.add(5), c.add(20), ObjKind::Frame, G, |_| panic!("prep must not run")),
            Err(CapError::TooDeep)
        );
        (*c.add(3)).depth = u64::MAX;
        assert_eq!(copy(c.add(3), c.add(20), rights::ALL, 0, false), Err(CapError::TooDeep));
    }
}

#[test]
fn broken_links_stop_the_operation() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    let mut log = Log::default();
    unsafe {
        retype(c.add(5), c.add(n), ObjKind::Endpoint, G, ok_prep).unwrap();
        copy(c.add(n), c.add(20), rights::ALL, 0, false).unwrap();
        // 자식의 역링크를 망가뜨림
        (*c.add(20)).prev = c.add(3) as u64;
        assert_eq!(revoke(c.add(n), &mut log), Err(CapError::Corrupt));
        assert_eq!(move_cap(c.add(20), c.add(21)), Err(CapError::Corrupt));
        assert!(log.removed.is_empty());
        assert!(get(c, 20).is_some());
    }
}

#[test]
fn a_container_whose_null_slot_is_used_is_refused() {
    let (c, next) = filled(&[r(0x4000_0000, 16 * G)], &[]);
    let n = next as usize;
    let inner = node(8);
    let mut log = Log::default();
    unsafe {
        retype(c.add(5), c.add(n), ObjKind::CNode, G, ok_prep).unwrap();
        (*inner).aux = 1;
        log.containers.insert(0x4000_0000, (inner, 8));
        assert_eq!(delete(c.add(n), &mut log), Err(CapError::Corrupt));
    }
}

fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    // 커널 스택(64 KiB)보다 넉넉하지 않은 스택에서 돌려, 깊이에 비례하는
    // 스택 사용이 생기면 넘치게 함
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .expect("ran within a small fixed stack");
}

#[test]
fn a_very_deep_derivation_chain_is_deleted_iteratively() {
    on_small_stack(|| {
        const N: usize = 60_000;
        let c = node(N + 8);
        let mut log = Log::default();
        unsafe {
            insert_root(c.add(1), Cap::Console).unwrap();
            for i in 1..N {
                copy(c.add(i), c.add(i + 1), rights::ALL, 0, false).unwrap();
            }
            assert_eq!((*c.add(N)).depth, N as u64 - 1);
            delete(c.add(1), &mut log).unwrap();
        }
        assert_eq!(log.removed.len(), N);
        assert!(log.removed.last().unwrap().1);
        assert_eq!(get(c, N), None);
    });
}

#[test]
fn deeply_nested_cnodes_are_destroyed_iteratively() {
    on_small_stack(|| {
        const N: usize = 3_000;
        let root = node(8);
        let mut log = Log::default();
        unsafe {
            insert_root(
                root.add(1),
                Cap::Untyped {
                    base: 0x4000_0000,
                    size: (N as u64 + 1) * G,
                    used: 0,
                },
            )
            .unwrap();
            // 각 CNode의 원본을 직전 CNode 안에 넣어 N겹으로 중첩
            let mut holder = root;
            for i in 0..N {
                let pa = 0x4000_0000 + i as u64 * G;
                let inner = node(4);
                retype(root.add(1), holder.add(2), ObjKind::CNode, G, |p| (p == pa).then_some(0)).unwrap();
                log.containers.insert(pa, (inner, 4));
                holder = inner;
            }
            // 가장 바깥 원본만 지워도 전부 연쇄 종료됨. untyped를 회수하지 않으므로
            // 정리는 오로지 컨테이너 비우기로만 진행됨
            delete(root.add(2), &mut log).unwrap();
            assert!(!has_descendants(root.add(1)));
        }
        assert_eq!(log.removed.len(), N);
        assert!(log.removed.iter().all(|x| x.1));
        assert!(log.containers.is_empty());
    });
}

// 무작위 연산열에 대한 모델 검사

const ROOT_N: usize = 48;
const TCB_N: usize = 3;

fn ident(cap: &Cap) -> (u8, u64) {
    match *cap {
        Cap::Untyped { base, .. } => (0, base),
        Cap::Console => (1, 0),
        Cap::Frame { base, .. }
        | Cap::PageTable { base, .. }
        | Cap::Tcb { base }
        | Cap::Endpoint { base, .. }
        | Cap::CNode { base, .. } => (2, base),
        Cap::AddrSpace { root_pa, .. } => (2, root_pa),
    }
}

fn cap_rights(cap: &Cap) -> u8 {
    match *cap {
        Cap::Frame { rights, .. } | Cap::Endpoint { rights, .. } => rights,
        _ => 0,
    }
}

struct World {
    granule: u64,
    /// 살아 있는 슬롯 배열: 오브젝트 PA -> (선두, 슬롯 수)
    containers: BTreeMap<u64, (*mut Slot, usize)>,
    /// 종료된 슬롯 배열, 끝까지 비어 있어야 함
    graveyard: Vec<(*mut Slot, usize)>,
    /// 살아 있는 오브젝트
    objs: BTreeSet<(u8, u64)>,
    /// 묶인 케이퍼빌리티가 제거된 TCB(슬롯 배열 선두 주소)
    torn: BTreeSet<u64>,
    finalized: u64,
    /// 한 번이라도 오브젝트였던 PA와, 그런 자리를 다시 잘라낸 횟수
    ever: BTreeSet<u64>,
    reused: u64,
}

impl Env for World {
    fn removed(&mut self, r: &Removed) {
        // 콜백 시점의 불변식: 슬롯은 이미 리스트에서 빠져 완전히 비어 있음
        unsafe {
            assert!(read(r.slot).unwrap().is_none());
            assert_eq!(((*r.slot).prev, (*r.slot).next, (*r.slot).depth), (0, 0, 0));
        }
        if r.bound {
            let owner = self
                .containers
                .values()
                .chain(self.graveyard.iter())
                .find(|(b, n)| {
                    let a = r.slot as u64;
                    a >= *b as u64 && a < *b as u64 + (*n * SLOT_SIZE) as u64
                })
                .expect("bound slot belongs to a known TCB");
            assert_eq!(owner.1, TCB_N);
            self.torn.insert(owner.0 as u64);
        }
        if r.original {
            assert!(self.objs.remove(&ident(&r.cap)), "object finalized twice: {:?}", r.cap);
            self.finalized += 1;
        }
    }

    fn container(&mut self, cap: &Cap) -> Option<(*mut Slot, usize)> {
        match cap {
            Cap::CNode { base, .. } | Cap::Tcb { base } => {
                let c = self.containers.remove(base).expect("container exists exactly once");
                self.graveyard.push(c);
                Some(c)
            }
            _ => None,
        }
    }
}

impl World {
    fn new(granule: u64, rng: &mut Rng) -> Self {
        let root = node(ROOT_N);
        let memory = [
            r(0x4000_0000 + rng.below(3) * 0x800, granule * (4 + rng.below(8))),
            r(0x5000_0000, granule * (2 + rng.below(6))),
        ];
        unsafe { bootstrap(&cref(root, ROOT_N), &BOOT, &memory, &[]) }.unwrap();
        let mut w = World {
            granule,
            containers: BTreeMap::new(),
            graveyard: Vec::new(),
            objs: BTreeSet::new(),
            torn: BTreeSet::new(),
            finalized: 0,
            ever: BTreeSet::new(),
            reused: 0,
        };
        w.containers.insert(BOOT.cnode, (root, ROOT_N));
        // 루트 TCB도 슬롯을 담는 오브젝트
        w.containers.insert(BOOT.tcb, (node(TCB_N), TCB_N));
        for i in 1..ROOT_N {
            if let Some(e) = get(root, i) {
                w.objs.insert(ident(&e.cap));
            }
        }
        w
    }

    /// 살아 있는 CNode 중 하나의 임의 슬롯(0번 제외)
    fn any_slot(&self, rng: &mut Rng) -> *mut Slot {
        let cnodes: Vec<&(*mut Slot, usize)> =
            self.containers.values().filter(|c| c.1 != TCB_N).collect();
        if cnodes.is_empty() {
            return std::ptr::null_mut();
        }
        let (base, n) = *cnodes[rng.below(cnodes.len() as u64) as usize];
        unsafe { base.add(1 + rng.below(n as u64 - 1) as usize) }
    }

    /// 조건에 맞는 CNode 슬롯 하나, 대부분은 조건을 지키고 가끔은 아무 슬롯이나
    /// 골라 거부 경로도 밟게 함
    fn pick(&self, rng: &mut Rng, want: impl Fn(Option<Entry>) -> bool) -> *mut Slot {
        if rng.below(8) == 0 {
            return self.any_slot(rng);
        }
        let mut c = Vec::new();
        for &(base, n) in self.containers.values().filter(|c| c.1 != TCB_N) {
            for i in 1..n {
                let s = unsafe { base.add(i) };
                if want(unsafe { read(s) }.unwrap()) {
                    c.push(s);
                }
            }
        }
        if c.is_empty() {
            self.any_slot(rng)
        } else {
            c[rng.below(c.len() as u64) as usize]
        }
    }

    fn any_tcb_slot(&self, rng: &mut Rng) -> *mut Slot {
        let tcbs: Vec<&(*mut Slot, usize)> =
            self.containers.values().filter(|c| c.1 == TCB_N).collect();
        if tcbs.is_empty() {
            return std::ptr::null_mut();
        }
        let (base, _) = *tcbs[rng.below(tcbs.len() as u64) as usize];
        unsafe { base.add(1 + rng.below(2) as usize) }
    }

    fn step(&mut self, rng: &mut Rng) {
        let filled = |e: Option<Entry>| e.is_some();
        let empty = |e: Option<Entry>| e.is_none();
        let untyped = |e: Option<Entry>| matches!(e, Some(Entry { cap: Cap::Untyped { .. }, .. }));
        match rng.below(100) {
            0..=39 => {
                let (a, b) = (self.pick(rng, untyped), self.pick(rng, empty));
                if a.is_null() || b.is_null() {
                    return;
                }
                let kind = [
                    ObjKind::Frame,
                    ObjKind::PageTable,
                    ObjKind::Tcb,
                    ObjKind::Endpoint,
                    ObjKind::CNode,
                    ObjKind::AddrSpace,
                ][rng.below(6) as usize];
                let g = self.granule;
                let mut at = None;
                let objs = &self.objs;
                let res = unsafe {
                    retype(a, b, kind, g, |pa| {
                        // 재사용 안전성: 잘라낸 자리에 살아 있는 오브젝트가 없어야 함
                        assert!(!objs.contains(&(2, pa)), "retype handed out a live object's memory");
                        at = Some(pa);
                        Some(7)
                    })
                };
                if res.is_ok() {
                    let pa = at.unwrap();
                    self.objs.insert((2, pa));
                    if !self.ever.insert(pa) {
                        self.reused += 1;
                    }
                    match kind {
                        ObjKind::CNode => {
                            let n = (g / SLOT_SIZE as u64) as usize;
                            self.containers.insert(pa, (node(n), n));
                        }
                        ObjKind::Tcb => {
                            self.containers.insert(pa, (node(TCB_N), TCB_N));
                        }
                        _ => {}
                    }
                } else {
                    assert!(at.is_none());
                }
            }
            40..=62 => {
                let (a, b) = (self.pick(rng, filled), self.pick(rng, empty));
                if a.is_null() || b.is_null() {
                    return;
                }
                let mask = [rights::ALL, rights::R, rights::W, rights::R | rights::W][rng.below(4) as usize];
                let badge = [0, 0, 1, 2][rng.below(4) as usize];
                // 가끔은 원본에 매핑 기록을 달아 둠: 복사본은 기록 없이 시작해야 하고
                // 이동은 기록을 그대로 옮겨야 함
                if rng.below(3) == 0
                    && let Ok(Some(Entry { cap: Cap::Frame { base, rights, map }, .. })) = unsafe { read(a) }
                {
                    let map = match map {
                        Some(_) => None,
                        None => Some(Mapping {
                            asid: 1 + rng.below(200),
                            generation: rng.next(),
                            va: rng.below(1 << 20) << 14,
                            level: 3,
                        }),
                    };
                    unsafe { update(a, Cap::Frame { base, rights, map }) }.unwrap();
                }
                if unsafe { copy(a, b, mask, badge, false) }.is_ok()
                    && let Some(Entry { cap: Cap::Frame { map, .. }, .. }) = unsafe { read(b) }.unwrap()
                {
                    assert_eq!(map, None, "a copy starts unmapped");
                }
            }
            63..=65 => {
                let (a, t) = (self.pick(rng, filled), self.any_tcb_slot(rng));
                if !a.is_null() && !t.is_null() {
                    let _ = unsafe { copy(a, t, rights::ALL, 0, true) };
                }
            }
            66..=69 => {
                // 커널이 태스크를 묶을 때 쓰는 쌍 복사. 원본 둘은 같을 수도 있고
                // 서로 부모-자식일 수도 있음. 실패하면 두 목적지 모두 그대로여야 함
                let (a, c) = (self.pick(rng, filled), self.pick(rng, filled));
                let t = self.any_tcb_slot(rng);
                if a.is_null() || c.is_null() || t.is_null() {
                    return;
                }
                let owner = self
                    .containers
                    .values()
                    .find(|(b, n)| *n == TCB_N && (t as u64) > *b as u64 && (t as u64) < *b as u64 + (TCB_N * SLOT_SIZE) as u64)
                    .map(|c| c.0)
                    .unwrap();
                let (d1, d2) = unsafe { (owner.add(1), owner.add(2)) };
                let before = unsafe { (read(d1).unwrap(), read(d2).unwrap()) };
                if unsafe { copy_pair_bound(a, d1, c, d2) }.is_err() {
                    assert_eq!(unsafe { (read(d1).unwrap(), read(d2).unwrap()) }, before);
                }
            }
            70..=79 => {
                let (a, b) = (self.pick(rng, filled), self.pick(rng, empty));
                if a.is_null() || b.is_null() {
                    return;
                }
                let before = unsafe { read(a) }.unwrap();
                if unsafe { move_cap(a, b) }.is_ok() {
                    assert_eq!(unsafe { read(b) }.unwrap(), before, "a move carries the cap unchanged");
                    assert_eq!(unsafe { read(a) }.unwrap(), None);
                }
            }
            op => {
                let a = self.pick(rng, filled);
                if a.is_null() {
                    return;
                }
                // 루트 CNode 자신의 원본을 지우면 세계 전체가 끝나므로 가끔만 허용해
                // 긴 연산열이 유지되게 함
                let root_self = self
                    .containers
                    .get(&BOOT.cnode)
                    .is_some_and(|c| unsafe { c.0.add(boot_slot::CNODE as usize) } == a);
                if root_self && rng.below(40) != 0 {
                    return;
                }
                if op <= 91 {
                    unsafe { delete(a, self) }.unwrap()
                } else {
                    unsafe { revoke(a, self) }.unwrap()
                }
            }
        }
    }

    fn check(&self) {
        // 살아 있는 배열의 0번과 종료된 배열 전체는 완전히 비어 있어야 함
        let zero = |s: *mut Slot| unsafe {
            (*s).w == [0; 4] && (*s).prev == 0 && (*s).next == 0 && (*s).depth == 0 && (*s).aux == 0
        };
        for &(base, n) in &self.graveyard {
            for i in 0..n {
                assert!(zero(unsafe { base.add(i) }), "a destroyed container still holds something");
            }
        }
        let mut valid: BTreeMap<u64, usize> = BTreeMap::new();
        for &(base, n) in self.containers.values() {
            assert!(zero(base));
            for i in 1..n {
                valid.insert(unsafe { base.add(i) } as u64, n);
            }
        }

        let mut heads = Vec::new();
        let mut filled = 0usize;
        for (&addr, &n) in &valid {
            let s = addr as *mut Slot;
            let Some(e) = unsafe { read(s) }.expect("every slot decodes") else {
                continue;
            };
            filled += 1;
            assert_eq!(e.bound, n == TCB_N, "bound caps live exactly in TCB slots");
            assert!(self.objs.contains(&ident(&e.cap)), "cap to a dead object: {:?}", e.cap);
            let (p, nx, d) = unsafe { ((*s).prev, (*s).next, (*s).depth) };
            if p == 0 {
                assert_eq!(d, 0);
                assert!(e.original, "a list head is a boot original");
                heads.push(s);
            } else {
                assert!(d >= 1);
                assert!(valid.contains_key(&p), "prev points outside live slots");
                assert_eq!(unsafe { (*(p as *mut Slot)).next }, addr);
            }
            if nx != 0 {
                assert!(valid.contains_key(&nx), "next points outside live slots");
                assert_eq!(unsafe { (*(nx as *mut Slot)).prev }, addr);
            }
        }

        // 각 리스트를 머리부터 걸으며 전위 순서와 부모-자식 규칙을 확인
        let mut visited = 0usize;
        let mut originals: BTreeMap<(u8, u64), u32> = BTreeMap::new();
        for head in heads {
            let mut stack: Vec<(u64, Entry)> = Vec::new();
            let mut s = head;
            while !s.is_null() {
                visited += 1;
                assert!(visited <= filled, "cycle in a derivation list");
                let e = unsafe { read(s) }.unwrap().expect("linked slots are filled");
                let d = unsafe { (*s).depth };
                while stack.last().is_some_and(|t| t.0 >= d) {
                    stack.pop();
                }
                if let Some((pd, parent)) = stack.last() {
                    assert_eq!(*pd + 1, d, "a child sits exactly one level below its parent");
                    match parent.cap {
                        Cap::Untyped { base, used, .. } => {
                            assert!(e.original, "an untyped's children are originals");
                            let (class, pa) = ident(&e.cap);
                            assert_eq!(class, 2);
                            assert_eq!(pa % self.granule, 0);
                            assert!(pa >= base && pa + self.granule <= base + used, "object outside the consumed part");
                        }
                        _ => {
                            assert!(!e.original, "a copy is never an original");
                            assert_eq!(ident(&e.cap), ident(&parent.cap), "a copy names its parent's object");
                            assert_eq!(
                                std::mem::discriminant(&e.cap),
                                std::mem::discriminant(&parent.cap)
                            );
                            assert_eq!(cap_rights(&e.cap) & !cap_rights(&parent.cap), 0, "rights only shrink");
                            if let (Cap::Endpoint { badge: pb, .. }, Cap::Endpoint { badge: cb, .. }) =
                                (parent.cap, e.cap)
                                && pb != 0
                            {
                                assert_eq!(pb, cb, "a badge is never replaced");
                            }
                        }
                    }
                } else {
                    assert_eq!(d, 0);
                }
                if e.original {
                    *originals.entry(ident(&e.cap)).or_default() += 1;
                }
                stack.push((d, e));
                s = unsafe { (*s).next } as *mut Slot;
            }
        }
        assert_eq!(visited, filled, "every filled slot is reachable from a list head");

        // 살아 있는 오브젝트마다 원본이 정확히 하나
        for o in &self.objs {
            assert_eq!(originals.get(o), Some(&1), "object {o:x?} must have exactly one original");
        }
        assert_eq!(originals.len(), self.objs.len());
        // 슬롯을 담는 오브젝트는 살아 있는 동안에만 배열을 가짐
        for pa in self.containers.keys() {
            assert!(self.objs.contains(&(2, *pa)));
        }
    }
}

#[test]
fn random_operation_sequences_keep_every_invariant() {
    let mut rng = Rng(0xA076_1D64_78BD_642F);
    let mut finalized = 0;
    let mut torn = 0;
    let mut reused = 0;
    for round in 0..160 {
        let granule = if round % 2 == 0 { 4096 } else { 16384 };
        let mut w = World::new(granule, &mut rng);
        w.check();
        for _ in 0..260 {
            w.step(&mut rng);
            w.check();
        }
        finalized += w.finalized;
        torn += w.torn.len();
        reused += w.reused;
    }
    // 연산열이 실제로 종료 경로를 충분히 밟았는지 확인
    assert!(finalized > 1_500, "only {finalized} objects were finalized");
    assert!(torn > 20, "only {torn} TCBs lost a bound cap");
    assert!(reused > 100, "only {reused} retypes reused reclaimed memory");
}

#[test]
fn revoking_every_untyped_empties_the_world() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..60 {
        let mut w = World::new(4096, &mut rng);
        for _ in 0..200 {
            w.step(&mut rng);
        }
        w.check();
        // 남아 있는 untyped를 전부 회수하면 재분류로 생긴 오브젝트는 하나도
        // 남지 않아야 함
        let slots: Vec<*mut Slot> = w
            .containers
            .values()
            .flat_map(|&(b, n)| (1..n).map(move |i| unsafe { b.add(i) }))
            .collect();
        for s in slots {
            if w.graveyard.iter().any(|&(b, n)| (s as u64) >= b as u64 && (s as u64) < b as u64 + (n * 64) as u64) {
                continue;
            }
            if let Ok(Some(Entry { cap: Cap::Untyped { .. }, .. })) = unsafe { read(s) } {
                unsafe { revoke(s, &mut w) }.unwrap();
            }
        }
        w.check();
        let boot: BTreeSet<(u8, u64)> = [BOOT.tcb, BOOT.addr_space_root, BOOT.cnode]
            .into_iter()
            .map(|b| (2, b))
            .collect();
        for o in &w.objs {
            // untyped가 삭제돼 버린 경우 그 밑의 오브젝트도 이미 종료됐어야 함
            assert!(o.0 != 2 || boot.contains(o), "object {o:x?} survived the revocation of every untyped");
        }
    }
}
