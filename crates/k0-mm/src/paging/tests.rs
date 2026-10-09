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
    pool.map_page(root, va, 0x8000_0000, Perm::Rw.attrs()).unwrap();
    assert_eq!(pool.used, 4);
    pool.map_page(root, va + G, 0x8000_0000 + G, Perm::Rw.attrs()).unwrap();
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
fn overlapping_leaf_is_rejected_without_side_effects() {
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    let va = 0x0000_0001_0000_0000;
    pool.map_page(root, va, 0x8000_0000, Perm::Ro.attrs()).unwrap();
    let used = pool.used;
    assert_eq!(
        pool.map_page(root, va, 0x9000_0000, Perm::Rw.attrs()),
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
        pool.map_page(root, va, 0x8000_0000, Perm::Rw.attrs()),
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
            pool.map_page(root, va, 0, Perm::Rw.attrs()),
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
    assert_eq!(pool.map_page(root, va, 0, Perm::Rw.attrs()), Err(MmuError::BadTable));

    pool.tables[root].0[slot] = (pool.pa(0) + 31 * G) | DESC_TABLE;
    assert_eq!(pool.map_page(root, va, 0, Perm::Rw.attrs()), Err(MmuError::BadTable));

    pool.tables[root].0[slot] = (pool.pa(0) - G) | DESC_TABLE;
    assert_eq!(pool.map_page(root, va, 0, Perm::Rw.attrs()), Err(MmuError::BadTable));

    assert_eq!(pool.used, 1);
}

#[test]
fn map_range_requires_granule_alignment() {
    let mut pool = fresh_pool();
    let root = pool.alloc().unwrap();
    for (va, pa, len) in [(1, 0, G), (0, 8, G), (0, 0, G + 1), (G / 2, 0, G)] {
        assert_eq!(
            pool.map_range(root, va, pa, len, Perm::Ro),
            Err(MmuError::Misaligned)
        );
    }
    pool.map_range(root, 4 * G, 0x8000_0000, 3 * G, Perm::Ro).unwrap();
    assert_eq!(pool.used, 4);
}
