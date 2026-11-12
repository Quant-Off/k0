//! 제로 트러스트 FDT(DTB) 최소 파서 모듈입니다.
//!
//! # Features
//! 부트로더가 넘긴 DTB를 신뢰하지 않는 입력으로 취급합니다. 헤더의 모든
//! 오프셋과 길이를 검사 산술로 확인하고, 커널 이미지와 겹치는 DTB를 거부한
//! 뒤, 루트의 #address-cells / #size-cells와 메모리 노드의 reg,
//! /reserved-memory 자식 노드들의 reg, memreserve 블록, /chosen 노드의
//! 엔트로피(rng-seed, kaslr-seed)와 initrd 구간만 읽습니다. 메모리 노드는
//! 이름이 memory이고 device_type이 "memory"이며 사용 가능한 루트 자식만
//! 인정합니다. 예약 구간은 untyped에서 제외하는 방향으로만 쓰이기 때문에
//! 악의적 값이어도 자원 축소(부팅 거부)일 뿐 권한 확대가 되지 못합니다.
//! 메모리를 제한하는 프로퍼티(linux,usable-memory 계열)는 해석하지 않는
//! 대신 있으면 부팅을 거부합니다.
//! 엔트로피는 PAC 키 파생의 재료일 뿐이라 내용을 검증하지 않고
//! 길이만 상한으로 자릅니다. (악의적 값이어도 다른 재료와 해시로 혼합되어
//! 키를 약화시키지 못함, 단 제공되지 않으면 저엔트로피가 됨)
//!
//! # Errors
//! 형식 위반은 전부 `BootError`로 반환하며, 어떤 경우에도 blob 바깥을 읽지
//! 않습니다. 호출자는 실패 시 부팅을 중단해야 합니다(fail-secure).

use core::ops::Range;

pub(crate) mod walk;

use walk::{read_cells, walk, Fdt, Flow, Node, P};

/// 저장 가능한 물리 메모리 영역 수의 상한
pub const MAX_MEM_REGIONS: usize = 8;

/// 저장 가능한 예약 구간(memreserve + /reserved-memory) 수의 상한
pub const MAX_RSV_REGIONS: usize = 32;

/// 시드 값이 놓인 구간의 최대 수(`rng-seed`와 `kaslr-seed` 하나씩)
pub const MAX_SEED_SPANS: usize = 2;

/// /chosen에서 수집하는 엔트로피 바이트 상한 (rng-seed + kaslr-seed)
pub const MAX_ENTROPY: usize = 72;

const FDT_MAGIC: u32 = 0xd00d_feed;
const HEADER_SIZE: u32 = 40;
/// 수용 가능한 DTB 최대 크기입니다. 이 값이 곧 DTB에 할당하는 매핑 상한이라서
/// 과대한 totalsize로 페이지 테이블 풀을 고갈시키지 못합니다.
pub const MAX_DTB_SIZE: u32 = 2 * 1024 * 1024;

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
    /// 엔트로피에 들어간 `rng-seed` 바이트 수(`kaslr-seed`는 세지 않음)
    rng_seed_len: usize,
    /// DTB 안에서 시드 값이 놓인 구간들(blob 시작으로부터의 오프셋, 길이)
    seed_spans: [(usize, usize); MAX_SEED_SPANS],
    seed_span_count: usize,
    chosen_seen: bool,
}

impl BootInfo {
    /// 엔트로피에 들어간 `rng-seed` 바이트 수를 주는 함수입니다.
    ///
    /// 시드의 품질을 말할 때 쓰는 값입니다. `kaslr-seed`는 8바이트뿐이라
    /// 여기에 세지 않습니다.
    pub fn rng_seed_len(&self) -> usize {
        self.rng_seed_len
    }

    /// DTB 안에서 시드 값(`rng-seed`, `kaslr-seed`)이 놓인 구간을 주는
    /// 함수입니다. 각 항목은 (DTB 시작으로부터의 오프셋, 길이)이고 값 전체를
    /// 덮습니다(엔트로피 사본은 앞의 일부만 담을 수 있음).
    ///
    /// 커널이 시드를 쓴 뒤 DTB에 남은 원본을 지우는 데 씁니다.
    pub fn seed_spans(&self) -> &[(usize, usize)] {
        &self.seed_spans[..self.seed_span_count]
    }

    /// 엔트로피 사본을 지우는 함수입니다. 이후 [BootInfo::entropy]는 빈
    /// 구간을 줍니다.
    ///
    /// 시드를 다 쓴 뒤에 불러, 나중의 코드가 지워진 시드를 재료로 삼는 일이
    /// 없게 합니다.
    pub fn wipe_entropy(&mut self) {
        for b in self.entropy.iter_mut() {
            // SAFETY: b는 이 구조체 안의 유효한 바이트임. volatile이라 소거가
            //         최적화로 사라지지 않음
            unsafe { core::ptr::write_volatile(b, 0) };
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        self.entropy_len = 0;
        self.rng_seed_len = 0;
    }

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
    DuplicateProperty,
    UnsupportedRanges,
    MemoryLimited,
    BadInitrd,
    /// `/chosen` 노드가 둘 이상임. 시드가 어느 노드에 있는지 모호해져 쓴 뒤에
    /// 전부 지웠다고 말할 수 없음
    DuplicateChosen,
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
/// `addr`는 4바이트 정렬된 읽기 가능한 주소여야 합니다.
unsafe fn read_be32_phys(addr: usize) -> u32 {
    u32::from_be(unsafe { core::ptr::read_volatile(addr as *const u32) })
}

/// 메모리 노드로 인정할지 판정하는 함수입니다.
///
/// 이름(memory 또는 memory@...)과 device_type("memory")이 모두 맞고 사용
/// 가능한 노드만 RAM으로 칩니다. 이름만 같은 노드나 꺼진 노드를 RAM으로
/// 세면 untyped가 늘어나는 방향의 오류가 됩니다.
fn is_memory(node: &Node<'_>) -> bool {
    (node.name == b"memory" || node.name.starts_with(b"memory@"))
        && node.get(P::DeviceType) == Some(b"memory\0")
        && node.available()
}

/// 메모리에 있는 DTB 바이트열을 파싱하는 함수입니다.
///
/// [parse]가 주소로 읽어 온 뒤 하는 일과 같습니다. 커널 밖의 코드(UEFI 로더,
/// 호스트 도구)가 커널과 같은 규칙으로 DTB를 판정할 때 씁니다.
///
/// # Arguments
/// `blob` - DTB 전체(헤더의 totalsize 길이)
/// `dtb` - 결과에 기록할 DTB의 물리 범위
///
/// # Errors
/// 헤더와 구조 블록의 형식 위반, 한계 초과는 `BootError`
pub fn parse_blob(blob: &[u8], dtb: Range<usize>) -> Result<BootInfo, BootError> {
    let fdt = Fdt::new(blob)?;
    let mut info = BootInfo {
        dtb,
        regions: [MemRegion { base: 0, size: 0 }; MAX_MEM_REGIONS],
        region_count: 0,
        reserved: [MemRegion { base: 0, size: 0 }; MAX_RSV_REGIONS],
        reserved_count: 0,
        entropy: [0; MAX_ENTROPY],
        entropy_len: 0,
        rng_seed_len: 0,
        seed_spans: [(0, 0); MAX_SEED_SPANS],
        seed_span_count: 0,
        chosen_seen: false,
    };

    fdt.memreserve(|base, size| push_reserved(&mut info, base, size))?;

    walk(&fdt, |levels, node, depth| -> Result<Flow, BootError> {
        match depth {
            1 => {
                // 자식이 없는 루트여도 셀 프로퍼티의 형식은 확인함
                node.u32(P::AddressCells)?;
                node.u32(P::SizeCells)?;
            }
            2 => {
                // 루트가 셀 크기를 생략하면 DT spec 기본값(2 / 1)을 씀
                let root = &levels[1];
                if is_memory(node) {
                    if node.has(P::UsableMemory) {
                        return Err(BootError::MemoryLimited);
                    }
                    if let Some(reg) = node.get(P::Reg) {
                        let ac = root.addr_cells()?.unwrap_or(2);
                        let sc = root.size_cells()?.unwrap_or(1);
                        let mut count = info.region_count;
                        parse_reg(reg, ac, sc, &mut info.regions, &mut count)?;
                        info.region_count = count;
                    }
                } else if node.name == b"chosen" {
                    if node.has(P::UsableMemoryRange) {
                        return Err(BootError::MemoryLimited);
                    }
                    if info.chosen_seen {
                        return Err(BootError::DuplicateChosen);
                    }
                    info.chosen_seen = true;
                    for p in [P::RngSeed, P::KaslrSeed] {
                        if let Some(value) = node.get(p) {
                            let taken = push_entropy(value, &mut info);
                            if matches!(p, P::RngSeed) {
                                info.rng_seed_len = taken;
                            }
                            // 값은 blob 안의 조각이므로 주소 차이가 곧 오프셋임.
                            // 노드 하나에 같은 프로퍼티가 둘일 수 없고 /chosen도
                            // 하나뿐이라 칸이 모자랄 수 없음
                            let offset = value.as_ptr() as usize - blob.as_ptr() as usize;
                            if !value.is_empty() && offset + value.len() <= blob.len() {
                                info.seed_spans[info.seed_span_count] = (offset, value.len());
                                info.seed_span_count += 1;
                            }
                        }
                    }
                    push_initrd(node, &mut info)?;
                }
            }
            3 if levels[2].name == b"reserved-memory" => {
                let (root, rsv) = (&levels[1], &levels[2]);
                // 주소를 옮기는 ranges가 있으면 reg가 루트 주소 공간의 값이 아니게
                // 되어 엉뚱한 구간을 예약하게 되므로 해석하지 않고 거부함
                if rsv.ranges.is_some_and(|r| !r.is_empty()) {
                    return Err(BootError::UnsupportedRanges);
                }
                // reg 없이 size만 가진 동적 예약 노드는 펌웨어가 고정 주소를
                // 예약한 것이 아니라서 무시함
                if let Some(reg) = node.get(P::Reg) {
                    // spec상 루트와 같아야 하지만 명시를 우선함
                    let ac = rsv.addr_cells()?.or(root.addr_cells()?).unwrap_or(2);
                    let sc = rsv.size_cells()?.or(root.size_cells()?).unwrap_or(1);
                    let mut count = info.reserved_count;
                    parse_reg(reg, ac, sc, &mut info.reserved, &mut count)?;
                    info.reserved_count = count;
                }
            }
            _ => {}
        }
        Ok(Flow::Continue)
    })?;

    if info.region_count == 0 {
        return Err(BootError::NoMemory);
    }
    Ok(info)
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

/// 예약 목록에 구간 하나를 넣는 함수입니다.
fn push_reserved(info: &mut BootInfo, base: u64, size: u64) -> Result<(), BootError> {
    base.checked_add(size).ok_or(BootError::BadReg)?;
    if info.reserved_count == MAX_RSV_REGIONS {
        return Err(BootError::TooManyRegions);
    }
    info.reserved[info.reserved_count] = MemRegion { base, size };
    info.reserved_count += 1;
    Ok(())
}

/// /chosen의 initrd 구간을 예약 목록에 넣는 함수입니다.
///
/// 커널은 initrd를 쓰지 않지만, 부트로더가 실어 둔 구간이 untyped로 넘어가지
/// 않게 예약합니다. 두 프로퍼티 중 하나만 있거나 값이 뒤집힌 경우는 부트
/// 정보를 신뢰할 수 없어 거부합니다.
fn push_initrd(node: &Node<'_>, info: &mut BootInfo) -> Result<(), BootError> {
    let cell = |v: &[u8]| match v.len() {
        4 | 8 => Ok(read_cells(v)),
        _ => Err(BootError::BadInitrd),
    };
    match (node.get(P::InitrdStart), node.get(P::InitrdEnd)) {
        (None, None) => Ok(()),
        (Some(s), Some(e)) => {
            let (start, end) = (cell(s)?, cell(e)?);
            if start >= end {
                return Err(BootError::BadInitrd);
            }
            push_reserved(info, start, end - start)
        }
        _ => Err(BootError::BadInitrd),
    }
}

/// /chosen의 엔트로피 프로퍼티를 상한까지 수집하는 함수입니다.
///
/// 내용은 검증하지 않고(해시 혼합 재료일 뿐) 넘치는 바이트는 버립니다.
/// 수집한 바이트 수를 반환합니다.
fn push_entropy(value: &[u8], info: &mut BootInfo) -> usize {
    let cap = MAX_ENTROPY - info.entropy_len;
    let n = value.len().min(cap);
    info.entropy[info.entropy_len..info.entropy_len + n].copy_from_slice(&value[..n]);
    info.entropy_len += n;
    n
}

/// 다른 모듈의 테스트 도구가 blob 하나의 메모리 맵을 보기 위한 통로입니다.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::{parse_blob, BootError, MemRegion};

    pub(crate) fn parse_for_probe(blob: &[u8]) -> Result<(Vec<MemRegion>, Vec<MemRegion>), BootError> {
        let info = parse_blob(blob, 0x1000..0x1000 + blob.len())?;
        Ok((info.memory().to_vec(), info.reserved().to_vec()))
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod builder;
#[cfg(test)]
pub(crate) mod fuzz;
#[cfg(test)]
mod tests;
