//! 제로 트러스트 FDT(DTB) 최소 파서 모듈입니다.
//!
//! # Features
//! 부트로더가 넘긴 DTB를 신뢰하지 않는 입력으로 취급합니다. 헤더의 모든
//! 오프셋과 길이를 검사 산술로 확인하고, 커널 이미지와 겹치는 DTB를 거부한
//! 뒤, 루트의 #address-cells / #size-cells와 /memory 노드의 reg,
//! /reserved-memory 자식 노드들의 reg, memreserve 블록, /chosen 노드의
//! 엔트로피(rng-seed, kaslr-seed)만 읽습니다. 그 외 노드는 해석하지
//! 않습니다. 예약 구간은 untyped에서 제외하는 방향으로만 쓰이기 때문에
//! 악의적 값이어도 자원 축소(부팅 거부)일 뿐 권한 확대가 되지 못합니다.
//! 엔트로피는 PAC 키 파생의 재료일 뿐이라 내용을 검증하지 않고
//! 길이만 상한으로 자릅니다. (악의적 값이어도 다른 재료와 해시로 혼합되어
//! 키를 약화시키지 못함, 단 제공되지 않으면 저엔트로피가 됨)
//!
//! # Errors
//! 형식 위반은 전부 `BootError`로 반환하며, 어떤 경우에도 blob 바깥을 읽지
//! 않습니다. 호출자는 실패 시 부팅을 중단해야 합니다(fail-secure).

use core::ops::Range;

/// 저장 가능한 물리 메모리 영역 수의 상한
pub const MAX_MEM_REGIONS: usize = 8;

/// 저장 가능한 예약 구간(memreserve + /reserved-memory) 수의 상한
pub const MAX_RSV_REGIONS: usize = 8;

/// /chosen에서 수집하는 엔트로피 바이트 상한 (rng-seed + kaslr-seed)
pub const MAX_ENTROPY: usize = 72;

const FDT_MAGIC: u32 = 0xd00d_feed;
const HEADER_SIZE: u32 = 40;
/// 수용 가능한 DTB 최대 크기입니다. 이 값이 곧 진입 페이즈 1에서 DTB에 할당하는
/// 매핑 상한이라서 과대한 totalsize로 페이지 테이블 풀을 고갈시키지 못합니다.
const MAX_DTB_SIZE: u32 = 2 * 1024 * 1024;
const MAX_DEPTH: u32 = 32;
const SUPPORTED_VERSION: u32 = 17;

const FDT_BEGIN_NODE: u32 = 0x1;
const FDT_END_NODE: u32 = 0x2;
const FDT_PROP: u32 = 0x3;
const FDT_NOP: u32 = 0x4;
const FDT_END: u32 = 0x9;

/// 물리 메모리 영역 하나를 나타내는 구조체입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemRegion {
    pub base: u64,
    pub size: u64,
}

/// DTB 파싱 결과를 담는 구조체입니다.
///
/// 진입 페이즈 1의 후속 단계(페이지 테이블 구성)가 이 정보를 입력으로 받습니다.
pub struct BootInfo {
    pub dtb: Range<usize>,
    regions: [MemRegion; MAX_MEM_REGIONS],
    region_count: usize,
    reserved: [MemRegion; MAX_RSV_REGIONS],
    reserved_count: usize,
    entropy: [u8; MAX_ENTROPY],
    entropy_len: usize,
}

impl BootInfo {
    pub fn memory(&self) -> &[MemRegion] {
        &self.regions[..self.region_count]
    }

    /// 부트로더/펌웨어 예약 구간을 주는 함수입니다.
    ///
    /// memreserve 블록과 /reserved-memory 자식 노드의 reg를 합친 목록이며
    /// untyped 목록화와 부트 윈도우 선정에서 제외해야 합니다.
    pub fn reserved(&self) -> &[MemRegion] {
        &self.reserved[..self.reserved_count]
    }

    /// /chosen이 제공한 엔트로피 바이트(rng-seed + kaslr-seed)를 주는 함수입니다.
    ///
    /// 비어 있을 수 있고, 그 경우 PAC 키 파생이 저엔트로피가 되므로 호출자는
    /// 로그로 드러내야 합니다.
    pub fn entropy(&self) -> &[u8] {
        &self.entropy[..self.entropy_len]
    }
}

/// DTB 파싱이 거부된 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootError {
    NullPointer,
    Misaligned,
    BadMagic,
    BadHeader,
    UnsupportedVersion,
    OverlapsKernel,
    Truncated,
    BadStructure,
    TooDeep,
    BadString,
    UnsupportedCells,
    BadReg,
    TooManyRegions,
    NoMemory,
}

/// DTB 헤더만 검사해 blob이 차지하는 물리 범위를 얻는 함수입니다.
///
/// 진입 페이즈 1의 페이지 테이블 구성이 DTB를 매핑하려면 파싱 전에 크기를
/// 알아야 하기 때문에 분리되어 있습니다. 정수 비교만 수행하고 재배치된 포인터를
/// 담은 데이터를 일절 건드리지 않으므로 higher-half 점프 이전에도
/// 안전합니다. (본 파싱은 점프 이후에 수행)
///
/// # Arguments
/// `dtb_phys` - 부트로더가 x0로 넘긴 DTB 물리 주소
///
/// # Errors
/// null pointer, 정렬 위반, 매직 불일치, 크기 범위 위반 시 `BootError`
pub fn dtb_span(dtb_phys: usize) -> Result<Range<usize>, BootError> {
    if dtb_phys == 0 {
        return Err(BootError::NullPointer);
    }
    if dtb_phys % 8 != 0 {
        return Err(BootError::Misaligned);
    }

    // SAFETY: 부트로더가 넘긴 8바이트 정렬 주소의 첫 8바이트(magic, totalsize)만
    //         읽음. MMU OFF거나 해당 범위가 매핑된 상태에서만 호출해야 함
    let magic = unsafe { read_be32_phys(dtb_phys) };
    if magic != FDT_MAGIC {
        return Err(BootError::BadMagic);
    }
    // SAFETY: 위와 동일한 전제, dtb_phys + 4는 4바이트 정렬
    let totalsize = unsafe { read_be32_phys(dtb_phys + 4) };
    if !(HEADER_SIZE..=MAX_DTB_SIZE).contains(&totalsize) {
        return Err(BootError::BadHeader);
    }

    let dtb_end = dtb_phys
        .checked_add(totalsize as usize)
        .ok_or(BootError::BadHeader)?;
    Ok(dtb_phys..dtb_end)
}

/// 물리 주소의 DTB를 검증하고 파싱하는 함수입니다.
///
/// 문자열 비교 등 재배치된 데이터를 사용하므로 higher-half 점프 이후에만
/// 호출할 수 있습니다. 실제 읽기는 `dtb_phys + read_offset`으로 수행하기
/// 때문에 TTBR1 별칭(VA)을 통해 읽으면 TTBR0(identity)이 회수된 뒤에도
/// 동작합니다. 겹침 검사와 결과의 메모리 맵은 물리 주소 기준입니다.
///
/// # Arguments
/// `dtb_phys` - 부트로더가 x0로 넘긴 DTB 물리 주소
/// `kernel_image` - 커널 이미지가 차지하는 물리 범위(겹침 거부용)
/// `read_offset` - 읽기 주소에 더할 오프셋(TTBR1 별칭이면 `KERNEL_VA_OFFSET`)
///
/// # Errors
/// 헤더 형식 위반, 커널 이미지와의 겹침, 구조 블록 형식 위반 시 `BootError`
pub fn parse(
    dtb_phys: usize,
    kernel_image: Range<usize>,
    read_offset: usize,
) -> Result<BootInfo, BootError> {
    let read_base = dtb_phys.checked_add(read_offset).ok_or(BootError::BadHeader)?;
    let dtb_read = dtb_span(read_base)?;
    let len = dtb_read.end - dtb_read.start;
    let dtb = dtb_phys..dtb_phys.checked_add(len).ok_or(BootError::BadHeader)?;
    if dtb.start < kernel_image.end && kernel_image.start < dtb.end {
        return Err(BootError::OverlapsKernel);
    }

    // SAFETY: [dtb_read.start, dtb_read.end)는 dtb_span의 상한 검증을 통과했고
    //         커널 이미지와 겹치지 않으며, 진입 페이즈 1이 이 범위를 RO로 매핑해둿음
    let blob = unsafe { core::slice::from_raw_parts(dtb_read.start as *const u8, len) };

    parse_blob(blob, dtb)
}

/// 물리 주소에서 big-endian u32 하나를 읽는 함수입니다.
///
/// # Arguments
/// `addr` - 읽을 물리 주소
///
/// # Safety
/// `addr`는 4바이트 정렬된 읽기 가능한 물리 주소여야 합니다. (MMU OFF 전제)
unsafe fn read_be32_phys(addr: usize) -> u32 {
    u32::from_be(unsafe { core::ptr::read_volatile(addr as *const u32) })
}

fn be32(blob: &[u8], off: usize) -> Result<u32, BootError> {
    let b = blob.get(off..off.wrapping_add(4)).ok_or(BootError::Truncated)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be64(blob: &[u8], off: usize) -> Result<u64, BootError> {
    let b = blob.get(off..off.wrapping_add(8)).ok_or(BootError::Truncated)?;
    Ok(u64::from_be_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

fn align4(off: usize) -> Result<usize, BootError> {
    off.checked_add(3).map(|v| v & !3).ok_or(BootError::Truncated)
}

fn parse_blob(blob: &[u8], dtb: Range<usize>) -> Result<BootInfo, BootError> {
    let off_struct = be32(blob, 8)? as usize;
    let off_strings = be32(blob, 12)? as usize;
    let version = be32(blob, 20)?;
    let last_comp_version = be32(blob, 24)?;
    let size_strings = be32(blob, 32)? as usize;
    let size_struct = be32(blob, 36)? as usize;

    if version < SUPPORTED_VERSION || last_comp_version > SUPPORTED_VERSION {
        return Err(BootError::UnsupportedVersion);
    }
    // 구조 블록은 4바이트 토큰 열이라 시작과 크기 모두 4의 배수여야 함
    // (크기가 어긋나면 마지막 토큰이 블록 끝을 넘어 걸치게 됨)
    if off_struct % 4 != 0 || size_struct % 4 != 0 {
        return Err(BootError::BadHeader);
    }
    let struct_end = off_struct
        .checked_add(size_struct)
        .filter(|&e| e <= blob.len())
        .ok_or(BootError::BadHeader)?;
    let strings_end = off_strings
        .checked_add(size_strings)
        .filter(|&e| e <= blob.len())
        .ok_or(BootError::BadHeader)?;

    let mut info = BootInfo {
        dtb,
        regions: [MemRegion { base: 0, size: 0 }; MAX_MEM_REGIONS],
        region_count: 0,
        reserved: [MemRegion { base: 0, size: 0 }; MAX_RSV_REGIONS],
        reserved_count: 0,
        entropy: [0; MAX_ENTROPY],
        entropy_len: 0,
    };

    // memreserve 블록: (addr, size) be64 쌍이 (0, 0)으로 끝남
    let off_rsvmap = be32(blob, 16)? as usize;
    if off_rsvmap % 8 != 0 {
        return Err(BootError::BadHeader);
    }
    let mut rsv_off = off_rsvmap;
    loop {
        let base = be64(blob, rsv_off)?;
        let size = be64(blob, rsv_off + 8)?;
        rsv_off = rsv_off.checked_add(16).ok_or(BootError::Truncated)?;
        if base == 0 && size == 0 {
            break;
        }
        if size == 0 {
            continue;
        }
        push_reserved(&mut info, base, size)?;
    }

    // 루트가 셀 크기를 생략하면 DT spec 기본값(2 / 1)을 쓴다
    let mut addr_cells: u32 = 2;
    let mut size_cells: u32 = 1;
    let mut depth: u32 = 0;
    let mut memory_depth: Option<u32> = None;
    let mut chosen_depth: Option<u32> = None;
    // /reserved-memory 자체의 셀 크기(spec상 루트와 같아야 하지만 명시를 우선)
    let mut rsv_node: bool = false;
    let mut rsv_addr_cells: u32 = 2;
    let mut rsv_size_cells: u32 = 1;
    let mut off = off_struct;

    loop {
        if off >= struct_end {
            return Err(BootError::BadStructure);
        }
        let token = be32(blob, off)?;
        off += 4;

        match token {
            FDT_BEGIN_NODE => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(BootError::TooDeep);
                }
                let rel = blob
                    .get(off..struct_end)
                    .ok_or(BootError::BadStructure)?
                    .iter()
                    .position(|&b| b == 0)
                    .ok_or(BootError::BadStructure)?;
                let name = &blob[off..off + rel];
                if depth == 1 && !name.is_empty() {
                    return Err(BootError::BadStructure);
                }
                if depth == 2 && (name == b"memory" || name.starts_with(b"memory@")) {
                    memory_depth = Some(depth);
                }
                if depth == 2 && name == b"chosen" {
                    chosen_depth = Some(depth);
                }
                if depth == 2 && name == b"reserved-memory" {
                    rsv_node = true;
                    rsv_addr_cells = addr_cells;
                    rsv_size_cells = size_cells;
                }
                off = align4(off + rel + 1)?;
            }
            FDT_END_NODE => {
                if depth == 0 {
                    return Err(BootError::BadStructure);
                }
                if memory_depth == Some(depth) {
                    memory_depth = None;
                }
                if chosen_depth == Some(depth) {
                    chosen_depth = None;
                }
                if rsv_node && depth == 2 {
                    rsv_node = false;
                }
                depth -= 1;
            }
            FDT_PROP => {
                if depth == 0 {
                    return Err(BootError::BadStructure);
                }
                let len = be32(blob, off)? as usize;
                let name_off = be32(blob, off + 4)? as usize;
                let val_start = off + 8;
                let val_end = val_start
                    .checked_add(len)
                    .filter(|&e| e <= struct_end)
                    .ok_or(BootError::Truncated)?;
                let name = prop_name(blob, off_strings, strings_end, name_off)?;
                let value = &blob[val_start..val_end];

                if depth == 1 {
                    match name {
                        b"#address-cells" => addr_cells = cell_count(value)?,
                        b"#size-cells" => size_cells = cell_count(value)?,
                        _ => {}
                    }
                } else if memory_depth == Some(depth) && name == b"reg" {
                    push_regions(value, addr_cells, size_cells, &mut info)?;
                } else if rsv_node && depth == 2 {
                    match name {
                        b"#address-cells" => rsv_addr_cells = cell_count(value)?,
                        b"#size-cells" => rsv_size_cells = cell_count(value)?,
                        _ => {}
                    }
                } else if rsv_node && depth == 3 && name == b"reg" {
                    push_reserved_regions(value, rsv_addr_cells, rsv_size_cells, &mut info)?;
                } else if chosen_depth == Some(depth)
                    && (name == b"rng-seed" || name == b"kaslr-seed")
                {
                    push_entropy(value, &mut info);
                }
                off = align4(val_end)?;
            }
            FDT_NOP => {}
            FDT_END => {
                if depth != 0 {
                    return Err(BootError::BadStructure);
                }
                break;
            }
            _ => return Err(BootError::BadStructure),
        }
    }

    if info.region_count == 0 {
        return Err(BootError::NoMemory);
    }
    Ok(info)
}

fn prop_name(
    blob: &[u8],
    off_strings: usize,
    strings_end: usize,
    name_off: usize,
) -> Result<&[u8], BootError> {
    let start = off_strings
        .checked_add(name_off)
        .filter(|&s| s < strings_end)
        .ok_or(BootError::BadString)?;
    let rel = blob[start..strings_end]
        .iter()
        .position(|&b| b == 0)
        .ok_or(BootError::BadString)?;
    Ok(&blob[start..start + rel])
}

fn cell_count(value: &[u8]) -> Result<u32, BootError> {
    if value.len() != 4 {
        return Err(BootError::UnsupportedCells);
    }
    Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}

fn parse_reg(
    value: &[u8],
    addr_cells: u32,
    size_cells: u32,
    out: &mut [MemRegion],
    count: &mut usize,
) -> Result<(), BootError> {
    if !(1..=2).contains(&addr_cells) || !(1..=2).contains(&size_cells) {
        return Err(BootError::UnsupportedCells);
    }
    let entry = ((addr_cells + size_cells) * 4) as usize;
    if value.is_empty() || value.len() % entry != 0 {
        return Err(BootError::BadReg);
    }

    for chunk in value.chunks_exact(entry) {
        let (a, s) = chunk.split_at((addr_cells * 4) as usize);
        let base = read_cells(a);
        let size = read_cells(s);
        if size == 0 {
            continue;
        }
        base.checked_add(size).ok_or(BootError::BadReg)?;
        if *count == out.len() {
            return Err(BootError::TooManyRegions);
        }
        out[*count] = MemRegion { base, size };
        *count += 1;
    }
    Ok(())
}

fn push_regions(
    value: &[u8],
    addr_cells: u32,
    size_cells: u32,
    info: &mut BootInfo,
) -> Result<(), BootError> {
    let mut count = info.region_count;
    parse_reg(value, addr_cells, size_cells, &mut info.regions, &mut count)?;
    info.region_count = count;
    Ok(())
}

/// /reserved-memory 자식 노드의 reg를 예약 목록에 넣는 함수입니다.
///
/// reg 없이 size만 가진 동적 예약 노드는 펌웨어가 고정 주소를 예약한 것이
/// 아니기 때문에 여기 도달하지 않고 무시됩니다.
fn push_reserved_regions(
    value: &[u8],
    addr_cells: u32,
    size_cells: u32,
    info: &mut BootInfo,
) -> Result<(), BootError> {
    let mut count = info.reserved_count;
    parse_reg(value, addr_cells, size_cells, &mut info.reserved, &mut count)?;
    info.reserved_count = count;
    Ok(())
}

/// memreserve 블록의 구간 하나를 예약 목록에 넣는 함수입니다.
fn push_reserved(info: &mut BootInfo, base: u64, size: u64) -> Result<(), BootError> {
    base.checked_add(size).ok_or(BootError::BadReg)?;
    if info.reserved_count == MAX_RSV_REGIONS {
        return Err(BootError::TooManyRegions);
    }
    info.reserved[info.reserved_count] = MemRegion { base, size };
    info.reserved_count += 1;
    Ok(())
}

fn read_cells(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

/// /chosen의 엔트로피 프로퍼티를 상한까지 수집하는 함수입니다.
///
/// 내용은 검증하지 않고(해시 혼합 재료일 뿐) 넘치는 바이트는 버립니다.
fn push_entropy(value: &[u8], info: &mut BootInfo) {
    let cap = MAX_ENTROPY - info.entropy_len;
    let n = value.len().min(cap);
    info.entropy[info.entropy_len..info.entropy_len + n].copy_from_slice(&value[..n]);
    info.entropy_len += n;
}
