mod maps;

use std::panic::{catch_unwind, AssertUnwindSafe};

use k0_boot::testing::builder::Builder;
use k0_boot::testing::{cells, env_u64, mutate, rk3399, rpi4, splitmix, strs, virt2, virt3, Rng, N};
use k0_boot::{discover_blob, parse_blob, BootError, Fdt, MemRegion, Token, ValidateError, BOOT_WINDOW_SIZE};

use crate::fdtw::{rewrite, root_cells, Edits, RewriteError, WriteError, Writer};
use crate::memmap::{plan, Limits, Map, Plan, PlanError, Scratch, ALIGN};
use crate::prepare::{firmware_dtb_size, prepare, PrepareError, Request, Work};
use crate::verify::{verify, Block, VerifyError};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
const WB: u64 = 0x8;
const CONV: u32 = 7;
const RT_DATA: u32 = 6;

type D = (u32, u64, u64, u64);

fn map_bytes(descs: &[D], stride: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for &(kind, start, pages, attr) in descs {
        let mut d = vec![0u8; stride];
        d[0..4].copy_from_slice(&kind.to_le_bytes());
        d[8..16].copy_from_slice(&start.to_le_bytes());
        d[24..32].copy_from_slice(&pages.to_le_bytes());
        d[32..40].copy_from_slice(&attr.to_le_bytes());
        out.extend(d);
    }
    out
}

fn mem(base: u64, size: u64) -> MemRegion {
    MemRegion { base, size }
}

fn plan_of(descs: &[D], holes: &[MemRegion], block: &MemRegion, limits: &Limits) -> Result<Plan, PlanError> {
    let bytes = map_bytes(descs, 48);
    let map = Map::new(&bytes, 48)?;
    let (np, nr) = Scratch::need(map.count(), holes.len());
    let mut points = vec![0u64; np];
    let mut ranges = vec![mem(0, 0); nr];
    plan(&map, holes, block, limits, Scratch { points: &mut points, ranges: &mut ranges })
}

const WIDE: Limits = Limits { max_addr: 1 << 47, max_entry: 1 << 47 };

/// 가장 큰 빈 메모리 디스크립터 안에 4MiB 블록을 놓습니다.
fn block_in(descs: &[D]) -> MemRegion {
    let &(_, start, pages, _) = descs
        .iter()
        .filter(|d| d.0 == CONV && d.3 & WB != 0)
        .max_by_key(|d| d.2)
        .expect("a conventional descriptor");
    let base = (start + ALIGN - 1) & !(ALIGN - 1);
    assert!(base + 4 * MIB <= start + pages * 4096);
    mem(base, 4 * MIB)
}

fn parts(block: &MemRegion) -> Block {
    let dtb = block.base..block.base + 64 * KIB;
    let image = dtb.end..dtb.end + MIB;
    Block { dtb, window: image.end..image.end + BOOT_WINDOW_SIZE, image }
}

fn covered(regions: &[MemRegion], addr: u64) -> bool {
    regions.iter().any(|r| r.base <= addr && addr < r.base + r.size)
}

#[test]
fn the_measured_maps_plan_within_the_kernel_limits() {
    for (name, descs, ranges, dropped) in [
        ("qemu 512M", maps::QEMU_EDK2_512M, 6, 0),
        ("qemu 16G", maps::QEMU_EDK2_16G_NOACPI, 6, 0),
        ("debian 512M", maps::DEBIAN_AAVMF_512M_NOACPI, 6, 0),
        ("server", maps::SERVER_EDK2_16G, 8, 2),
    ] {
        let block = block_in(descs);
        let p = plan_of(descs, &[], &block, &WIDE).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!((p.regions().len(), p.dropped), (ranges, dropped), "{name}");
        // 버린 구간은 작은 것뿐이라 손실은 1MiB에 못 미침
        assert!(p.usable_bytes - p.kept_bytes < MIB, "{name}: lost {}", p.usable_bytes - p.kept_bytes);

        let bytes = map_bytes(descs, 48);
        let map = Map::new(&bytes, 48).unwrap();
        assert_eq!(verify(p.regions(), &map, &[], &parts(&block), &WIDE), Ok(()), "{name}");
        // 부트 서비스와 런타임 메모리의 어느 바이트도 구간에 들지 않음
        for d in map.iter().filter(|d| !d.usable()) {
            assert!(!covered(p.regions(), d.start), "{name}: {d:?}");
            assert!(!covered(p.regions(), d.end - 1), "{name}: {d:?}");
        }
    }
}

#[test]
fn only_free_and_loader_memory_with_the_right_attributes_is_ram() {
    let block = mem(0x1000_0000, 4 * MIB);
    let base: D = (CONV, 0x1000_0000, 0x800, WB);
    for (what, extra) in [
        ("boot services code", (3, 0x2000_0000, 0x400, WB)),
        ("boot services data", (4, 0x2000_0000, 0x400, WB)),
        ("runtime data", (RT_DATA, 0x2000_0000, 0x400, WB | 1 << 63)),
        ("acpi reclaim", (9, 0x2000_0000, 0x400, WB)),
        ("reserved", (0, 0x2000_0000, 0x400, WB)),
        ("mmio", (11, 0x2000_0000, 0x400, 1 | 1 << 63)),
        ("persistent", (14, 0x2000_0000, 0x400, WB)),
        ("unknown type", (0x7000_0001, 0x2000_0000, 0x400, WB)),
        ("no write-back attribute", (CONV, 0x2000_0000, 0x400, 0x1)),
        ("special purpose", (CONV, 0x2000_0000, 0x400, WB | 0x4_0000)),
        ("non-volatile", (CONV, 0x2000_0000, 0x400, WB | 0x8000)),
        ("runtime attribute", (CONV, 0x2000_0000, 0x400, WB | 1 << 63)),
    ] {
        let p = plan_of(&[base, extra], &[], &block, &WIDE).unwrap();
        assert_eq!(p.regions(), &[mem(0x1000_0000, 8 * MIB)], "{what}");
    }
    for (what, extra) in [("loader code", (1, 0x2000_0000, 0x400, WB)), ("loader data", (2, 0x2000_0000, 0x400, WB))] {
        let p = plan_of(&[base, extra], &[], &block, &WIDE).unwrap();
        assert_eq!(p.regions().len(), 2, "{what}");
    }
}

#[test]
fn a_deny_descriptor_wins_over_an_overlapping_usable_one() {
    let block = mem(0x1000_0000, 4 * MIB);
    // 빈 메모리 16MiB 한가운데를 런타임 데이터 4KiB가 덮음(실제 서버 맵에 있는 모양)
    let descs = [(CONV, 0x1000_0000, 0x1000, WB), (RT_DATA, 0x1080_1000, 1, WB | 1 << 63)];
    let p = plan_of(&descs, &[], &block, &WIDE).unwrap();
    // 구멍은 정렬 단위 바깥쪽으로 넓어짐
    assert_eq!(p.regions(), &[mem(0x1000_0000, 0x80_0000), mem(0x1080_4000, 0x7f_c000)]);
    // 순서가 바뀌어도 결과는 같음
    let swapped = [descs[1], descs[0]];
    assert_eq!(plan_of(&swapped, &[], &block, &WIDE).unwrap().regions(), p.regions());
}

#[test]
fn touching_descriptors_merge_before_they_are_rounded() {
    let block = mem(0x1000_0000, 4 * MIB);
    // 4KiB 경계에서 맞닿은 로더 데이터와 빈 메모리. 따로 줄이면 사이에 구멍이 생김
    let descs = [(CONV, 0x1000_0000, 0x801, WB), (2, 0x1080_1000, 0x7ff, WB)];
    let p = plan_of(&descs, &[], &block, &WIDE).unwrap();
    assert_eq!(p.regions(), &[mem(0x1000_0000, 16 * MIB)]);
}

#[test]
fn holes_are_cut_out_with_outward_rounding() {
    let block = mem(0x1000_0000, 4 * MIB);
    let descs = [(CONV, 0x1000_0000, 0x4000, WB)];
    // 디바이스 한 페이지와 맵 밖에 있는 예약, 그리고 끝이 넘치는 구멍
    let holes = [mem(0x1200_1000, 0x1000), mem(0x9000_0000, MIB), mem(u64::MAX - 4096, 8192)];
    let p = plan_of(&descs, &holes, &block, &WIDE).unwrap();
    assert_eq!(p.regions(), &[mem(0x1000_0000, 0x200_0000), mem(0x1200_4000, 0x1ff_c000)]);
}

#[test]
fn the_block_is_checked_not_added() {
    let descs = [(CONV, 0x1000_0000, 0x4000, WB), (CONV, 0x2000_0000, 0x400, WB)];
    let block = mem(0x1000_0000, 4 * MIB);
    assert!(plan_of(&descs, &[], &block, &WIDE).is_ok());
    // 블록 위에 펌웨어 예약이 걸치면 블록을 우선하지 않고 거부
    let holes = [mem(0x1010_0000, 4096)];
    assert_eq!(plan_of(&descs, &holes, &block, &WIDE), Err(PlanError::BlockNotUsable));
    // 사용 가능한 메모리 밖, 두 구간에 걸침, 거부 디스크립터 위
    assert_eq!(plan_of(&descs, &[], &mem(0x3000_0000, 4 * MIB), &WIDE), Err(PlanError::BlockNotUsable));
    assert_eq!(plan_of(&descs, &[], &mem(0x13e0_0000, 4 * MIB), &WIDE), Err(PlanError::BlockNotUsable));
    let denied = [descs[0], (RT_DATA, 0x1020_0000, 1, WB)];
    assert_eq!(plan_of(&denied, &[], &block, &WIDE), Err(PlanError::BlockNotUsable));
    // 정렬이 맞지 않는 블록
    assert_eq!(plan_of(&descs, &[], &mem(0x1000_1000, 4 * MIB), &WIDE), Err(PlanError::BadBlock));
    assert_eq!(plan_of(&descs, &[], &mem(0x1000_0000, 0), &WIDE), Err(PlanError::BadBlock));
    // 물리 주소 0에서 시작하는 블록(DTB가 0번지에 놓이게 됨)
    let low = [(CONV, 0, 0x1000, WB)];
    assert_eq!(plan_of(&low, &[], &mem(0, 4 * MIB), &WIDE), Err(PlanError::BadBlock));
    assert!(plan_of(&low, &[], &mem(ALIGN, 4 * MIB), &WIDE).is_ok());
}

#[test]
fn beyond_eight_ranges_the_block_and_the_largest_survive() {
    // 크기가 다른 구간 12개, 블록은 가장 작은 구간에 있음
    let mut descs = Vec::new();
    for i in 0..12u64 {
        let pages = if i == 0 { 0x400 } else { 0x1000 * (i + 1) };
        descs.push((CONV, 0x1_0000_0000 * (i + 1), pages, WB));
    }
    let block = mem(0x1_0000_0000, 4 * MIB);
    let p = plan_of(&descs, &[], &block, &WIDE).unwrap();
    assert_eq!((p.regions().len(), p.dropped), (8, 4));
    assert_eq!(p.regions()[0], mem(0x1_0000_0000, 4 * MIB));
    // 나머지 일곱은 큰 것부터(뒤쪽 구간들)
    let bases: Vec<u64> = p.regions()[1..].iter().map(|r| r.base >> 32).collect();
    assert_eq!(bases, [6, 7, 8, 9, 10, 11, 12]);
    // 크기가 같으면 낮은 주소가 남음
    let same: Vec<D> = (0..12u64).map(|i| (CONV, 0x1_0000_0000 * (i + 1), 0x1000, WB)).collect();
    let p = plan_of(&same, &[], &mem(0xc_0000_0000, 4 * MIB), &WIDE).unwrap();
    let bases: Vec<u64> = p.regions().iter().map(|r| r.base >> 32).collect();
    assert_eq!(bases, [1, 2, 3, 4, 5, 6, 7, 12]);
}

#[test]
fn narrow_cells_clip_addresses_and_split_entries() {
    // RAM 12GiB가 1GiB에서 시작. 크기 셀이 하나면 항목 하나가 4GiB를 못 담음
    let descs = [(CONV, GIB, 12 * GIB / 4096, WB)];
    let block = mem(6 * GIB, 4 * MIB);
    let limits = Limits::from_cells(2, 1);
    let p = plan_of(&descs, &[], &block, &limits).unwrap();
    assert_eq!(p.regions().len(), 4);
    assert!(p.regions().iter().all(|r| r.size <= limits.max_entry));
    assert_eq!(p.kept_bytes, 12 * GIB);
    // 항목 하나의 크기는 크기 셀 하나에 들어가는 최댓값(4GiB에서 정렬 단위를 뺀 값)
    assert_eq!(limits.max_entry, (1 << 32) - ALIGN);
    assert_eq!(p.regions().iter().filter(|r| r.size == limits.max_entry).count(), 2);
    assert_eq!(p.regions().iter().map(|r| r.size).max(), Some(limits.max_entry));
    // 블록은 한 항목 안에 통째로 있음
    assert!(p.regions().iter().any(|r| r.base <= block.base && block.base + block.size <= r.base + r.size));
    // 주소 셀이 하나면 4GiB 위는 말할 수 없으므로 잘라냄
    let low = Limits::from_cells(1, 1);
    assert_eq!(plan_of(&descs, &[], &block, &low), Err(PlanError::BlockNotUsable));
    let p = plan_of(&descs, &[], &mem(2 * GIB, 4 * MIB), &low).unwrap();
    assert_eq!(p.regions(), &[mem(GIB, 3 * GIB)]);
}

#[test]
fn ram_is_clipped_at_the_alias_ceiling() {
    let ceiling = 1u64 << 47;
    let descs = [(CONV, ceiling - 64 * MIB, 128 * MIB / 4096, WB), (CONV, ceiling + GIB, 0x1000, WB)];
    let block = mem(ceiling - 32 * MIB, 4 * MIB);
    let p = plan_of(&descs, &[], &block, &Limits::from_cells(2, 2)).unwrap();
    assert_eq!(p.regions(), &[mem(ceiling - 64 * MIB, 64 * MIB)]);
}

#[test]
fn malformed_maps_are_refused() {
    let good = map_bytes(&[(CONV, 0x1000_0000, 0x100, WB)], 48);
    assert!(Map::new(&good, 48).is_ok());
    assert_eq!(Map::new(&good, 39).err(), Some(PlanError::BadStride));
    assert_eq!(Map::new(&good[..47], 48).err(), Some(PlanError::BadMapSize));
    let odd = map_bytes(&[(CONV, 0x1000_0800, 0x100, WB)], 48);
    assert_eq!(Map::new(&odd, 48).err(), Some(PlanError::Misaligned));
    let huge = map_bytes(&[(CONV, 0x1000_0000, u64::MAX / 2, WB)], 48);
    assert_eq!(Map::new(&huge, 48).err(), Some(PlanError::Overflow));
    let wrap = map_bytes(&[(CONV, u64::MAX & !0xFFF, 2, WB)], 48);
    assert_eq!(Map::new(&wrap, 48).err(), Some(PlanError::Overflow));
    // 크기가 0인 디스크립터는 해가 없으므로 건너뜀. 간격이 더 큰 펌웨어도 읽음
    let zero = map_bytes(&[(CONV, 0x1000_0800, 0, WB), (CONV, 0x1000_0000, 0x400, WB)], 64);
    let map = Map::new(&zero, 64).unwrap();
    assert_eq!((map.count(), map.iter().count()), (2, 1));
    // 작업 공간이 모자라면 계획하지 않음
    let (mut points, mut ranges) = (vec![0u64; 1], vec![mem(0, 0); 8]);
    let r = plan(&map, &[], &mem(0x1000_0000, 4 * MIB), &WIDE, Scratch { points: &mut points, ranges: &mut ranges });
    assert_eq!(r, Err(PlanError::ScratchTooSmall));
}

#[test]
fn the_verifier_catches_what_a_broken_plan_would_emit() {
    let descs = [
        (CONV, 0x1000_0000, 0x1000, WB),
        (RT_DATA, 0x1100_0000, 0x10, WB | 1 << 63),
        (CONV, 0x1101_0000, 0x1000, WB),
        (4, 0x3000_0000, 0x100, WB),
    ];
    let bytes = map_bytes(&descs, 48);
    let map = Map::new(&bytes, 48).unwrap();
    let block = parts(&mem(0x1000_0000, 4 * MIB));
    let good = [mem(0x1000_0000, 16 * MIB), mem(0x1101_0000, 16 * MIB)];
    let check = |regions: &[MemRegion], holes: &[MemRegion], b: &Block, l: &Limits| verify(regions, &map, holes, b, l);
    assert_eq!(check(&good, &[], &block, &WIDE), Ok(()));

    assert_eq!(check(&[], &[], &block, &WIDE), Err(VerifyError::BadCount));
    assert_eq!(check(&[good[0]; 9], &[], &block, &WIDE), Err(VerifyError::BadCount));
    assert_eq!(check(&[mem(0x1000_1000, 16 * MIB)], &[], &block, &WIDE), Err(VerifyError::BadRegion));
    assert_eq!(check(&[good[1], good[0]], &[], &block, &WIDE), Err(VerifyError::Unsorted));
    assert_eq!(check(&[good[0], good[0]], &[], &block, &WIDE), Err(VerifyError::Unsorted));
    // 항목 크기 한계를 넘는 구간, DTB만 다른 구간에 있는 경우
    let narrow = Limits { max_addr: 1 << 47, max_entry: 8 * MIB };
    assert_eq!(check(&good, &[], &block, &narrow), Err(VerifyError::AboveLimit));
    let stray = Block { dtb: 0x1101_0000..0x1102_0000, ..block.clone() };
    assert_eq!(check(&good, &[], &stray, &WIDE), Err(VerifyError::BlockSplit));
    // 크기가 0인 구멍은 아무것도 빼지 않음
    assert_eq!(plan_of(&descs[..1], &[mem(0x1040_1000, 0)], &mem(0x1000_0000, 4 * MIB), &WIDE).unwrap().regions(), &good[..1]);
    let low = Limits { max_addr: 0x1080_0000, max_entry: 1 << 47 };
    assert_eq!(check(&good[..1], &[], &block, &low), Err(VerifyError::AboveLimit));
    // 런타임 데이터를 삼킨 구간, 부트 서비스 메모리, 맵에 없는 주소
    assert_eq!(check(&[mem(0x1000_0000, 33 * MIB)], &[], &block, &WIDE), Err(VerifyError::NotUsable));
    assert_eq!(check(&[good[0], mem(0x3000_0000, MIB)], &[], &block, &WIDE), Err(VerifyError::NotUsable));
    assert_eq!(check(&[good[0], mem(0x5000_0000, MIB)], &[], &block, &WIDE), Err(VerifyError::NotUsable));
    // 겹치는 디스크립터가 있는 맵에서 거부가 덮은 곳
    let both = map_bytes(&[descs[0], (RT_DATA, 0x1080_0000, 4, WB)], 48);
    let overlapped = Map::new(&both, 48).unwrap();
    assert_eq!(verify(&good[..1], &overlapped, &[], &block, &WIDE), Err(VerifyError::Denied));
    assert_eq!(check(&good, &[mem(0x1180_0000, 4096)], &block, &WIDE), Err(VerifyError::Hole));
    // 이미지와 윈도우가 다른 구간에 있거나 어느 구간에도 없음
    let split = Block { window: 0x1101_0000..0x1121_0000, ..block.clone() };
    assert_eq!(check(&good, &[], &split, &WIDE), Err(VerifyError::BlockSplit));
    let outside = Block { image: 0x4000_0000..0x4010_0000, ..block.clone() };
    assert_eq!(check(&good, &[], &outside, &WIDE), Err(VerifyError::BlockSplit));
    let zero_map_bytes = map_bytes(&[(CONV, 0, 0x1000, WB)], 48);
    let zero_map = Map::new(&zero_map_bytes, 48).unwrap();
    assert_eq!(
        verify(&[mem(0, 16 * MIB)], &zero_map, &[], &parts(&mem(0, 4 * MIB)), &WIDE),
        Err(VerifyError::ZeroPage)
    );
}

fn walk_names(blob: &[u8]) -> Vec<String> {
    let fdt = Fdt::new(blob).unwrap();
    let mut cur = fdt.cursor();
    let mut out = Vec::new();
    while let Some(tok) = cur.next().unwrap() {
        match tok {
            Token::Begin { name } => out.push(format!("{}+{}", cur.depth(), String::from_utf8_lossy(name))),
            Token::Prop { name, value } => {
                out.push(format!("{}.{}={}", cur.depth(), String::from_utf8_lossy(name), value.len()))
            }
            Token::End => {}
        }
    }
    out
}

fn rewritten(input: &[u8], memory: &[MemRegion], seed: &[u8]) -> Result<Vec<u8>, RewriteError> {
    // 버퍼에 남은 이전 내용이 결과에 섞이지 않는지 보려고 쓰레기로 채움
    let mut out = vec![0xAAu8; input.len() + 4096];
    let len = rewrite(input, &Edits { memory, seed }, &mut out)?;
    assert!(out[len..].iter().all(|&b| b == 0), "tail not cleared");
    out.truncate(len);
    Ok(out)
}

#[test]
fn the_writer_builds_a_tree_the_kernel_parser_accepts() {
    let mut buf = vec![0xAAu8; 1024];
    let mut w = Writer::new(&mut buf, &[mem(0x8000_0000, 0x1000)], 64).unwrap();
    w.begin(b"").unwrap();
    w.prop(b"#address-cells", &2u32.to_be_bytes()).unwrap();
    w.prop(b"#size-cells", &2u32.to_be_bytes()).unwrap();
    w.begin(b"memory@40000000").unwrap();
    w.prop(b"device_type", b"memory\0").unwrap();
    w.prop(b"reg", &cells(&[0, 0x4000_0000, 0, 0x100_0000])).unwrap();
    w.end().unwrap();
    w.begin(b"other").unwrap();
    w.prop(b"reg", &[]).unwrap();
    w.end().unwrap();
    w.end().unwrap();
    let len = w.finish(7).unwrap();
    let blob = &buf[..len];
    let info = parse_blob(blob, 0..len).unwrap();
    assert_eq!(info.memory(), &[mem(0x4000_0000, 0x100_0000)]);
    assert_eq!(info.reserved(), &[mem(0x8000_0000, 0x1000)]);
    assert_eq!(k0_boot::be32(blob, 28), Ok(7));
    // 같은 이름은 문자열 블록에 한 번만 들어감
    let strings = &blob[k0_boot::be32(blob, 12).unwrap() as usize..];
    assert_eq!(strings.windows(4).filter(|w| w == b"reg\0").count(), 1);
    assert!(buf[len..].iter().all(|&b| b == 0));
}

#[test]
fn the_writer_refuses_what_does_not_fit_or_balance() {
    let mut small = [0u8; 48];
    assert_eq!(Writer::new(&mut small, &[], 0).err(), Some(WriteError::NoSpace));
    let mut buf = [0u8; 96];
    let mut w = Writer::new(&mut buf, &[], 8).unwrap();
    assert_eq!(w.end(), Err(WriteError::Unbalanced));
    assert_eq!(w.prop(b"x", &[]), Err(WriteError::Unbalanced));
    w.begin(b"").unwrap();
    assert_eq!(w.begin(b"a\0b"), Err(WriteError::BadName));
    assert_eq!(w.prop(b"name-too-long", &[]), Err(WriteError::NoSpace));
    assert_eq!(w.prop(b"x", &[0; 64]), Err(WriteError::NoSpace));
    assert_eq!(w.finish(0).err(), Some(WriteError::Unbalanced));
}

#[test]
fn rewriting_replaces_only_the_memory_description() {
    let regions = [mem(0x4000_0000, 0x1c00_0000), mem(0x5d00_0000, 0x200_0000)];
    for (name, tree) in [("virt3", virt3()), ("virt2", virt2()), ("rk3399", rk3399())] {
        let input = tree.blob();
        let out = rewritten(&input, &regions, &[]).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let info = parse_blob(&out, 0..out.len()).unwrap();
        assert_eq!(info.memory(), &regions, "{name}");
        assert!(info.reserved().is_empty(), "{name}");
        assert_eq!(discover_blob(&out), discover_blob(&input), "{name}");
        // 메모리 노드 말고는 노드와 프로퍼티가 그대로임
        let strip = |v: Vec<String>| -> Vec<String> {
            let mut out = Vec::new();
            let mut skipping = false;
            for line in v {
                if line.starts_with("2+") {
                    skipping = line.starts_with("2+memory");
                }
                if !skipping {
                    out.push(line);
                }
            }
            out
        };
        assert_eq!(strip(walk_names(&out)), strip(walk_names(&input)), "{name}");
        // 결과는 입력에만 달려 있음
        assert_eq!(rewritten(&input, &regions, &[]).unwrap(), out, "{name}");
    }
}

#[test]
fn rewriting_drops_every_firmware_claim_about_memory() {
    let mut tree = virt3();
    tree.kids.push(
        N::new("reserved-memory")
            .p("#address-cells", cells(&[2]))
            .p("#size-cells", cells(&[2]))
            .p("ranges", Vec::new())
            .c(N::new("fw@50000000").p("reg", cells(&[0, 0x5000_0000, 0, 0x10_0000]))),
    );
    tree.kids.push(N::new("memory").p("device_type", strs(&["memory"])).p("status", strs(&["disabled"])));
    let chosen = tree.kid("chosen");
    chosen.set("linux,initrd-start", cells(&[0x4800_0000]));
    chosen.set("linux,initrd-end", cells(&[0x4900_0000]));
    let mut b = Builder::new();
    b.memreserve(0x4400_0000, 0x1000);
    tree.write(&mut b);
    let input = b.build();
    assert_eq!(parse_blob(&input, 0..input.len()).unwrap().reserved().len(), 3);

    let regions = [mem(0x4100_0000, 0x100_0000)];
    let out = rewritten(&input, &regions, &[]).unwrap();
    let info = parse_blob(&out, 0..out.len()).unwrap();
    assert_eq!((info.memory(), info.reserved().len()), (&regions[..], 0));
    let names = walk_names(&out);
    assert_eq!(names.iter().filter(|n| n.starts_with("2+memory")).count(), 1);
    assert!(names.contains(&"2+memory@41000000".to_string()));
    assert!(!names.iter().any(|n| n.contains("reserved-memory") || n.contains("initrd") || n.contains("fw@")));
    assert_eq!(discover_blob(&out), discover_blob(&input));
}

#[test]
fn the_seed_is_the_firmware_seed_followed_by_the_loader_seed() {
    let regions = [mem(0x4000_0000, 0x1000_0000)];
    let fw: Vec<u8> = (0..48).collect();
    let loader = [0xEEu8; 32];
    let mut tree = virt3();
    tree.kid("chosen").set("rng-seed", fw.clone());
    tree.kid("chosen").set("kaslr-seed", vec![0x55; 8]);
    // 자식이 있는 chosen(U-Boot의 프레임버퍼 노드 같은 것)에서도 프로퍼티가 자식보다 앞에 옴
    tree.kid("chosen").kids.push(N::new("framebuffer@0").p("status", strs(&["okay"])));
    let input = tree.blob();

    let out = rewritten(&input, &regions, &loader).unwrap();
    let info = parse_blob(&out, 0..out.len()).unwrap();
    let mut want = fw[..32].to_vec();
    want.extend(loader);
    want.extend([0x55u8; 8]);
    assert_eq!(info.entropy(), &want[..]);
    assert_eq!(discover_blob(&out), discover_blob(&input));

    // 로더의 엔트로피가 없으면 펌웨어 것의 앞 32바이트만 남음
    let out = rewritten(&input, &regions, &[]).unwrap();
    assert_eq!(parse_blob(&out, 0..out.len()).unwrap().entropy().len(), 8 + 32);

    // chosen이 없는 트리: 엔트로피가 있으면 만들고 없으면 만들지 않음
    let mut bare = virt3();
    bare.kids.retain(|k| k.name != "chosen");
    let input = bare.blob();
    let out = rewritten(&input, &regions, &loader).unwrap();
    assert_eq!(parse_blob(&out, 0..out.len()).unwrap().entropy(), &loader[..]);
    assert!(walk_names(&out).contains(&"2+chosen".to_string()));
    let out = rewritten(&input, &regions, &[]).unwrap();
    assert!(!walk_names(&out).contains(&"2+chosen".to_string()));
}

#[test]
fn rewriting_refuses_what_it_cannot_express() {
    let regions = [mem(0x4000_0000, 0x1000_0000)];
    let mut twice = virt3();
    twice.kids.push(N::new("chosen").p("rng-seed", vec![1; 64]));
    assert_eq!(rewritten(&twice.blob(), &regions, &[]), Err(RewriteError::DuplicateChosen));

    let input = virt3().blob();
    assert_eq!(rewritten(&input, &[], &[]), Err(RewriteError::BadRegionCount));
    assert_eq!(rewritten(&input, &[regions[0]; 9], &[]), Err(RewriteError::BadRegionCount));
    let mut tiny = vec![0u8; 256];
    let r = rewrite(&input, &Edits { memory: &regions, seed: &[] }, &mut tiny);
    assert_eq!(r, Err(RewriteError::Output(WriteError::NoSpace)));
    assert_eq!(rewritten(&input[..input.len() - 8], &regions, &[]), Err(RewriteError::Input(BootError::BadHeader)));

    // 루트의 크기 셀이 하나인 트리(Raspberry Pi 4 모양)는 4GiB 이상의 크기를 못 담음
    let pi = rpi4().blob();
    assert_eq!(root_cells(&Fdt::new(&pi).unwrap()), Ok((2, 1)));
    assert_eq!(rewritten(&pi, &[mem(0, 4 * GIB)], &[]), Err(RewriteError::RegionTooLarge));
    let out = rewritten(&pi, &[mem(0, 0x3b40_0000), mem(GIB, 3 * GIB)], &[]).unwrap();
    let info = parse_blob(&out, 0..out.len()).unwrap();
    assert_eq!(info.memory(), &[mem(0, 0x3b40_0000), mem(GIB, 3 * GIB)]);
    assert_eq!(discover_blob(&out), discover_blob(&pi));
    // 셀 폭이 1이나 2가 아니면 거부, 생략하면 기본값 2와 1
    let mut odd = virt3();
    odd.set("#address-cells", cells(&[3]));
    assert_eq!(rewritten(&odd.blob(), &regions, &[]), Err(RewriteError::UnsupportedCells));
    let mut none = virt3();
    none.del("#address-cells");
    none.del("#size-cells");
    assert_eq!(root_cells(&Fdt::new(&none.blob()).unwrap()), Ok((2, 1)));
}

/// 준비 한 번에 필요한 것을 묶은 테스트 도구입니다.
struct Bench {
    map: Vec<u8>,
    block_base: u64,
    out: Vec<u8>,
    seed: Vec<u8>,
}

impl Bench {
    fn new(descs: &[D]) -> Self {
        Self { map: map_bytes(descs, 48), block_base: block_in(descs).base, out: vec![0; 64 * 1024], seed: Vec::new() }
    }

    fn run(&mut self, firmware_dtb: &[u8]) -> Result<crate::prepare::Prepared, PrepareError> {
        let map = Map::new(&self.map, 48).unwrap();
        let (np, nr, nh) = Work::need(map.count());
        let (mut points, mut ranges, mut holes) = (vec![0u64; np], vec![mem(0, 0); nr], vec![mem(0, 0); nh]);
        let req = Request {
            firmware_dtb,
            map,
            block_base: self.block_base,
            image_size: MIB,
            image_len: MIB - 8192,
            tail_size: BOOT_WINDOW_SIZE + ALIGN,
            granule: 4096,
            device_limit: 1 << 40,
            seed: &self.seed,
        };
        prepare(&req, Work { points: &mut points, ranges: &mut ranges, holes: &mut holes }, &mut self.out)
    }
}

#[test]
fn prepare_makes_a_dtb_the_kernel_accepts() {
    for (name, descs) in [("qemu", maps::QEMU_EDK2_512M), ("debian", maps::DEBIAN_AAVMF_512M_NOACPI)] {
        for tree in [virt3(), virt2()] {
            let input = tree.blob();
            assert_eq!(firmware_dtb_size(&input), Some(input.len()));
            let mut bench = Bench::new(descs);
            bench.seed = vec![7; 32];
            let done = bench.run(&input).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let out = &bench.out[..done.dtb_len];
            let info = parse_blob(out, 0..out.len()).unwrap();
            assert_eq!(info.memory(), done.plan.regions(), "{name}");
            assert_eq!(info.entropy(), &[7u8; 32][..], "{name}");
            assert_eq!(Some(done.platform), discover_blob(&input).ok().map(|d| d.platform), "{name}");
            // 펌웨어 DTB는 512MiB 전체를 RAM이라 했지만 결과는 그보다 작음
            assert!(done.plan.kept_bytes < 512 * MIB && done.plan.kept_bytes > 400 * MIB, "{name}");
            assert!(done.boot_services_bytes > 16 * MIB, "{name}");
        }
    }
    assert_eq!(firmware_dtb_size(&[0xd0, 0x0d, 0xfe, 0xed, 0, 0, 0, 39]), None);
    assert_eq!(firmware_dtb_size(&[0xd0, 0x0d, 0xfe, 0xed, 0, 0x20, 0, 1]), None);
    assert_eq!(firmware_dtb_size(&[0, 0, 0, 0, 0, 0, 1, 0]), None);
}

#[test]
fn prepare_refuses_devices_that_sit_in_firmware_memory() {
    // 콘솔 레지스터가 런타임 서비스 데이터 한가운데에 있다고 말하는 DTB
    let rt = maps::QEMU_EDK2_512M.iter().find(|d| d.0 == RT_DATA).unwrap().1;
    let mut tree = virt3();
    tree.kid("pl011@9000000").set("reg", cells(&[0, rt as u32, 0, 0x1000]));
    let mut bench = Bench::new(maps::QEMU_EDK2_512M);
    assert_eq!(bench.run(&tree.blob()).err(), Some(PrepareError::Validate(ValidateError::DeviceOverlapsMemory)));

    // 빈 메모리 안에 있다고 말하면 그 페이지를 RAM에서 빼고 받아들임
    let mut tree = virt3();
    tree.kid("pl011@9000000").set("reg", cells(&[0, 0x4100_0000, 0, 0x1000]));
    let mut bench = Bench::new(maps::QEMU_EDK2_512M);
    bench.block_base = 0x4200_0000;
    let done = bench.run(&tree.blob()).unwrap();
    assert!(!covered(done.plan.regions(), 0x4100_0000));
    assert!(covered(done.plan.regions(), 0x40ff_c000 - 1) && covered(done.plan.regions(), 0x4100_4000));
}

#[test]
fn prepare_takes_reservations_from_the_kernel_parser() {
    // 펌웨어 예약이 블록에 걸치면 블록을 다른 곳에 잡아야 함
    let mut b = Builder::new();
    let mut bench = Bench::new(maps::QEMU_EDK2_512M);
    b.memreserve(bench.block_base + MIB, 4096);
    virt3().write(&mut b);
    assert_eq!(bench.run(&b.build()).err(), Some(PrepareError::Plan(PlanError::BlockNotUsable)));

    // 커널의 파서가 거부하는 펌웨어 DTB는 로더도 거부함
    let mut limited = virt3();
    limited.kid("chosen").set("linux,usable-memory-range", cells(&[0, 0x4000_0000, 0, 0x1000_0000]));
    let mut bench = Bench::new(maps::QEMU_EDK2_512M);
    assert_eq!(bench.run(&limited.blob()).err(), Some(PrepareError::FirmwareDtb(BootError::MemoryLimited)));
    let mut no_timer = virt3();
    no_timer.kids.retain(|k| k.name != "timer");
    assert!(matches!(bench.run(&no_timer.blob()), Err(PrepareError::FirmwareDiscover(_))));

    // 디바이스가 이 CPU의 물리 주소 폭을 넘으면 커널의 대조가 거부함
    let mut far = virt3();
    far.kid("pl011@9000000").set("reg", cells(&[0x200, 0, 0, 0x1000]));
    let mut bench = Bench::new(maps::QEMU_EDK2_512M);
    assert_eq!(bench.run(&far.blob()).err(), Some(PrepareError::Validate(ValidateError::DeviceAboveLimit)));

    // 커널이 받는 2MiB를 넘는 DTB는 버퍼에 들어가더라도 거부
    let mut big = virt3();
    big.kids.push(N::new("blob").p("data", vec![0x5a; 2 * 1024 * 1024]));
    let mut bench = Bench::new(maps::QEMU_EDK2_512M);
    bench.out = vec![0; 5 * 1024 * 1024];
    // 블록이 커졌으므로 넉넉한 구간에 둠
    bench.block_base = 0x4800_0000;
    assert_eq!(bench.run(&big.blob()).err(), Some(PrepareError::OutputTooLarge));

    // 블록 배치가 어긋나면 시작하지 않음
    let mut bench = Bench::new(maps::QEMU_EDK2_512M);
    bench.block_base += 4096;
    assert_eq!(bench.run(&virt3().blob()).err(), Some(PrepareError::BadLayout));
}

fn fuzz_cases(default: u64) -> (Option<u64>, u64, u64) {
    let only = env_u64("K0_FUZZ_CASE");
    let base = env_u64("K0_FUZZ_SEED").unwrap_or(0x6B30_5F45_4649_5F31);
    let cases = if only.is_some() { 1 } else { env_u64("K0_FUZZ_ITERS").unwrap_or(default) };
    (only, splitmix(base), cases)
}

fn random_map(rng: &mut Rng) -> Vec<D> {
    let kinds = [CONV, CONV, CONV, CONV, 1, 2, 3, 4, 5, RT_DATA, 0, 9, 10, 11, 14, 0x8000_0000];
    let attrs = [WB, WB, WB, WB, 0xF, WB | 1 << 63, 1, 0, WB | 0x4_0000, WB | 0x8000];
    let mut descs = Vec::new();
    let mut at = rng.below(64) * 16 * MIB;
    for _ in 0..1 + rng.below(24) {
        let kind = kinds[rng.index(kinds.len())];
        let attr = attrs[rng.index(attrs.len())];
        let pages = match rng.below(8) {
            0 => 0,
            1 => 1 + rng.below(4),
            2 => 0x10_0000 + rng.below(0x40_0000),
            _ => 1 + rng.below(0x4000),
        };
        descs.push((kind, at, pages, attr));
        at = match rng.below(6) {
            // 겹침, 맞닿음, 4KiB 틈, 큰 틈
            0 => at.saturating_sub(rng.below(pages + 1) * 4096),
            1 | 2 => at + pages * 4096,
            3 => at + pages * 4096 + 4096,
            _ => at + pages * 4096 + rng.below(0x2000) * 4096,
        };
    }
    // 순서에 기대지 않는지 보려고 섞음
    for i in (1..descs.len()).rev() {
        descs.swap(i, rng.index(i + 1));
    }
    descs
}

fn random_block(rng: &mut Rng, descs: &[D]) -> MemRegion {
    let size = 4 * MIB;
    let good = |d: &&D| matches!(d.0, CONV | 1 | 2) && d.3 & WB != 0 && d.3 >> 15 == 0 && d.2 * 4096 >= size + ALIGN;
    let candidates: Vec<&D> = descs.iter().filter(good).collect();
    if candidates.is_empty() || rng.below(16) == 0 {
        return mem(rng.below(1 << 20) * ALIGN, size);
    }
    let d = candidates[rng.index(candidates.len())];
    let room = (d.2 * 4096 - size) / ALIGN;
    mem(((d.1 + ALIGN - 1) & !(ALIGN - 1)) + rng.below(room) * ALIGN, size)
}

#[test]
fn fuzz_plan_with_random_maps() {
    let (only, stream, cases) = fuzz_cases(20_000);
    let (mut accepted, mut rejected) = (0u64, 0u64);
    for i in 0..cases {
        let seed = only.unwrap_or_else(|| splitmix(stream.wrapping_add(i)));
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut rng = Rng::new(seed);
            let descs = random_map(&mut rng);
            let block = random_block(&mut rng, &descs);
            let holes: Vec<MemRegion> = (0..rng.below(5))
                .map(|_| mem(rng.below(1 << 24) * 4096, 4096 * (1 + rng.below(64))))
                .collect();
            let limits = [WIDE, Limits::from_cells(2, 1), Limits::from_cells(1, 2), Limits::from_cells(1, 1)]
                [rng.index(4)];
            let Ok(p) = plan_of(&descs, &holes, &block, &limits) else { return false };

            let bytes = map_bytes(&descs, 48);
            let map = Map::new(&bytes, 48).unwrap();
            // 계획과 다른 방법으로 쓴 검증이 같은 결론을 내야 함
            assert_eq!(verify(p.regions(), &map, &holes, &parts(&block), &limits), Ok(()));
            assert!(p.kept_bytes <= p.usable_bytes);
            if p.dropped == 0 {
                assert_eq!(p.kept_bytes, p.usable_bytes);
            }
            // 무작위로 찍은 주소로 한 번 더 확인
            for r in p.regions() {
                for _ in 0..8 {
                    let a = r.base + rng.below(r.size);
                    assert!(map.iter().any(|d| d.usable() && d.start <= a && a < d.end), "{a:#x} is not usable memory");
                    assert!(!map.iter().any(|d| !d.usable() && d.start <= a && a < d.end), "{a:#x} is denied");
                    assert!(!holes.iter().any(|h| h.base <= a && a - h.base < h.size), "{a:#x} is in a hole");
                    assert!(a < limits.max_addr);
                }
            }
            true
        }));
        match r {
            Ok(true) => accepted += 1,
            Ok(false) => rejected += 1,
            Err(_) => panic!("fuzz case failed, reproduce with K0_FUZZ_CASE={seed:#x} (case {i})"),
        }
    }
    eprintln!("fuzz: {cases} memory map cases, accepted {accepted}, rejected {rejected}");
    if only.is_none() && cases >= 1000 {
        assert!(accepted * 5 >= cases, "too few maps were accepted");
        assert!(rejected > 0, "no map was ever rejected");
    }
}

#[test]
fn fuzz_rewrite_with_mutated_trees() {
    let (only, stream, cases) = fuzz_cases(20_000);
    let mut seeded = virt3();
    seeded.kid("chosen").set("rng-seed", vec![9; 40]);
    seeded.kids.push(N::new("reserved-memory").c(N::new("r@1000").p("reg", cells(&[0, 0x1000, 0, 0x1000]))));
    let seeds = [virt3().blob(), virt2().blob(), rpi4().blob(), rk3399().blob(), seeded.blob()];
    let regions = [mem(0x40_0000, 0x3000_0000), mem(0x4000_0000, 0x4000_0000)];
    let (mut accepted, mut rejected, mut checked) = (0u64, 0u64, 0u64);
    for i in 0..cases {
        let seed = only.unwrap_or_else(|| splitmix(stream.wrapping_add(i)));
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut rng = Rng::new(seed);
            let mut input = seeds[rng.index(seeds.len())].clone();
            if rng.below(8) != 0 {
                mutate(&mut rng, &mut input);
            }
            let n = rng.index(40);
            let loader_seed = rng.bytes(n);
            let mut out = vec![0xAAu8; input.len() + 512];
            let Ok(len) = rewrite(&input, &Edits { memory: &regions, seed: &loader_seed }, &mut out) else {
                return (false, false);
            };
            // 출력은 언제나 구조가 온전한 DTB임
            let made = &out[..len];
            let fdt = Fdt::new(made).expect("output header");
            let mut cur = fdt.cursor();
            while cur.next().expect("output structure").is_some() {}
            // 커널의 파서가 입력을 받아들였다면 출력도 받아들이고 메모리는 요청한 그대로임
            let Ok(before) = parse_blob(&input, 0..input.len()) else { return (true, false) };
            let after = parse_blob(made, 0..len).expect("output must stay acceptable");
            assert_eq!(after.memory(), &regions);
            assert!(after.reserved().is_empty());
            assert!(after.entropy().len() <= before.entropy().len().max(32) + 32);
            (true, true)
        }));
        match r {
            Ok((ok, both)) => {
                accepted += u64::from(ok);
                rejected += u64::from(!ok);
                checked += u64::from(both);
            }
            Err(_) => panic!("fuzz case failed, reproduce with K0_FUZZ_CASE={seed:#x} (case {i})"),
        }
    }
    eprintln!("fuzz: {cases} rewrite cases, accepted {accepted} (kernel-valid inputs {checked}), rejected {rejected}");
    if only.is_none() && cases >= 1000 {
        assert!(checked * 20 >= cases, "too few kernel-valid inputs");
        assert!(rejected > 0, "no input was ever rejected");
    }
}

#[test]
fn fuzz_prepare_end_to_end() {
    let (only, stream, cases) = fuzz_cases(5_000);
    let trees = [virt3().blob(), virt2().blob()];
    let all = [maps::QEMU_EDK2_512M, maps::QEMU_EDK2_16G_NOACPI, maps::DEBIAN_AAVMF_512M_NOACPI];
    let (mut accepted, mut rejected) = (0u64, 0u64);
    for i in 0..cases {
        let seed = only.unwrap_or_else(|| splitmix(stream.wrapping_add(i)));
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut rng = Rng::new(seed);
            // 측정한 맵의 디스크립터를 몇 개 바꾸고 섞음
            let mut descs = all[rng.index(all.len())].to_vec();
            for _ in 0..rng.below(4) {
                let j = rng.index(descs.len());
                match rng.below(3) {
                    0 => descs[j].0 = rng.below(16) as u32,
                    1 => descs[j].3 ^= 1 << rng.below(20),
                    _ => descs.swap(j, 0),
                }
            }
            let mut input = trees[rng.index(trees.len())].clone();
            if rng.below(3) == 0 {
                mutate(&mut rng, &mut input);
            }
            if !descs.iter().any(|d| d.0 == CONV && d.3 & WB != 0 && d.2 * 4096 >= 8 * MIB) {
                return false;
            }
            let mut bench = Bench::new(&descs);
            let n = rng.index(33);
            bench.seed = rng.bytes(n);
            let Ok(done) = bench.run(&input) else { return false };
            let out = &bench.out[..done.dtb_len];
            let info = parse_blob(out, 0..out.len()).expect("kernel parser");
            assert_eq!(info.memory(), done.plan.regions());
            let map = Map::new(&bench.map, 48).unwrap();
            for r in info.memory() {
                assert!(!map.iter().any(|d| !d.usable() && d.start < r.base + r.size && r.base < d.end));
            }
            true
        }));
        match r {
            Ok(true) => accepted += 1,
            Ok(false) => rejected += 1,
            Err(_) => panic!("fuzz case failed, reproduce with K0_FUZZ_CASE={seed:#x} (case {i})"),
        }
    }
    eprintln!("fuzz: {cases} prepare cases, accepted {accepted}, rejected {rejected}");
    if only.is_none() && cases >= 1000 {
        assert!(accepted * 4 >= cases, "too few cases were accepted");
        assert!(rejected > 0, "no case was ever rejected");
    }
}
