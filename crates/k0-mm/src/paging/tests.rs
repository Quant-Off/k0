use super::*;

const G: u64 = GRANULE as u64;

fn shift(level: u32) -> u32 {
    PAGE_SHIFT + BITS_PER_LEVEL * (3 - level)
}

fn writable(attrs: u64) -> bool {
    attrs & (1 << 7) == 0
}

fn el0_accessible(attrs: u64) -> bool {
    attrs & (1 << 6) != 0
}

fn fresh_pool() -> Box<Pool> {
    let layout = std::alloc::Layout::new::<Pool>();
    // SAFETY: Pool은 정수 배열과 정수, bool로만 이루어져 전부 0인 비트 패턴이 유효한 빈 풀이고
    //         같은 레이아웃으로 할당한 포인터라 Box가 소유해도 됨
    unsafe {
        let p = std::alloc::alloc_zeroed(layout).cast::<Pool>();
        assert!(!p.is_null());
        (*p).base_pa = 0x4020_0000;
        Box::from_raw(p)
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn index_decomposes_and_recomposes_every_va() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for _ in 0..10_000 {
        let va = rng.next() & ((1 << 48) - 1);
        let mut back = va & (G - 1);
        for level in 0..4 {
            let idx = index(va, level) as u64;
            let width = if level == 0 { 48 - shift(0) } else { BITS_PER_LEVEL };
            assert!(idx < 1 << width);
            back |= idx << shift(level);
        }
        assert_eq!(back, va);
    }
}

#[test]
fn index_ignores_the_upper_half_selector() {
    let va = 0x0000_1234_5678_9000u64 & !(G - 1);
    for level in 0..4 {
        assert_eq!(index(va, level), index(va | KERNEL_VA_OFFSET, level));
    }
}

#[test]
fn kernel_permissions_obey_wx() {
    let perms = [Perm::Text, Perm::Ro, Perm::Rw, Perm::Device];
    let mut executable = 0;
    for p in perms {
        let a = p.attrs();
        assert_eq!(a & 0b11, DESC_PAGE);
        assert_ne!(a & ATTR_AF, 0);
        assert_ne!(a & UXN, 0, "kernel mappings are never EL0-executable");
        assert!(!el0_accessible(a), "kernel mappings are never EL0-accessible");
        if writable(a) {
            assert_ne!(a & PXN, 0, "writable kernel mapping must be PXN");
        }
        if a & PXN == 0 {
            executable += 1;
            assert!(!writable(a));
        }
    }
    assert_eq!(executable, 1);
    assert_eq!(Perm::Device.attrs() & (0b111 << 2), IDX_DEVICE);
}

#[test]
fn address_mask_keeps_only_granule_aligned_48_bit_addresses() {
    assert_eq!(ADDR_MASK & (G - 1), 0);
    assert_eq!(ADDR_MASK >> 48, 0);
    assert_eq!(ADDR_MASK & (UXN | PXN | ATTR_AF | 0b11), 0);
}

#[test]
fn mapping_a_page_builds_three_tables_then_reuses_them() {
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    let va = 0x0000_0040_0000_0000 & !(G - 1);
    pool.map_page(root, va, 0x8000_0000, Perm::Rw.attrs(), false).unwrap();
    assert_eq!(pool.used, 4);
    pool.map_page(root, va + G, 0x8000_0000 + G, Perm::Rw.attrs(), false).unwrap();
    assert_eq!(pool.used, 4);

    let mut t = root;
    for level in 0..3 {
        let e = pool.tables[t].0[index(va, level)];
        assert_eq!(e & 0b11, DESC_TABLE);
        t = pool.index_of(e & ADDR_MASK).unwrap();
    }
    assert_eq!(pool.tables[t].0[index(va, 3)], 0x8000_0000 | Perm::Rw.attrs());
}

#[test]
fn an_identical_leaf_is_accepted_only_when_asked() {
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    let va = 0x0000_0001_0000_0000;
    let rw = Perm::Rw.attrs();
    pool.map_page(root, va, 0x8000_0000, rw, false).unwrap();
    let used = pool.used;
    // 같은 PA와 같은 속성: 재분류 재사용 경로만 허용
    assert_eq!(pool.map_page(root, va, 0x8000_0000, rw, false), Err(MmuError::Overlap));
    assert_eq!(pool.map_page(root, va, 0x8000_0000, rw, true), Ok(()));
    // PA나 속성이 다르면 어느 쪽이든 거부
    assert_eq!(pool.map_page(root, va, 0x9000_0000, rw, true), Err(MmuError::Overlap));
    assert_eq!(
        pool.map_page(root, va, 0x8000_0000, Perm::Ro.attrs(), true),
        Err(MmuError::Overlap)
    );
    assert_eq!(pool.used, used);
    let mut t = root;
    for level in 0..3 {
        t = pool.index_of(pool.tables[t].0[index(va, level)] & ADDR_MASK).unwrap();
    }
    assert_eq!(pool.tables[t].0[index(va, 3)], 0x8000_0000 | rw);
}

#[test]
fn overlapping_leaf_is_rejected_without_side_effects() {
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    let va = 0x0000_0001_0000_0000;
    pool.map_page(root, va, 0x8000_0000, Perm::Ro.attrs(), false).unwrap();
    let used = pool.used;
    assert_eq!(
        pool.map_page(root, va, 0x9000_0000, Perm::Rw.attrs(), false),
        Err(MmuError::Overlap)
    );
    assert_eq!(pool.used, used);
}

#[test]
fn exhaustion_mid_walk_rolls_back_new_links() {
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    while pool.used < POOL_LEN - 2 {
        pool.alloc().unwrap();
    }
    let before: Vec<u64> = pool.tables[root].0.to_vec();
    let used = pool.used;
    let va = 0x0000_0080_0000_0000 & !(G - 1);
    assert_eq!(
        pool.map_page(root, va, 0x8000_0000, Perm::Rw.attrs(), false),
        Err(MmuError::OutOfTables)
    );
    assert_eq!(pool.used, used);
    assert_eq!(pool.tables[root].0.to_vec(), before);
    for i in used..POOL_LEN {
        assert!(pool.tables[i].0.iter().all(|&e| e == 0), "table {i} left dirty");
    }
}

#[test]
fn repeated_failures_cannot_drain_the_pool() {
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    while pool.used < POOL_LEN - 1 {
        pool.alloc().unwrap();
    }
    for i in 0..1000u64 {
        let va = (i << 39) & ((1 << 48) - 1) & !(G - 1);
        assert_eq!(
            pool.map_page(root, va, 0, Perm::Rw.attrs(), false),
            Err(MmuError::OutOfTables)
        );
        assert_eq!(pool.used, POOL_LEN - 1);
    }
}

#[test]
fn forged_or_block_entries_are_rejected() {
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    let va = 0x0000_0000_4000_0000;
    let slot = index(va, 0);

    pool.tables[root].0[slot] = 0x4000_0000 | 0b01;
    assert_eq!(pool.map_page(root, va, 0, Perm::Rw.attrs(), false), Err(MmuError::BadTable));

    pool.tables[root].0[slot] = (pool.pa(0) + 31 * G) | DESC_TABLE;
    assert_eq!(pool.map_page(root, va, 0, Perm::Rw.attrs(), false), Err(MmuError::BadTable));

    pool.tables[root].0[slot] = (pool.pa(0) - G) | DESC_TABLE;
    assert_eq!(pool.map_page(root, va, 0, Perm::Rw.attrs(), false), Err(MmuError::BadTable));

    assert_eq!(pool.used, 1);
}

#[test]
fn map_range_requires_granule_alignment() {
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    for (va, pa, len) in [(1, 0, G), (0, 8, G), (0, 0, G + 1), (G / 2, 0, G)] {
        assert_eq!(
            pool.map_range(root, va, pa, len, Perm::Ro, false),
            Err(MmuError::Misaligned)
        );
    }
    pool.map_range(root, 4 * G, 0x8000_0000, 3 * G, Perm::Ro, false).unwrap();
    assert_eq!(pool.used, 4);
}

#[test]
fn table_addresses_come_from_the_recorded_base() {
    // 이미지가 2^47 위의 물리 주소에 있어도(가상 주소의 비트와 무관하게)
    // 테이블의 물리 주소는 기록해 둔 기준에서만 나옴
    for base in [0x4020_0000u64, 0x7FFF_FFE0_0000] {
        let mut pool = fresh_pool();
        pool.base_pa = base;
        let root = pool.alloc().unwrap();
        pool.map_page(root, 0xFFFF_8000_0000_0000, 0x8000_0000, Perm::Rw.attrs(), false).unwrap();
        for i in 0..pool.used {
            assert_eq!(pool.pa(i), base + (i as u64) * G);
            assert_eq!(pool.index_of(pool.pa(i)), Ok(i));
        }
        assert_eq!(pool.index_of(base - G), Err(MmuError::BadTable));
        assert_eq!(pool.index_of(base + 1), Err(MmuError::BadTable));
        assert_eq!(pool.index_of(pool.pa(pool.used)), Err(MmuError::BadTable));
        let (t, i) = pool.leaf_of(root, 0xFFFF_8000_0000_0000).unwrap();
        assert_eq!(pool.tables[t].0[i] & ADDR_MASK, 0x8000_0000);
        assert_eq!(pool.leaf_of(root, 0xFFFF_0000_0000_0000), Err(MmuError::MissingTable));
    }
}

#[test]
fn the_image_range_is_closed_to_aliases() {
    let mut pool = fresh_pool();
    pool.image = (0x4008_0000, 0x4030_0000);
    assert!(pool.overlaps_image(0x4008_0000, 0x4008_1000));
    assert!(pool.overlaps_image(0x402F_F000, 0x4030_1000));
    assert!(pool.overlaps_image(0x4000_0000, 0x5000_0000));
    assert!(!pool.overlaps_image(0x4007_F000, 0x4008_0000));
    assert!(!pool.overlaps_image(0x4030_0000, 0x4030_1000));
}

/// 예약 구간 구실을 하는 호스트 메모리입니다. 가짜 물리 주소 `base`에 놓인
/// 것처럼 보이도록 이 스레드의 별칭 오프셋을 맞춥니다. 내용은 0이 아닌
/// 값으로 채워, 테이블이 내줄 때 소거되는지 드러나게 합니다.
struct Arena {
    ptr: *mut u8,
    layout: std::alloc::Layout,
    base: u64,
    tables: usize,
}

impl Arena {
    fn new(base: u64, tables: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(tables.max(1) * GRANULE, GRANULE).unwrap();
        // SAFETY: 크기가 0이 아닌 유효한 레이아웃이고 할당한 범위만 채움
        let ptr = unsafe {
            let p = std::alloc::alloc(layout);
            assert!(!p.is_null());
            core::ptr::write_bytes(p, 0xA5, layout.size());
            p
        };
        TEST_ALIAS_OFFSET.with(|c| c.set((ptr as u64).wrapping_sub(base)));
        Self {
            ptr,
            layout,
            base,
            tables,
        }
    }

    fn range(&self) -> Range<u64> {
        self.base..self.base + (self.tables * GRANULE) as u64
    }
}

impl Drop for Arena {
    fn drop(&mut self) {
        TEST_ALIAS_OFFSET.with(|c| c.set(0));
        // SAFETY: new()가 같은 레이아웃으로 할당한 포인터임
        unsafe { std::alloc::dealloc(self.ptr, self.layout) };
    }
}

/// 정적 배열에 `free`장만 남긴 켜진 풀을 만드는 함수입니다.
fn live_pool(free: usize) -> Box<Pool> {
    let mut pool = fresh_pool();
    pool.root1 = pool.alloc().unwrap();
    pool.enabled = true;
    assert!(free <= POOL_LEN - pool.used);
    pool.used = POOL_LEN - free;
    pool
}

/// 테이블 하나가 덮는 바이트 수(레벨 1, 2, 3)
fn span(level: u32) -> u64 {
    G << (BITS_PER_LEVEL * (4 - level))
}

#[test]
fn the_table_bound_counts_the_blocks_a_range_touches_at_each_level() {
    let l3 = span(3);
    // 최하위 테이블 하나 안에 든 구간: 레벨마다 한 장
    assert_eq!(alias_tables_for(&(l3..l3 + G)), 3);
    assert_eq!(alias_tables_for(&(l3..2 * l3)), 3);
    // 최하위 테이블의 경계를 넘으면 한 장 더
    assert_eq!(alias_tables_for(&(l3 - G..l3 + G)), 4);
    assert_eq!(alias_tables_for(&(0..10 * l3)), 12);
    // 위 레벨의 경계를 넘으면 그 레벨에서도 한 장 더
    let l2 = span(2);
    assert_eq!(alias_tables_for(&(l2 - G..l2 + G)), 5);
    let l1 = span(1);
    if l1 < PA_CEILING {
        assert_eq!(alias_tables_for(&(l1 - G..l1 + G)), 6);
    }
}

#[test]
fn aliasing_every_granule_of_any_ranges_stays_within_the_bound() {
    let mut rng = Rng(0x0BAD_5EED_1234_5678);
    for round in 0..300 {
        // 경계 근처에 걸친 구간 몇 개. 구간끼리 겹쳐도 상한은 여전히 상한임
        let mut ranges: Vec<Range<u64>> = Vec::new();
        for _ in 0..1 + rng.next() % 5 {
            let level = 1 + (rng.next() % 3) as u32;
            let boundary = (1 + rng.next() % 6) * span(level);
            let before = (rng.next() % 40) * G;
            let pages = 1 + rng.next() % 90;
            let start = boundary.saturating_sub(before).max(G);
            let end = start + pages * G;
            if end <= PA_CEILING {
                ranges.push(start..end);
            }
        }
        if ranges.is_empty() {
            continue;
        }
        let bound: u64 = ranges.iter().map(alias_tables_for).sum();

        // 정적 배열은 루트만 내주고 닫음. 그 뒤의 테이블은 상한만큼만 있는
        // 예약에서 나와야 함
        let arena = Arena::new(0x7000_0000_0000, bound as usize);
        let mut pool = fresh_pool();
        let root = pool.alloc().unwrap();
        pool.used = POOL_LEN;
        pool.ext[0] = (arena.base, bound as usize);
        pool.ext_regions = 1;
        pool.ext_total = bound as usize;

        for r in &ranges {
            let mut pa = r.start;
            while pa < r.end {
                pool.map_page(root, pa, pa, Perm::Rw.attrs(), true)
                    .unwrap_or_else(|e| panic!("round {round}: {e:?} at {pa:#x} of {ranges:x?}"));
                pa += G;
            }
        }
        assert!(pool.ext_used as u64 <= bound, "round {round}");
        assert_eq!(pool.used, POOL_LEN);
        // 매핑은 실제로 예약의 테이블을 거쳐 읽힘
        let (t, i) = pool.leaf_of(root, ranges[0].start).unwrap();
        assert!(t >= POOL_LEN);
        assert_eq!(pool.tbl(t)[i], ranges[0].start | Perm::Rw.attrs());
    }
}

#[test]
fn tables_from_a_reserve_are_zeroed_when_handed_out_and_come_first() {
    let arena = Arena::new(0x10_0000_0000, 4);
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    pool.ext[0] = (arena.base, 4);
    pool.ext_regions = 1;
    pool.ext_total = 4;
    let static_used = pool.used;

    let t = pool.alloc().unwrap();
    assert_eq!(t, POOL_LEN, "the reserve is used before the static array");
    assert_eq!(pool.pa(t), arena.base);
    assert!(pool.tbl(t).iter().all(|&e| e == 0), "arena was filled with 0xA5");
    assert_eq!(pool.used, static_used);

    // 예약이 바닥나면 정적 배열로 넘어감
    for _ in 0..3 {
        assert!(pool.alloc().unwrap() >= POOL_LEN);
    }
    assert_eq!(pool.alloc().unwrap(), static_used);
    let _ = root;
}

#[test]
fn a_failed_walk_gives_back_reserve_tables_too() {
    let arena = Arena::new(0x10_0000_0000, 2);
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    pool.used = POOL_LEN;
    pool.ext[0] = (arena.base, 2);
    pool.ext_regions = 1;
    pool.ext_total = 2;

    // 세 장이 필요한데 두 장뿐이라 걷다가 실패함. 받은 두 장과 루트의 링크가
    // 전부 되돌려져야 함
    let va = 0x40_0000_0000;
    assert_eq!(pool.map_page(root, va, 0x8000_0000, Perm::Rw.attrs(), false), Err(MmuError::OutOfTables));
    assert_eq!(pool.ext_used, 0);
    assert!(pool.tables[root].0.iter().all(|&e| e == 0));
    // 되돌린 뒤에는 같은 예약으로 더 얕은 일을 할 수 있음
    assert_eq!(pool.alloc().unwrap(), POOL_LEN);
}

#[test]
fn links_into_a_reserve_are_trusted_only_for_tables_already_handed_out() {
    let arena = Arena::new(0x10_0000_0000, 4);
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    pool.ext[0] = (arena.base, 4);
    pool.ext_regions = 1;
    pool.ext_total = 4;
    let t = pool.alloc().unwrap();
    assert_eq!(pool.index_of(arena.base), Ok(t));
    // 예약 안이라도 아직 내주지 않은 테이블, 정렬되지 않은 주소, 예약 밖
    assert_eq!(pool.index_of(arena.base + G), Err(MmuError::BadTable));
    assert_eq!(pool.index_of(arena.base + 8), Err(MmuError::BadTable));
    assert_eq!(pool.index_of(arena.base + 4 * G), Err(MmuError::BadTable));
    assert_eq!(pool.index_of(arena.base - G), Err(MmuError::BadTable));
    // 그런 주소를 가리키는 디스크립터는 걷지 않음
    let va = 0x0000_0001_0000_0000;
    pool.tables[root].0[index(va, 0)] = (arena.base + G) | DESC_TABLE;
    assert_eq!(pool.map_page(root, va, 0x8000_0000, Perm::Rw.attrs(), false), Err(MmuError::BadTable));
}

#[test]
fn a_reserve_pays_for_its_own_mapping_after_the_first_piece() {
    // 최하위 테이블 세 장 분량에 조금 더한 예약. 정적 배열에는 첫 조각을 매핑할
    // 석 장만 남김
    let tables = 3 * ENTRIES + 5;
    let base = 8 * span(2);
    let arena = Arena::new(base, tables);
    let mut pool = live_pool(3);
    pool.reserve(arena.range()).unwrap();

    assert_eq!(pool.ext_regions, 1);
    assert_eq!(pool.ext[0], (base, tables));
    assert_eq!(pool.ext_total, tables);
    assert_eq!(pool.used, POOL_LEN, "the first piece took the last static tables");
    // 나머지 세 조각의 최하위 테이블은 예약에서 나옴
    assert_eq!(pool.ext_used, 3);
    // 예약 전체가 별칭으로 매핑돼 있음
    for i in [0, ENTRIES - 1, ENTRIES, tables - 1] {
        let pa = base + (i * GRANULE) as u64;
        let (t, e) = pool.leaf_of(pool.root1, pa + KERNEL_VA_OFFSET).unwrap();
        assert_eq!(pool.tbl(t)[e], pa | Perm::Rw.attrs());
    }
}

#[test]
fn a_reserve_that_cannot_map_its_first_piece_is_not_registered() {
    let arena = Arena::new(8 * span(2), 16);
    let mut pool = live_pool(2);
    let used = pool.used;
    assert_eq!(pool.reserve(arena.range()), Err(MmuError::OutOfTables));
    assert_eq!((pool.ext_regions, pool.ext_total, pool.ext_used), (0, 0, 0));
    assert_eq!(pool.used, used, "the failed walk gave its tables back");
}

#[test]
fn a_reserve_that_fails_later_keeps_what_it_already_mapped() {
    // 두 번째 조각이 이미 매핑된 페이지와 겹쳐 실패함. 첫 조각은 등록된 채
    // 남아 테이블로 쓰이고, 호출자는 이 구간의 메모리를 내주지 않음
    let tables = ENTRIES + 4;
    let base = 8 * span(2);
    let arena = Arena::new(base, tables);
    let mut pool = live_pool(6);
    let clash = base + (ENTRIES * GRANULE) as u64 + G;
    let root1 = pool.root1;
    pool.map_page(root1, clash + KERNEL_VA_OFFSET, clash, Perm::Ro.attrs(), false).unwrap();

    assert_eq!(pool.reserve(arena.range()), Err(MmuError::Overlap));
    assert_eq!(pool.ext_regions, 1);
    assert_eq!(pool.ext[0], (base, ENTRIES));
    assert_eq!(pool.ext_total, ENTRIES);
}

#[test]
fn reserves_are_rejected_when_misaligned_or_too_many() {
    let arena = Arena::new(8 * span(2), 2);
    let mut pool = live_pool(6);
    let r = arena.range();
    assert_eq!(pool.reserve(r.start + 8..r.end), Err(MmuError::Misaligned));
    assert_eq!(pool.reserve(r.start..r.start), Err(MmuError::Misaligned));
    pool.ext_regions = MAX_TABLE_RESERVES;
    assert_eq!(pool.reserve(r), Err(MmuError::OutOfTables));
}

#[test]
fn a_reserve_that_starts_inside_a_table_block_is_cut_at_the_block_boundary() {
    // 시작이 최하위 테이블의 경계에 있지 않은 예약. 첫 조각은 다음 경계에서
    // 끝나야 하고(그 조각만 정적 배열이 치름), 경계를 넘는 조각을 한 번에
    // 매핑하려 들면 남은 정적 테이블로는 모자람
    let before = 7u64;
    let tables = before as usize + ENTRIES + 3;
    let base = 8 * span(2) + span(3) - before * G;
    let arena = Arena::new(base, tables);
    let mut pool = live_pool(3);
    pool.reserve(arena.range()).unwrap();

    assert_eq!(pool.ext[0], (base, tables));
    assert_eq!(pool.used, POOL_LEN, "only the first piece used static tables");
    // 경계 뒤의 두 조각은 예약의 테이블 두 장으로 매핑됨
    assert_eq!(pool.ext_used, 2);
    for i in [0, before as usize - 1, before as usize, tables - 1] {
        let pa = base + (i * GRANULE) as u64;
        let (t, e) = pool.leaf_of(pool.root1, pa + KERNEL_VA_OFFSET).unwrap();
        assert_eq!(pool.tbl(t)[e], pa | Perm::Rw.attrs());
    }
}

#[test]
fn table_numbers_run_through_several_reserves_in_order() {
    // 예약 구간 셋(크기 2, 1, 3)이 한 호스트 메모리 안에 떨어져 있음. 테이블
    // 번호는 등록한 순서대로 이어지고 주소와 번호가 서로 바뀌어야 함
    let arena = Arena::new(0x10_0000_0000, 16);
    let mut pool = fresh_pool();
    let _root = pool.alloc().unwrap();
    let b = arena.base;
    pool.ext[0] = (b + 8 * G, 2);
    pool.ext[1] = (b, 1);
    pool.ext[2] = (b + 3 * G, 3);
    pool.ext_regions = 3;
    pool.ext_total = 6;

    let want = [b + 8 * G, b + 9 * G, b, b + 3 * G, b + 4 * G, b + 5 * G];
    for (n, &pa) in want.iter().enumerate() {
        // 아직 내주지 않은 테이블은 주소가 예약 안이어도 받아들이지 않음
        assert_eq!(pool.index_of(pa), Err(MmuError::BadTable));
        let t = pool.alloc().unwrap();
        assert_eq!(t, POOL_LEN + n);
        assert_eq!(pool.pa(t), pa);
        assert_eq!(pool.index_of(pa), Ok(t));
    }
    // 구간 사이의 빈 곳과 구간 바로 뒤는 테이블이 아님
    for pa in [b + G, b + 2 * G, b + 6 * G, b + 10 * G] {
        assert_eq!(pool.index_of(pa), Err(MmuError::BadTable));
    }
}
