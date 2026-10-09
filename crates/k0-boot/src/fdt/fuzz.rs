use std::panic::{catch_unwind, AssertUnwindSafe};

use super::builder::*;
use super::*;

const DEFAULT_CASES: u64 = 20_000;
const BASE_SEED: u64 = 0x6B30_5F44_5442_465A;
const KERNEL: Range<usize> = 0x4008_0000..0x4030_0000;

const NOISE_NODES: [&str; 16] = [
    "cpus",
    "cpu@0",
    "soc",
    "uart@9000000",
    "timer",
    "psci",
    "pl061@9030000",
    "flash@0",
    "intc@8000000",
    "apb-pclk",
    "fw-cfg@9020000",
    "pcie@10000000",
    "platform-bus@c000000",
    "virtio_mmio@a000000",
    "gpio-keys",
    "pmu",
];

const NOISE_PROPS: [&str; 12] = [
    "compatible",
    "status",
    "interrupts",
    "phandle",
    "linux,phandle",
    "clock-frequency",
    "ranges",
    "device_type",
    "interrupt-parent",
    "dma-coherent",
    "stdout-path",
    "model",
];

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(splitmix(seed) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    fn index(&mut self, n: usize) -> usize {
        self.below(n as u64) as usize
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn env_u64(name: &str) -> Option<u64> {
    let v = std::env::var(name).ok()?;
    let v = v.trim().replace('_', "");
    match v.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => v.parse().ok(),
    }
}

fn noise(rng: &mut Rng, b: &mut Builder, depth: u32) {
    for _ in 0..rng.below(3) {
        match rng.below(4) {
            0 => {
                b.nop();
            }
            1 => {
                let name = NOISE_PROPS[rng.index(NOISE_PROPS.len())];
                let n = rng.index(24);
                b.prop(name, &rng.bytes(n));
            }
            _ if depth < 6 => {
                b.begin(NOISE_NODES[rng.index(NOISE_NODES.len())]);
                noise(rng, b, depth + 1);
                b.end();
            }
            _ => {}
        }
    }
}

fn region(rng: &mut Rng, addr_cells: u32, size_cells: u32) -> (u64, u64) {
    let amax = if addr_cells == 1 { u64::from(u32::MAX) } else { u64::MAX };
    let smax = if size_cells == 1 { u64::from(u32::MAX) } else { u64::MAX };
    let base = match rng.below(4) {
        0 => rng.below(0x1_0000) << 12,
        1 => amax - rng.below(0x1_0000),
        _ => rng.next() & amax,
    };
    let room = (u64::MAX - base).min(smax);
    let size = match rng.below(5) {
        0 => 0,
        1 => room,
        _ => rng.below(room.min(1 << 40)) + 1,
    };
    (base, size.min(room))
}

fn random_board(rng: &mut Rng) -> Board {
    let cells = match rng.below(5) {
        0 => None,
        1 => Some((1, 1)),
        2 => Some((1, 2)),
        3 => Some((2, 1)),
        _ => Some((2, 2)),
    };
    let (ac, sc) = cells.unwrap_or((2, 1));
    let rsv_cells = match rng.below(3) {
        0 => Some((1 + rng.below(2) as u32, 1 + rng.below(2) as u32)),
        _ => None,
    };
    let (rac, rsc) = rsv_cells.unwrap_or((ac, sc));
    let memory = (0..rng.below(4))
        .map(|_| (0..1 + rng.below(4)).map(|_| region(rng, ac, sc)).collect())
        .collect();
    let memreserve = (0..rng.below(4)).map(|_| region(rng, 2, 2)).collect();
    let rsv_children = (0..rng.below(4))
        .map(|_| match rng.below(4) {
            0 => None,
            _ => Some((0..1 + rng.below(2)).map(|_| region(rng, rac, rsc)).collect()),
        })
        .collect();
    let seed = |rng: &mut Rng| match rng.below(3) {
        0 => None,
        _ => {
            let n = rng.index(64);
            Some(rng.bytes(n))
        }
    };
    Board {
        cells,
        memory,
        memreserve,
        rsv_cells,
        rsv_children,
        rng_seed: seed(rng),
        kaslr_seed: seed(rng),
    }
}

fn interesting(rng: &mut Rng, len: usize) -> u32 {
    let len = len as u32;
    let pool = [
        0,
        1,
        2,
        3,
        4,
        8,
        9,
        16,
        17,
        32,
        33,
        40,
        0x7F,
        0x80,
        0xFF,
        0xFFFF,
        0x7FFF_FFFF,
        0x8000_0000,
        u32::MAX,
        u32::MAX - 3,
        len,
        len.wrapping_sub(4),
        len.wrapping_add(4),
        0xd00d_feed,
        MAX_DTB_SIZE,
        MAX_DTB_SIZE + 1,
    ];
    if rng.below(4) == 0 { rng.next() as u32 } else { pool[rng.index(pool.len())] }
}

fn mutate(rng: &mut Rng, blob: &mut Vec<u8>) {
    for _ in 0..1 + rng.below(4) {
        let len = blob.len();
        if len < 8 {
            blob.extend(rng.bytes(8));
            continue;
        }
        match rng.below(10) {
            0 => {
                let i = rng.index(len);
                blob[i] ^= 1 << rng.below(8);
            }
            1 => {
                let i = rng.index(len);
                blob[i] = [0x00, 0x01, 0x7F, 0x80, 0xFF][rng.index(5)];
            }
            2 => {
                let i = rng.index(len / 4) * 4;
                let v = interesting(rng, len);
                set_be32(blob, i, v);
            }
            3 if len >= 40 => {
                let field = rng.index(10) * 4;
                let v = interesting(rng, len);
                set_be32(blob, field, v);
            }
            4 => blob.truncate(rng.index(len)),
            5 => {
                let n = rng.index(64);
                blob.extend(rng.bytes(n));
            }
            6 => {
                let a = rng.index(len);
                let b = rng.index(len);
                let n = rng.index((len - a.max(b)).min(64) + 1);
                blob.copy_within(a..a + n, b);
            }
            7 => {
                let i = rng.index(len / 4) * 4;
                let v = interesting(rng, len);
                blob.splice(i..i, v.to_be_bytes());
            }
            8 => {
                let i = rng.index(len - 3);
                blob.drain(i..i + 4);
            }
            _ if len >= 8 => {
                set_be32(blob, OFF_TOTALSIZE, len as u32);
            }
            _ => {}
        }
    }
}

fn invariants(info: &BootInfo, dtb: Range<usize>) {
    assert!((1..=MAX_MEM_REGIONS).contains(&info.memory().len()));
    assert!(info.reserved().len() <= MAX_RSV_REGIONS);
    for r in info.memory().iter().chain(info.reserved()) {
        assert!(r.size > 0, "zero-sized region {r:?}");
        assert!(r.base.checked_add(r.size).is_some(), "overflowing region {r:?}");
    }
    assert!(info.entropy().len() <= MAX_ENTROPY);
    assert_eq!(info.dtb, dtb);
}

fn board_is_valid(board: &Board) -> bool {
    let mem = board.expected_memory().len();
    let rsv = board.expected_reserved().len();
    (1..=MAX_MEM_REGIONS).contains(&mem) && rsv <= MAX_RSV_REGIONS
}

struct Window {
    words: Vec<u64>,
    dirty: usize,
}

impl Window {
    fn new() -> Self {
        Self {
            words: vec![0; MAX_DTB_SIZE as usize / 8 + 1],
            dirty: 0,
        }
    }

    fn load(&mut self, blob: &[u8]) -> usize {
        // SAFETY: words는 MAX_DTB_SIZE + 8바이트 소유 버퍼이고 blob과 겹치지 않음
        let bytes = unsafe {
            core::slice::from_raw_parts_mut(self.words.as_mut_ptr() as *mut u8, self.words.len() * 8)
        };
        bytes[..self.dirty].fill(0);
        let n = blob.len().min(bytes.len());
        bytes[..n].copy_from_slice(&blob[..n]);
        self.dirty = n;
        self.words.as_ptr() as usize
    }
}

#[derive(Default)]
struct Stats {
    clean_ok: u64,
    mutated_ok: u64,
    mutated_err: u64,
    pointer_ok: u64,
}

fn one_case(seed: u64, window: &mut Window, stats: &mut Stats, verbose: bool) {
    let mut rng = Rng::new(seed);
    let board = random_board(&mut rng);
    let mut b = Builder::new();
    let mut nrng = Rng::new(seed ^ 0xA5A5);
    board.write(&mut b, &mut |b| noise(&mut nrng, b, 2));
    let clean = b.build();
    let dtb = 0x4800_0000..0x4800_0000 + clean.len();

    match parse_blob(&clean, dtb.clone()) {
        Ok(info) => {
            assert!(board_is_valid(&board), "accepted an invalid board {board:?}");
            assert_eq!(info.memory(), board.expected_memory().as_slice());
            assert_eq!(info.reserved(), board.expected_reserved().as_slice());
            assert_eq!(info.entropy(), board.expected_entropy().as_slice());
            invariants(&info, dtb);
            stats.clean_ok += 1;
        }
        Err(e) => assert!(!board_is_valid(&board), "rejected a valid board with {e:?}: {board:?}"),
    }

    let mut blob = clean;
    mutate(&mut rng, &mut blob);
    if verbose {
        eprintln!("fuzz: mutated input ({} bytes)", blob.len());
        for (i, chunk) in blob.chunks(32).enumerate() {
            let hex: String = chunk.iter().map(|b| format!("{b:02x}")).collect();
            eprintln!("{:06x}: {hex}", i * 32);
        }
    }
    let dtb = 0x4800_0000..0x4800_0000 + blob.len();
    match parse_blob(&blob, dtb.clone()) {
        Ok(info) => {
            invariants(&info, dtb);
            stats.mutated_ok += 1;
        }
        Err(_) => stats.mutated_err += 1,
    }

    let addr = window.load(&blob);
    let kernel = if rng.below(8) == 0 { addr..addr + 1 } else { KERNEL };
    if let Ok(info) = parse(addr, kernel.clone(), 0) {
        assert!(info.dtb.end <= kernel.start || kernel.end <= info.dtb.start);
        let len = info.dtb.len();
        assert!((HEADER_SIZE as usize..=MAX_DTB_SIZE as usize).contains(&len));
        invariants(&info, addr..addr + len);
        stats.pointer_ok += 1;
    }
}

#[test]
fn fuzz_parse_with_mutated_boards() {
    let only = env_u64("K0_FUZZ_CASE");
    let base = env_u64("K0_FUZZ_SEED").unwrap_or(BASE_SEED);
    let cases = if only.is_some() { 1 } else { env_u64("K0_FUZZ_ITERS").unwrap_or(DEFAULT_CASES) };
    let mut window = Window::new();
    let mut stats = Stats::default();

    for i in 0..cases {
        let seed = only.unwrap_or_else(|| splitmix(base.wrapping_add(i)));
        let r = catch_unwind(AssertUnwindSafe(|| one_case(seed, &mut window, &mut stats, only.is_some())));
        if r.is_err() {
            panic!("fuzz case failed, reproduce with K0_FUZZ_CASE={seed:#x} (case {i}, base seed {base:#x})");
        }
    }

    eprintln!(
        "fuzz: {cases} cases, clean ok {}, mutated ok {} err {}, pointer ok {}",
        stats.clean_ok, stats.mutated_ok, stats.mutated_err, stats.pointer_ok
    );
    if only.is_none() && cases >= 1000 {
        assert!(stats.clean_ok * 4 >= cases, "generator produced too few valid boards");
        assert!(stats.mutated_ok > 0, "no mutated input was ever accepted");
        assert!(stats.pointer_ok > 0, "pointer path never accepted an input");
    }
}
