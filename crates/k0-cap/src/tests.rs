use super::*;

const G: u64 = 4096;

fn empty() -> CNode {
    CNode {
        slots: [Cap::Empty; CNODE_SLOTS],
        used: 0,
    }
}

fn r(base: u64, size: u64) -> PhysRegion {
    PhysRegion { base, size }
}

fn filled(memory: &[PhysRegion], reserved: &[PhysRegion]) -> CNode {
    let mut c = empty();
    populate(&mut c, memory, reserved, 0x1234_0000).expect("populate");
    c
}

fn untypeds(c: &CNode) -> Vec<(u64, u64, u64)> {
    c.slots()
        .iter()
        .filter_map(|cap| match *cap {
            Cap::Untyped { base, size, used } => Some((base, size, used)),
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

#[test]
fn fixed_slots_come_first() {
    let c = filled(&[r(0x4000_0000, 0x10_0000)], &[]);
    assert!(matches!(c.cap(0), Some(Cap::Empty)));
    assert!(matches!(c.cap(1), Some(Cap::RootTcb)));
    assert!(matches!(c.cap(2), Some(Cap::AddrSpace { root_pa: 0x1234_0000 })));
    assert!(matches!(c.cap(3), Some(Cap::Console)));
    assert_eq!(untypeds(&c), vec![(0x4000_0000, 0x10_0000, 0)]);
}

#[test]
fn reserved_region_splits_memory() {
    let c = filled(&[r(0x1000, 0x9000)], &[r(0x4000, 0x2000)]);
    assert_eq!(untypeds(&c), vec![(0x1000, 0x3000, 0), (0x6000, 0x4000, 0)]);
}

#[test]
fn reserved_edges_and_full_cover() {
    assert_eq!(untypeds(&filled(&[r(0x1000, 0x4000)], &[r(0x1000, 0x1000)])), vec![(0x2000, 0x3000, 0)]);
    assert_eq!(untypeds(&filled(&[r(0x1000, 0x4000)], &[r(0x4000, 0x1000)])), vec![(0x1000, 0x3000, 0)]);
    assert_eq!(untypeds(&filled(&[r(0x1000, 0x4000)], &[r(0x0, 0x9000)])), vec![]);
}

#[test]
fn reserved_straddling_region_start_is_clipped() {
    let c = filled(&[r(0x4000, 0x4000)], &[r(0x2000, 0x3000)]);
    assert_eq!(untypeds(&c), vec![(0x5000, 0x3000, 0)]);
}

#[test]
fn reserved_order_and_overlap_do_not_matter() {
    let mem = [r(0x0, 0x10000)];
    let a = filled(&mem, &[r(0x8000, 0x1000), r(0x2000, 0x2000), r(0x3000, 0x2000)]);
    let b = filled(&mem, &[r(0x2000, 0x3000), r(0x8000, 0x1000)]);
    assert_eq!(untypeds(&a), untypeds(&b));
    assert_eq!(untypeds(&a), vec![(0x0, 0x2000, 0), (0x5000, 0x3000, 0), (0x9000, 0x7000, 0)]);
}

#[test]
fn reserved_outside_memory_is_ignored() {
    let c = filled(&[r(0x10000, 0x1000)], &[r(0x0, 0x1000), r(0x20000, 0x1000)]);
    assert_eq!(untypeds(&c), vec![(0x10000, 0x1000, 0)]);
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
        let mut c = empty();
        assert_eq!(populate(&mut c, memory, reserved, 0), Err(CapError::BadRegion));
    }
}

#[test]
fn adjacent_memory_regions_are_accepted() {
    let c = filled(&[r(0x0, 0x1000), r(0x1000, 0x1000)], &[]);
    assert_eq!(untypeds(&c), vec![(0x0, 0x1000, 0), (0x1000, 0x1000, 0)]);
}

#[test]
fn too_many_untypeds_run_out_of_slots() {
    let holes: Vec<PhysRegion> = (0..40).map(|i| r(i * 2 * G + G, G)).collect();
    let mut c = empty();
    assert_eq!(
        populate(&mut c, &[r(0, 80 * G)], &holes, 0),
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

        let mut c = empty();
        match populate(&mut c, &memory, &reserved, 0) {
            Ok(()) => {}
            Err(CapError::OutOfSlots) => continue,
            Err(e) => panic!("unexpected {e:?} for {memory:?} {reserved:?}"),
        }
        let u = untypeds(&c);
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
fn slot_accessors_respect_used_count() {
    let mut c = filled(&[r(0x1000, 0x1000)], &[]);
    let n = c.slots().len();
    assert_eq!(n, 5);
    assert!(c.cap(n).is_none());
    assert!(c.cap_mut(n).is_none());
    assert!(c.cap(usize::MAX).is_none());
    assert!(c.cap_mut(n - 1).is_some());
}

#[test]
fn retype_carves_aligned_objects_in_order() {
    let mut c = filled(&[r(0x4000_0000, 8 * G)], &[]);
    let slot = 4;
    let mut seen = Vec::new();
    for kind in [ObjKind::Frame, ObjKind::PageTable, ObjKind::Tcb, ObjKind::Endpoint] {
        let new = c
            .retype(slot, kind, G, |pa| {
                seen.push(pa);
                true
            })
            .expect("retype");
        assert_eq!(new, c.slots().len() - 1);
        let base = match (kind, c.cap(new).unwrap()) {
            (ObjKind::Frame, Cap::Frame { base, mapped: false }) => base,
            (ObjKind::PageTable, Cap::PageTable { base, installed: false }) => base,
            (ObjKind::Tcb, Cap::Tcb { base }) => base,
            (ObjKind::Endpoint, Cap::Endpoint { base }) => base,
            (k, cap) => panic!("{k:?} produced {cap:?}"),
        };
        assert_eq!(base, *seen.last().unwrap());
    }
    assert_eq!(seen, vec![0x4000_0000, 0x4000_1000, 0x4000_2000, 0x4000_3000]);
    assert_eq!(untypeds(&c), vec![(0x4000_0000, 8 * G, 4 * G)]);
}

#[test]
fn retype_rounds_unaligned_watermark_up() {
    let mut c = filled(&[r(0x1800, 0x4000)], &[]);
    let slot = 4;
    let mut at = 0;
    c.retype(slot, ObjKind::Frame, G, |pa| {
        at = pa;
        true
    })
    .unwrap();
    assert_eq!(at, 0x2000);
    assert_eq!(untypeds(&c), vec![(0x1800, 0x4000, 0x1800)]);
    assert_eq!(c.retype(slot, ObjKind::Frame, G, |pa| pa == 0x3000), Ok(6));
    assert_eq!(c.retype(slot, ObjKind::Frame, G, |pa| pa == 0x4000), Ok(7));
    assert_eq!(
        c.retype(slot, ObjKind::Frame, G, |_| panic!("prep must not run")),
        Err(RetypeError::Exhausted)
    );
    assert_eq!(untypeds(&c), vec![(0x1800, 0x4000, 0x3800)]);
}

#[test]
fn failed_prep_leaves_state_unchanged() {
    let mut c = filled(&[r(0x10000, 4 * G)], &[]);
    let before_used = c.slots().len();
    assert_eq!(c.retype(4, ObjKind::Tcb, G, |_| false), Err(RetypeError::PrepFailed));
    assert_eq!(c.slots().len(), before_used);
    assert_eq!(untypeds(&c), vec![(0x10000, 4 * G, 0)]);
    let mut at = 0;
    c.retype(4, ObjKind::Tcb, G, |pa| {
        at = pa;
        true
    })
    .unwrap();
    assert_eq!(at, 0x10000);
}

#[test]
fn retype_rejects_bad_sources_without_prep() {
    let mut c = filled(&[r(0x10000, 4 * G)], &[]);
    let frame = c.retype(4, ObjKind::Frame, G, |_| true).unwrap();
    let never = |_| -> bool { panic!("prep must not run") };
    assert_eq!(c.retype(c.slots().len(), ObjKind::Frame, G, never), Err(RetypeError::BadSlot));
    assert_eq!(c.retype(usize::MAX, ObjKind::Frame, G, never), Err(RetypeError::BadSlot));
    for slot in [0, 1, 2, 3, frame] {
        assert_eq!(c.retype(slot, ObjKind::Frame, G, never), Err(RetypeError::NotUntyped));
    }
}

#[test]
fn retype_reports_full_cnode_before_prep() {
    let mut c = filled(&[r(0, 64 * G)], &[]);
    while c.slots().len() < CNODE_SLOTS {
        c.retype(4, ObjKind::Frame, G, |_| true).unwrap();
    }
    let used = untypeds(&c)[0].2;
    assert_eq!(
        c.retype(4, ObjKind::Frame, G, |_| panic!("prep must not run")),
        Err(RetypeError::OutOfSlots)
    );
    assert_eq!(untypeds(&c)[0].2, used);
}

#[test]
fn retype_near_address_space_top_does_not_overflow() {
    let mut c = empty();
    c.slots[0] = Cap::Untyped {
        base: u64::MAX - G + 2,
        size: G - 2,
        used: 0,
    };
    c.used = 1;
    assert_eq!(
        c.retype(0, ObjKind::Frame, G, |_| panic!("prep must not run")),
        Err(RetypeError::Exhausted)
    );
}

#[test]
fn random_retypes_never_overlap() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for granule in [4096u64, 16384] {
        for _ in 0..200 {
            let memory: Vec<PhysRegion> = (0..1 + rng.below(3))
                .map(|i| r(i * 0x100_0000 + rng.below(4) * 0x800, granule * (1 + rng.below(12))))
                .collect();
            let mut c = filled(&memory, &[]);
            let sources: Vec<usize> = (4..c.slots().len()).collect();
            let mut carved: Vec<(u64, usize)> = Vec::new();
            for _ in 0..40 {
                let src = sources[rng.below(sources.len() as u64) as usize];
                let Cap::Untyped { base, size, used } = c.cap(src).unwrap() else {
                    unreachable!()
                };
                let mut at = None;
                match c.retype(src, ObjKind::Frame, granule, |pa| {
                    at = Some(pa);
                    true
                }) {
                    Ok(_) => {
                        let pa = at.unwrap();
                        assert_eq!(pa % granule, 0);
                        assert!(pa >= base + used && pa + granule <= base + size);
                        assert!(carved.iter().all(|&(o, _)| o + granule <= pa || pa + granule <= o));
                        carved.push((pa, src));
                    }
                    Err(RetypeError::Exhausted) | Err(RetypeError::OutOfSlots) => assert!(at.is_none()),
                    Err(e) => panic!("unexpected {e:?}"),
                }
            }
        }
    }
}
