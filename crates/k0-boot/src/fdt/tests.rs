use super::builder::*;
use super::*;

const DTB_AT: usize = 0x4800_0000;

fn run(blob: &[u8]) -> Result<BootInfo, BootError> {
    parse_blob(blob, DTB_AT..DTB_AT + blob.len())
}

fn check(board: &Board) {
    let blob = board.blob();
    let info = run(&blob).unwrap_or_else(|e| panic!("{e:?} for {board:?}"));
    assert_eq!(info.memory(), board.expected_memory().as_slice());
    assert_eq!(info.reserved(), board.expected_reserved().as_slice());
    assert_eq!(info.entropy(), board.expected_entropy().as_slice());
    assert_eq!(info.dtb, DTB_AT..DTB_AT + blob.len());
}

fn err(blob: &[u8]) -> BootError {
    match run(blob) {
        Ok(_) => panic!("expected rejection"),
        Err(e) => e,
    }
}

struct Aligned(Vec<u64>);

impl Aligned {
    fn new(blob: &[u8]) -> Self {
        let mut words = vec![0u64; blob.len().div_ceil(8)];
        // SAFETY: words는 blob 이상 크기의 소유 버퍼이고 바이트 복사만 수행함
        unsafe {
            core::ptr::copy_nonoverlapping(blob.as_ptr(), words.as_mut_ptr() as *mut u8, blob.len());
        }
        Self(words)
    }

    fn addr(&self) -> usize {
        self.0.as_ptr() as usize
    }
}

#[test]
fn qemu_virt_board_parses() {
    let board = Board::qemu_virt();
    check(&board);
    let info = run(&board.blob()).unwrap();
    assert_eq!(info.memory(), &[MemRegion { base: 0x4000_0000, size: 0x2000_0000 }]);
    assert_eq!(info.entropy().len(), 40);
}

#[test]
fn nop_tokens_are_ignored() {
    let board = Board::qemu_virt();
    let mut b = Builder::new();
    board.write(&mut b, &mut |b| {
        b.nop();
    });
    let info = run(&b.build()).unwrap();
    assert_eq!(info.memory(), board.expected_memory().as_slice());
    assert_eq!(info.entropy(), board.expected_entropy().as_slice());
}

#[test]
fn version_window_is_enforced() {
    for (version, last, ok) in [(17, 16, true), (20, 17, true), (16, 16, false), (17, 18, false)] {
        let mut b = Builder::new();
        b.version = version;
        b.last_comp_version = last;
        Board::qemu_virt().write(&mut b, &mut |_| {});
        let r = run(&b.build());
        if ok {
            assert!(r.is_ok(), "{version}/{last}");
        } else {
            assert_eq!(r.err(), Some(BootError::UnsupportedVersion), "{version}/{last}");
        }
    }
}

#[test]
fn header_offsets_are_validated() {
    let good = Board::qemu_virt().blob();
    let len = good.len() as u32;
    let off_struct = get_be32(&good, OFF_DT_STRUCT);
    let size_struct = get_be32(&good, OFF_SIZE_STRUCT);
    let off_strings = get_be32(&good, OFF_DT_STRINGS);
    let cases = [
        (OFF_DT_STRUCT, off_struct + 2),
        (OFF_SIZE_STRUCT, size_struct - 2),
        (OFF_MEM_RSVMAP, 44),
        (OFF_SIZE_STRUCT, len),
        (OFF_DT_STRUCT, u32::MAX - 3),
        (OFF_SIZE_STRINGS, len),
        (OFF_DT_STRINGS, u32::MAX),
        (OFF_DT_STRINGS, off_strings + len),
    ];
    for (field, value) in cases {
        let mut blob = good.clone();
        set_be32(&mut blob, field, value);
        assert_eq!(err(&blob), BootError::BadHeader, "field {field} = {value:#x}");
    }
}

#[test]
fn short_blobs_are_truncated() {
    let good = Board::qemu_virt().blob();
    for n in [0, 4, 12, 39] {
        assert_eq!(err(&good[..n]), BootError::Truncated, "{n} bytes");
    }
}

#[test]
fn unterminated_memreserve_is_truncated() {
    let mut blob = Board::qemu_virt().blob();
    let off = (blob.len() - 16) / 8 * 8;
    set_be32(&mut blob, OFF_MEM_RSVMAP, off as u32);
    blob[off..].fill(0x01);
    assert_eq!(err(&blob), BootError::Truncated);
    set_be32(&mut blob, OFF_MEM_RSVMAP, 41);
    assert_eq!(err(&blob), BootError::BadHeader);
}

#[test]
fn structural_violations_are_rejected() {
    let mut b = Builder::new();
    b.begin("").token(0x5).end();
    assert_eq!(err(&b.build()), BootError::BadStructure);

    let mut b = Builder::new();
    b.begin("").end().end();
    assert_eq!(err(&b.build()), BootError::BadStructure);

    let mut b = Builder::new();
    b.prop("model", b"x\0").begin("").end();
    assert_eq!(err(&b.build()), BootError::BadStructure);

    let mut b = Builder::new();
    b.begin("").begin("memory");
    assert_eq!(err(&b.build()), BootError::BadStructure);

    let mut b = Builder::new();
    b.begin("root").end();
    assert_eq!(err(&b.build()), BootError::BadStructure);

    let mut b = Builder::new();
    b.terminate = false;
    Board::qemu_virt().write(&mut b, &mut |_| {});
    assert_eq!(err(&b.build()), BootError::BadStructure);

    let mut b = Builder::new();
    b.terminate = false;
    b.begin("").token(BEGIN_NODE).raw(b"abcd");
    assert_eq!(err(&b.build()), BootError::BadStructure);
}

#[test]
fn property_names_must_resolve_in_the_strings_block() {
    let mut b = Builder::new();
    b.begin("").prop_at(0x1000, b"").end();
    assert_eq!(err(&b.build()), BootError::BadString);

    let mut blob = Board::qemu_virt().blob();
    let off_strings = get_be32(&blob, OFF_DT_STRINGS) as usize;
    let last = blob.len() - 1;
    assert_eq!(blob[last], 0);
    blob[last] = b'x';
    assert!(last > off_strings);
    assert_eq!(err(&blob), BootError::BadString);
}

#[test]
fn property_values_must_stay_in_the_structure_block() {
    let mut b = Builder::new();
    let name = b.string_offset("compatible");
    b.begin("")
        .token(PROP)
        .raw(&0x100u32.to_be_bytes())
        .raw(&name.to_be_bytes())
        .end();
    assert_eq!(err(&b.build()), BootError::Truncated);

    let mut b = Builder::new();
    b.begin("")
        .token(PROP)
        .raw(&u32::MAX.to_be_bytes())
        .raw(&name.to_be_bytes())
        .end();
    assert_eq!(err(&b.build()), BootError::Truncated);
}

#[test]
fn nesting_depth_is_capped() {
    let build = |depth: usize| {
        let mut b = Builder::new();
        b.begin("").begin("memory").prop("reg", &reg(&[(0x4000_0000, 0x1000)], 2, 1)).end();
        for i in 1..depth {
            b.begin(&format!("n{i}"));
        }
        for _ in 1..depth {
            b.end();
        }
        b.end();
        b.build()
    };
    assert!(run(&build(32)).is_ok());
    assert_eq!(err(&build(33)), BootError::TooDeep);
}

#[test]
fn dtb_span_validates_the_header_in_place() {
    let blob = Board::qemu_virt().blob();
    let buf = Aligned::new(&blob);
    assert_eq!(dtb_span(buf.addr()), Ok(buf.addr()..buf.addr() + blob.len()));
    assert_eq!(dtb_span(0), Err(BootError::NullPointer));
    assert_eq!(dtb_span(buf.addr() + 4), Err(BootError::Misaligned));

    for (magic, total, want) in [
        (0xd00d_feedu32, 39u32, BootError::BadHeader),
        (0xd00d_feed, 2 * 1024 * 1024 + 1, BootError::BadHeader),
        (0xfeed_d00d, blob.len() as u32, BootError::BadMagic),
    ] {
        let mut bad = blob.clone();
        set_be32(&mut bad, 0, magic);
        set_be32(&mut bad, OFF_TOTALSIZE, total);
        let buf = Aligned::new(&bad);
        assert_eq!(dtb_span(buf.addr()), Err(want));
    }
}

#[test]
fn parse_rejects_overlap_with_the_kernel_image() {
    let blob = Board::qemu_virt().blob();
    let buf = Aligned::new(&blob);
    let (start, end) = (buf.addr(), buf.addr() + blob.len());
    assert_eq!(parse(start, start..start + 1, 0).err(), Some(BootError::OverlapsKernel));
    assert_eq!(parse(start, end - 1..end + 0x1000, 0).err(), Some(BootError::OverlapsKernel));
    assert_eq!(parse(start, start - 0x1000..start + 0x2000, 0).err(), Some(BootError::OverlapsKernel));
    assert!(parse(start, end..end + 0x1000, 0).is_ok());
    assert!(parse(start, start - 0x1000..start, 0).is_ok());
}

#[test]
fn parse_reads_through_an_alias_but_reports_physical_ranges() {
    let blob = Board::qemu_virt().blob();
    let buf = Aligned::new(&blob);
    let offset = 0x1000_0000;
    let phys = buf.addr() - offset;
    let info = parse(phys, buf.addr()..buf.addr() + blob.len(), offset).expect("alias parse");
    assert_eq!(info.dtb, phys..phys + blob.len());
    assert_eq!(info.memory(), Board::qemu_virt().expected_memory().as_slice());
    assert_eq!(parse(phys, phys..phys + 1, offset).err(), Some(BootError::OverlapsKernel));
    assert_eq!(parse(usize::MAX, 0..0, 2).err(), Some(BootError::BadHeader));
}
