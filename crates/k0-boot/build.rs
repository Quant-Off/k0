//! 루트 태스크 임베드 준비 모듈입니다.
//!
//! # Features
//! root-task를 별도 타겟 디렉터리에서 빌드하고, 그 ELF를 호스트에서 직접
//! 파싱해 flat 이미지 + 세그먼트 메타데이터로 변환합니다. 커널이 ELF
//! 로더를 갖지 않기 위한 조치입니다. 변환 중에 W^X(RWX 세그먼트 금지),
//! 16KiB 정렬(양 플랫폼 그래뉼 계약), 진입점의 실행 세그먼트 소속을
//! 강제하고 위반은 빌드 실패로 즉시 드러납니다. flat 이미지의 SHA-256
//! 기준 해시를 생성 파일(roottask_gen.rs)에 함께 굽습니다. 시작 시
//! vendored sha2 크레이트를 표준 테스트 벡터로 교차 검증합니다.

use sha2::{Digest, Sha256};

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const ABC_DIGEST: [u8; 32] = [
    0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22,
    0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00,
    0x15, 0xad,
];
const EMPTY_DIGEST: [u8; 32] = [
    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
    0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
    0xb8, 0x55,
];

/// 양 플랫폼 그래뉼(4K/16K)을 모두 만족하는 세그먼트 정렬 계약
const SEG_ALIGN: u64 = 16 * 1024;

/// flat 이미지 크기 상한, 부트 프레임 윈도우 예산을 지키기 위한 방어선
const MAX_FLAT_SIZE: u64 = 1024 * 1024;

/// SHA-256 해시를 고정 크기 배열로 계산하는 함수입니다.
///
/// # Arguments
/// `data` - 해시할 바이트열
fn digest(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// ELF의 적재 세그먼트 하나를 나타내는 구조체입니다.
struct LoadSeg {
    vaddr: u64,
    offset: u64,
    filesz: u64,
    memsz: u64,
    flags: u32,
}

fn le16(b: &[u8], off: usize) -> u64 {
    u64::from(u16::from_le_bytes(b[off..off + 2].try_into().unwrap()))
}

fn le32(b: &[u8], off: usize) -> u64 {
    u64::from(u32::from_le_bytes(b[off..off + 4].try_into().unwrap()))
}

fn le64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

/// ELF64를 검증하며 진입점과 PT_LOAD 세그먼트 목록을 얻는 함수입니다.
///
/// # Arguments
/// `elf` - ELF 파일 전체 바이트
///
/// # Errors
/// 형식 위반은 전부 panic으로 빌드를 실패시킵니다(fail-secure)
fn parse_elf(elf: &[u8]) -> (u64, Vec<LoadSeg>) {
    assert!(elf.len() >= 64, "ELF 헤더가 잘림");
    assert_eq!(&elf[0..4], b"\x7fELF", "ELF 매직 불일치");
    assert_eq!(elf[4], 2, "ELF64가 아님");
    assert_eq!(elf[5], 1, "리틀 엔디언이 아님");
    assert_eq!(le16(elf, 18), 183, "AArch64가 아님");
    assert_eq!(le16(elf, 16), 2, "ET_EXEC(고정 배치)가 아님");

    let entry = le64(elf, 24);
    let phoff = le64(elf, 32) as usize;
    let phentsize = le16(elf, 54) as usize;
    let phnum = le16(elf, 56) as usize;
    assert_eq!(phentsize, 56, "프로그램 헤더 크기 불일치");
    assert!(
        phoff.checked_add(phentsize * phnum).is_some_and(|e| e <= elf.len()),
        "프로그램 헤더 테이블이 잘림"
    );

    let mut segs = Vec::new();
    for i in 0..phnum {
        let p = phoff + i * phentsize;
        if le32(elf, p) != 1 {
            continue; // PT_LOAD만
        }
        let seg = LoadSeg {
            flags: le32(elf, p + 4) as u32,
            offset: le64(elf, p + 8),
            vaddr: le64(elf, p + 16),
            filesz: le64(elf, p + 32),
            memsz: le64(elf, p + 40),
        };
        assert!(seg.filesz <= seg.memsz, "filesz > memsz");
        assert!(
            (seg.offset.checked_add(seg.filesz)).is_some_and(|e| e <= elf.len() as u64),
            "세그먼트 데이터가 잘림"
        );
        assert!(seg.vaddr.checked_add(seg.memsz).is_some(), "세그먼트 주소 오버플로");
        if seg.memsz > 0 {
            segs.push(seg);
        }
    }
    segs.sort_by_key(|s| s.vaddr);
    (entry, segs)
}

fn main() {
    // FIPS 180-4 테스트 벡터로 vendored sha2를 교차 검증
    assert_eq!(digest(b"abc"), ABC_DIGEST, "sha2 크레이트 검증 실패");
    assert_eq!(digest(b""), EMPTY_DIGEST, "sha2 크레이트 검증 실패");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest.parent().unwrap().parent().unwrap();

    // 바깥 cargo와의 락 충돌을 피하려고 전용 타겟 디렉터리에서 빌드
    let rt_target = out_dir.join("root-task-target");
    let cargo = env::var("CARGO").unwrap();
    let status = Command::new(&cargo)
        .current_dir(workspace)
        .env("CARGO_TARGET_DIR", &rt_target)
        .args([
            "build",
            "-p",
            "root-task",
            "--release",
            "--target",
            "aarch64-unknown-none-softfloat",
        ])
        .status()
        .expect("root-task 빌드 실행 실패");
    assert!(status.success(), "root-task 빌드 실패");

    let elf_path = rt_target.join("aarch64-unknown-none-softfloat/release/root-task");
    let elf = fs::read(&elf_path).expect("root-task 바이너리 읽기 실패");

    // flat 변환: 커널은 ELF를 해석하지 않고 이 결과만 신뢰함
    let (entry, segs) = parse_elf(&elf);
    assert!(!segs.is_empty(), "PT_LOAD 세그먼트 없음");
    assert!(segs.len() <= 4, "세그먼트가 너무 많음");

    let base = segs[0].vaddr;
    assert_eq!(base % SEG_ALIGN, 0, "베이스가 16K 정렬이 아님");
    let mut prev_end = base;
    let mut entry_in_text = false;
    for s in &segs {
        // 배치 계약: 16K 정렬, 오름차순 비중첩, W^X
        assert_eq!(s.vaddr % SEG_ALIGN, 0, "세그먼트가 16K 정렬이 아님");
        assert!(s.vaddr >= prev_end, "세그먼트 겹침");
        prev_end = s.vaddr + s.memsz;
        assert!(s.flags & 0b011 != 0b011, "W^X 위반(RWX 또는 WX 세그먼트)");
        if s.flags & 1 != 0 && (s.vaddr..s.vaddr + s.memsz).contains(&entry) {
            entry_in_text = true;
        }
    }
    assert!(entry_in_text, "진입점이 실행 세그먼트 밖에 있음");

    let span = prev_end.div_ceil(SEG_ALIGN) * SEG_ALIGN - base;
    assert!(span <= MAX_FLAT_SIZE, "flat 이미지가 상한 초과");

    // bss까지 0으로 채운 flat 이미지 구성
    let mut flat = vec![0u8; span as usize];
    for s in &segs {
        let dst = (s.vaddr - base) as usize;
        let src = s.offset as usize;
        flat[dst..dst + s.filesz as usize].copy_from_slice(&elf[src..src + s.filesz as usize]);
    }
    let hash = digest(&flat);
    let flat_path = out_dir.join("root-task.bin");
    fs::write(&flat_path, &flat).expect("flat 이미지 쓰기 실패");

    let mut seg_lines = String::new();
    for s in &segs {
        let kind = match (s.flags & 1 != 0, s.flags & 2 != 0) {
            (true, _) => "Text",
            (false, true) => "Rw",
            (false, false) => "Ro",
        };
        seg_lines.push_str(&format!(
            "    crate::roottask::RtSegment {{ va: {:#x}, memsz: {:#x}, kind: crate::roottask::RtSegKind::{} }},\n",
            s.vaddr, s.memsz, kind
        ));
    }
    let generated = format!(
        "pub static ROOT_TASK_IMAGE: &[u8] = include_bytes!(\"{}\");\n\
         pub const ROOT_TASK_SHA256: [u8; 32] = {:?};\n\
         pub const ROOT_TASK_BASE: u64 = {:#x};\n\
         pub const ROOT_TASK_ENTRY: u64 = {:#x};\n\
         pub static ROOT_TASK_SEGMENTS: &[crate::roottask::RtSegment] = &[\n{}];\n",
        flat_path.display(),
        hash,
        base,
        entry,
        seg_lines
    );
    fs::write(out_dir.join("roottask_gen.rs"), generated).unwrap();

    for rel in [
        "userspace/root-task/src/main.rs",
        "userspace/root-task/Cargo.toml",
        "userspace/root-task/build.rs",
        "userspace/root-task/user.ld",
        "crates/k0-abi/src/lib.rs",
    ] {
        println!("cargo::rerun-if-changed={}", workspace.join(rel).display());
    }
}
