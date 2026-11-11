//! UEFI 메모리 맵에서 커널에 넘길 메모리 구간을 계획하는 모듈입니다.
//!
//! # Features
//! 펌웨어가 준 메모리 맵 바이트열을 읽어, 커널이 RAM으로 써도 되는 구간만
//! 골라 최대 8개의 `reg` 항목으로 만듭니다. 원칙은 하나입니다. 메모리를 잃는
//! 것은 괜찮고, 펌웨어나 디바이스가 쓰는 메모리를 RAM이라고 말하는 것은 안
//! 됩니다. 그래서 모르는 타입은 전부 거부 쪽으로 보고, 디스크립터가 서로
//! 겹치면 거부가 이깁니다.
//!
//! 이 모듈은 할당하지 않습니다. 작업 공간은 호출자가 넘깁니다.

use k0_abi::PA_CEILING;
use k0_boot::{MemRegion, MAX_MEM_REGIONS};

/// UEFI 메모리 맵의 페이지 크기입니다. 그래뉼과 무관하게 항상 4KiB입니다.
pub const EFI_PAGE: u64 = 4096;

/// 계획의 모든 경계를 맞추는 단위입니다. 4KiB와 16KiB 그래뉼 모두의 배수라서
/// 어느 커널 이미지에도 같은 계획을 쓸 수 있습니다.
pub const ALIGN: u64 = 16 * 1024;

/// EFI_MEMORY_DESCRIPTOR에서 이 모듈이 읽는 필드까지의 크기입니다. 펌웨어가
/// 알려 주는 간격은 이보다 클 수 있습니다.
const DESC_MIN: usize = 40;

const LOADER_CODE: u32 = 1;
const LOADER_DATA: u32 = 2;
const BOOT_SERVICES_CODE: u32 = 3;
const BOOT_SERVICES_DATA: u32 = 4;
const CONVENTIONAL: u32 = 7;
const MMIO: u32 = 11;
const MMIO_PORT_SPACE: u32 = 12;

const ATTR_WB: u64 = 0x8;
const ATTR_NV: u64 = 0x8000;
const ATTR_SP: u64 = 0x4_0000;
const ATTR_RUNTIME: u64 = 1 << 63;

/// 계획을 세우지 못한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// 디스크립터 간격이 구조체보다 작음
    BadStride,
    /// 맵의 길이가 간격의 배수가 아님
    BadMapSize,
    /// 디스크립터의 시작 주소가 4KiB 경계가 아님
    Misaligned,
    /// 디스크립터의 주소 계산이 64비트를 넘침
    Overflow,
    /// 호출자가 준 작업 공간이 모자람
    ScratchTooSmall,
    /// 고정 블록의 정렬이나 크기가 맞지 않음
    BadBlock,
    /// 고정 블록이 사용 가능한 한 구간 안에 통째로 들어 있지 않음
    BlockNotUsable,
    /// 셀 폭에서 나온 한계값이 쓸 수 없는 값임
    BadLimits,
}

/// 메모리 디스크립터 하나에서 읽은 값입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Desc {
    pub kind: u32,
    pub start: u64,
    pub end: u64,
    pub attr: u64,
}

impl Desc {
    /// ExitBootServices 뒤에 커널이 RAM으로 써도 되는 디스크립터인지 판정하는
    /// 함수입니다.
    ///
    /// 빈 메모리와 이 로더 자신의 메모리만 허용합니다. 부트 서비스 메모리는
    /// 규격상 풀려나지만 쓰지 않습니다. 펌웨어가 표를 남겨 두는 곳이고
    /// 디바이스가 계속 써 넣을 수 있는 곳이기 때문입니다.
    pub fn usable(&self) -> bool {
        matches!(self.kind, LOADER_CODE | LOADER_DATA | CONVENTIONAL)
            && self.attr & ATTR_WB != 0
            && self.attr & (ATTR_RUNTIME | ATTR_SP | ATTR_NV) == 0
    }

    /// 디바이스 레지스터 구간인지 판정하는 함수입니다.
    pub fn is_mmio(&self) -> bool {
        matches!(self.kind, MMIO | MMIO_PORT_SPACE)
    }

    /// 부트 서비스가 쓰던 메모리인지 판정하는 함수입니다.
    pub fn is_boot_services(&self) -> bool {
        matches!(self.kind, BOOT_SERVICES_CODE | BOOT_SERVICES_DATA)
    }
}

fn le32(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn le64(b: &[u8], off: usize) -> Option<u64> {
    let s = b.get(off..off.checked_add(8)?)?;
    Some(u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

/// 검증을 마친 UEFI 메모리 맵의 뷰입니다.
///
/// 만들 때 모든 디스크립터의 정렬과 주소 계산을 확인하므로, 순회는 실패하지
/// 않습니다.
#[derive(Clone, Copy)]
pub struct Map<'a> {
    bytes: &'a [u8],
    stride: usize,
}

impl<'a> Map<'a> {
    /// 맵 바이트열을 검증해 뷰를 만드는 함수입니다.
    ///
    /// # Arguments
    /// `bytes` - GetMemoryMap이 채운 만큼의 바이트열
    /// `stride` - GetMemoryMap이 알려 준 디스크립터 간격
    ///
    /// # Errors
    /// 간격이나 길이가 맞지 않거나 디스크립터의 주소가 형식에 어긋나면 `PlanError`
    pub fn new(bytes: &'a [u8], stride: usize) -> Result<Self, PlanError> {
        if stride < DESC_MIN {
            return Err(PlanError::BadStride);
        }
        if bytes.len() % stride != 0 {
            return Err(PlanError::BadMapSize);
        }
        let map = Self { bytes, stride };
        for chunk in bytes.chunks_exact(stride) {
            Self::read(chunk)?;
        }
        Ok(map)
    }

    fn read(chunk: &[u8]) -> Result<Option<Desc>, PlanError> {
        let field = |v: Option<u64>| v.ok_or(PlanError::BadStride);
        let kind = le32(chunk, 0).ok_or(PlanError::BadStride)?;
        let start = field(le64(chunk, 8))?;
        let pages = field(le64(chunk, 24))?;
        let attr = field(le64(chunk, 32))?;
        if pages == 0 {
            return Ok(None);
        }
        if start % EFI_PAGE != 0 {
            return Err(PlanError::Misaligned);
        }
        let size = pages.checked_mul(EFI_PAGE).ok_or(PlanError::Overflow)?;
        let end = start.checked_add(size).ok_or(PlanError::Overflow)?;
        Ok(Some(Desc { kind, start, end, attr }))
    }

    /// 맵에 든 디스크립터 수를 주는 함수입니다. 크기가 0인 것도 셉니다.
    pub fn count(&self) -> usize {
        self.bytes.len() / self.stride
    }

    /// 크기가 0이 아닌 디스크립터를 차례로 주는 함수입니다.
    pub fn iter(&self) -> impl Iterator<Item = Desc> + 'a {
        self.bytes
            .chunks_exact(self.stride)
            .filter_map(|c| Self::read(c).ok().flatten())
    }

    /// 조건에 맞는 디스크립터의 바이트 수 합을 주는 함수입니다. 겹치는
    /// 디스크립터는 겹친 만큼 더 셉니다(로그에 쓰는 어림값).
    pub fn bytes_where(&self, pred: impl Fn(&Desc) -> bool) -> u64 {
        self.iter()
            .filter(|d| pred(d))
            .fold(0u64, |acc, d| acc.saturating_add(d.end - d.start))
    }
}

/// 루트 노드의 셀 폭이 정하는 한계입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// RAM의 끝이 넘으면 안 되는 주소
    pub max_addr: u64,
    /// `reg` 항목 하나가 담을 수 있는 최대 크기(정렬 단위의 배수)
    pub max_entry: u64,
}

impl Limits {
    /// 루트 노드의 `#address-cells`와 `#size-cells`에서 한계를 구하는 함수입니다.
    ///
    /// # Arguments
    /// `addr_cells` - 주소 셀 수(1 또는 2)
    /// `size_cells` - 크기 셀 수(1 또는 2)
    pub fn from_cells(addr_cells: u32, size_cells: u32) -> Self {
        Self {
            max_addr: if addr_cells == 1 { 1 << 32 } else { PA_CEILING },
            max_entry: if size_cells == 1 { (1 << 32) - ALIGN } else { PA_CEILING },
        }
    }
}

/// 계획에 쓰는 작업 공간입니다.
pub struct Scratch<'a> {
    pub points: &'a mut [u64],
    pub ranges: &'a mut [MemRegion],
}

impl Scratch<'_> {
    /// 맵과 구멍 수에 필요한 작업 공간의 크기를 주는 함수입니다.
    ///
    /// # Arguments
    /// `descriptors` - 맵의 디스크립터 수
    /// `holes` - 뺄 구간의 수
    pub fn need(descriptors: usize, holes: usize) -> (usize, usize) {
        (
            descriptors.saturating_mul(2),
            descriptors.saturating_add(holes).saturating_add(2),
        )
    }
}

/// 커널에 넘길 메모리 구간의 계획입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    regions: [MemRegion; MAX_MEM_REGIONS],
    len: usize,
    /// 사용 가능하다고 판정한 메모리의 합
    pub usable_bytes: u64,
    /// 그중 계획에 넣은 합
    pub kept_bytes: u64,
    /// 항목 수 한계 때문에 버린 구간 조각의 수
    pub dropped: usize,
}

impl Plan {
    /// 주소 순으로 정렬된 구간 목록을 주는 함수입니다.
    pub fn regions(&self) -> &[MemRegion] {
        &self.regions[..self.len]
    }
}

fn align_down(v: u64) -> u64 {
    v & !(ALIGN - 1)
}

fn align_up(v: u64) -> u64 {
    // 넘치면 주소 공간의 끝으로 봄(뒤에서 한계로 잘림)
    v.checked_add(ALIGN - 1).map_or(u64::MAX & !(ALIGN - 1), align_down)
}

fn end(r: &MemRegion) -> u64 {
    r.base + r.size
}

/// 목록 `ranges[..len]`에서 구간 하나를 빼는 함수입니다. 구간이 한 조각의
/// 가운데에 있으면 조각이 둘로 나뉩니다.
fn subtract(ranges: &mut [MemRegion], len: &mut usize, hs: u64, he: u64) -> Result<(), PlanError> {
    if he <= hs {
        return Ok(());
    }
    let mut i = 0;
    while i < *len {
        let (s, e) = (ranges[i].base, end(&ranges[i]));
        if he <= s || e <= hs {
            i += 1;
            continue;
        }
        let left = hs.saturating_sub(s).min(e - s);
        let right = e.saturating_sub(he);
        match (left > 0 && hs > s, right > 0 && he < e) {
            (true, true) => {
                if *len == ranges.len() {
                    return Err(PlanError::ScratchTooSmall);
                }
                ranges.copy_within(i + 1..*len, i + 2);
                ranges[i] = MemRegion { base: s, size: hs - s };
                ranges[i + 1] = MemRegion { base: he, size: e - he };
                *len += 1;
                i += 2;
            }
            (true, false) => {
                ranges[i] = MemRegion { base: s, size: hs - s };
                i += 1;
            }
            (false, true) => {
                ranges[i] = MemRegion { base: he, size: e - he };
                i += 1;
            }
            (false, false) => {
                ranges.copy_within(i + 1..*len, i);
                *len -= 1;
            }
        }
    }
    Ok(())
}

/// 메모리 맵에서 커널에 넘길 구간을 계획하는 함수입니다.
///
/// 순서는 다음과 같습니다. 디스크립터 경계로 나눈 구간마다 사용 가능한
/// 디스크립터가 덮고 거부 디스크립터는 덮지 않는 것만 남기고, 맞닿은 것을
/// 합치고, 한계 주소에서 자르고, 정렬 단위 안쪽으로 줄이고, 구멍(펌웨어 DTB의
/// 예약과 디바이스 구간)을 뺍니다. 고정 블록은 더해 넣지 않습니다. 결과의 한
/// 구간 안에 통째로 들어 있는지만 확인합니다. 항목이 8개를 넘으면 고정 블록이
/// 든 조각과 큰 조각부터 남깁니다.
///
/// # Arguments
/// `map` - 검증된 메모리 맵
/// `holes` - RAM이라고 말하면 안 되는 구간(바깥쪽으로 정렬해 뺌)
/// `block` - 로더가 할당한 고정 블록(정렬 단위에 맞아야 함)
/// `limits` - 루트 셀 폭이 정하는 한계
/// `scratch` - 작업 공간([Scratch::need] 이상)
///
/// # Errors
/// 작업 공간 부족, 고정 블록이 사용 가능한 한 구간 밖에 있음 등은 `PlanError`
pub fn plan(
    map: &Map<'_>,
    holes: &[MemRegion],
    block: &MemRegion,
    limits: &Limits,
    scratch: Scratch<'_>,
) -> Result<Plan, PlanError> {
    let Scratch { points, ranges } = scratch;
    // 블록의 처음은 DTB라서 물리 주소 0의 그래뉼에 놓일 수 없음(커널이 0번지의
    // DTB를 거부하고 그 그래뉼을 오브젝트로 내주지 않음)
    if block.size == 0 || block.base < ALIGN || block.base % ALIGN != 0 || block.size % ALIGN != 0 {
        return Err(PlanError::BadBlock);
    }
    let block_end = block.base.checked_add(block.size).ok_or(PlanError::BadBlock)?;
    if limits.max_entry == 0 || limits.max_entry % ALIGN != 0 || limits.max_entry < block.size {
        return Err(PlanError::BadLimits);
    }

    // 1. 디스크립터의 모든 경계를 모아 정렬하고 중복을 없앰
    let mut np = 0;
    for d in map.iter() {
        if np + 2 > points.len() {
            return Err(PlanError::ScratchTooSmall);
        }
        points[np] = d.start;
        points[np + 1] = d.end;
        np += 2;
    }
    points[..np].sort_unstable();
    let mut uniq = 0;
    for i in 0..np {
        if uniq == 0 || points[uniq - 1] != points[i] {
            points[uniq] = points[i];
            uniq += 1;
        }
    }

    // 2. 이웃한 경계 사이의 구간은 어느 디스크립터에든 통째로 들거나 전혀 들지
    //    않음. 사용 가능이 덮고 거부가 덮지 않는 구간만 남기며 맞닿으면 합침
    let mut len = 0;
    for w in points[..uniq].windows(2) {
        let (a, b) = (w[0], w[1]);
        let mut usable = false;
        let mut denied = false;
        for d in map.iter().filter(|d| d.start <= a && b <= d.end) {
            if d.usable() {
                usable = true;
            } else {
                denied = true;
            }
        }
        if !usable || denied {
            continue;
        }
        if len > 0 && end(&ranges[len - 1]) == a {
            ranges[len - 1].size += b - a;
        } else {
            if len == ranges.len() {
                return Err(PlanError::ScratchTooSmall);
            }
            ranges[len] = MemRegion { base: a, size: b - a };
            len += 1;
        }
    }

    // 3. 한계 주소에서 자르고 정렬 단위 안쪽으로 줄임
    let mut kept = 0;
    for i in 0..len {
        let s = align_up(ranges[i].base);
        let e = align_down(end(&ranges[i]).min(limits.max_addr));
        if e > s {
            ranges[kept] = MemRegion { base: s, size: e - s };
            kept += 1;
        }
    }
    len = kept;

    // 4. 구멍을 바깥쪽으로 정렬해 뺌
    for h in holes {
        let he = h.base.checked_add(h.size).map_or(u64::MAX, align_up);
        subtract(ranges, &mut len, align_down(h.base), he)?;
    }

    // 5. 고정 블록은 결과의 한 구간 안에 통째로 있어야 함
    let bi = ranges[..len]
        .iter()
        .position(|r| r.base <= block.base && block_end <= end(r))
        .ok_or(PlanError::BlockNotUsable)?;
    let usable_bytes = ranges[..len].iter().fold(0u64, |a, r| a.saturating_add(r.size));

    // 6. 항목 고르기. 고정 블록이 든 조각을 먼저, 그다음은 큰 조각부터.
    //    조각 하나는 `reg` 항목 하나가 담을 수 있는 크기를 넘지 않음
    let mut regions = [MemRegion { base: 0, size: 0 }; MAX_MEM_REGIONS];
    let mut n = 0;
    {
        let r = ranges[bi];
        let piece = if r.size <= limits.max_entry {
            r
        } else {
            let latest = end(&r) - limits.max_entry;
            MemRegion { base: r.base.max(block.base.min(latest)), size: limits.max_entry }
        };
        regions[n] = piece;
        n += 1;
        // 조각을 떼어낸 나머지를 목록에 되돌림
        ranges.copy_within(bi + 1..len, bi);
        len -= 1;
        for rest in [
            MemRegion { base: r.base, size: piece.base - r.base },
            MemRegion { base: end(&piece), size: end(&r) - end(&piece) },
        ] {
            if rest.size > 0 {
                if len == ranges.len() {
                    return Err(PlanError::ScratchTooSmall);
                }
                ranges[len] = rest;
                len += 1;
            }
        }
    }
    while n < MAX_MEM_REGIONS {
        let mut best: Option<usize> = None;
        for i in 0..len {
            let size = ranges[i].size.min(limits.max_entry);
            if size == 0 {
                continue;
            }
            let better = match best {
                None => true,
                Some(j) => {
                    let cur = ranges[j].size.min(limits.max_entry);
                    size > cur || (size == cur && ranges[i].base < ranges[j].base)
                }
            };
            if better {
                best = Some(i);
            }
        }
        let Some(i) = best else { break };
        let size = ranges[i].size.min(limits.max_entry);
        regions[n] = MemRegion { base: ranges[i].base, size };
        n += 1;
        ranges[i].base += size;
        ranges[i].size -= size;
    }
    let dropped = ranges[..len].iter().filter(|r| r.size > 0).count();
    regions[..n].sort_unstable_by_key(|r| r.base);
    let kept_bytes = regions[..n].iter().fold(0u64, |a, r| a.saturating_add(r.size));

    Ok(Plan { regions, len: n, usable_bytes, kept_bytes, dropped })
}
