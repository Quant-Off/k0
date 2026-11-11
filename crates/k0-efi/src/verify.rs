//! 완성된 메모리 기술을 계획과 다른 방법으로 다시 확인하는 모듈입니다.
//!
//! # Features
//! 계획([crate::memmap::plan])은 경계를 정렬해 훑는 방식으로 구간을 만듭니다.
//! 이 모듈은 그 코드를 쓰지 않고, 결과의 구간마다 원본 메모리 맵을 직접
//! 따라가며 같은 성질을 확인합니다. 계획에 버그가 있어 펌웨어 메모리가
//! 섞였더라도 커널로 넘어가기 전에 여기서 걸립니다.

use core::ops::Range;

use k0_boot::{MemRegion, MAX_MEM_REGIONS};

use crate::memmap::{Limits, Map, ALIGN};

/// 검증이 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// 구간이 없거나 8개를 넘음
    BadCount,
    /// 크기가 0이거나 주소 계산이 넘치거나 정렬이 맞지 않음
    BadRegion,
    /// 주소 순이 아니거나 서로 겹침
    Unsorted,
    /// 한계 주소나 항목 크기 한계를 넘음
    AboveLimit,
    /// 사용 가능한 디스크립터가 덮지 않는 바이트가 있음
    NotUsable,
    /// 거부 디스크립터와 겹침
    Denied,
    /// 펌웨어 예약이나 디바이스 구간과 겹침
    Hole,
    /// 이미지, DTB, 부트 윈도우가 같은 한 구간 안에 있지 않음
    BlockSplit,
    /// 이미지나 DTB가 물리 주소 0의 그래뉼에 걸침
    ZeroPage,
}

/// 로더가 할당한 고정 블록의 세 부분입니다.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub dtb: Range<u64>,
    pub image: Range<u64>,
    pub window: Range<u64>,
}

fn overlaps(r: &MemRegion, start: u64, end: u64) -> bool {
    start < end && start < r.base + r.size && r.base < end
}

/// 메모리 구간 목록이 원본 맵에 비추어 안전한지 확인하는 함수입니다.
///
/// # Arguments
/// `regions` - 커널의 파서가 출력 DTB에서 읽어 낸 메모리 구간
/// `map` - 계획에 쓴 것과 같은 메모리 맵
/// `holes` - RAM이라고 말하면 안 되는 구간(정렬하지 않은 원래 값)
/// `block` - 고정 블록의 세 부분
/// `limits` - 루트 셀 폭이 정하는 한계
///
/// # Errors
/// 어느 성질이든 어긋나면 `VerifyError`. 호출자는 부팅을 거부해야 합니다.
pub fn verify(
    regions: &[MemRegion],
    map: &Map<'_>,
    holes: &[MemRegion],
    block: &Block,
    limits: &Limits,
) -> Result<(), VerifyError> {
    if regions.is_empty() || regions.len() > MAX_MEM_REGIONS {
        return Err(VerifyError::BadCount);
    }

    let mut prev_end = 0u64;
    for (i, r) in regions.iter().enumerate() {
        if r.size == 0 || r.base % ALIGN != 0 || r.size % ALIGN != 0 {
            return Err(VerifyError::BadRegion);
        }
        let end = r.base.checked_add(r.size).ok_or(VerifyError::BadRegion)?;
        if end > limits.max_addr || r.size > limits.max_entry {
            return Err(VerifyError::AboveLimit);
        }
        if i > 0 && r.base < prev_end {
            return Err(VerifyError::Unsorted);
        }
        prev_end = end;

        // 구간의 처음부터 끝까지, 지금 위치를 덮는 사용 가능한 디스크립터를
        // 찾아 그 끝으로 건너뜀. 덮는 것이 없으면 그 바이트는 RAM이 아님
        let mut cursor = r.base;
        let mut steps = 0usize;
        while cursor < end {
            let next = map
                .iter()
                .filter(|d| d.usable() && d.start <= cursor && cursor < d.end)
                .map(|d| d.end)
                .max()
                .ok_or(VerifyError::NotUsable)?;
            cursor = next.min(end);
            steps += 1;
            if steps > map.count() {
                return Err(VerifyError::NotUsable);
            }
        }

        if map.iter().any(|d| !d.usable() && overlaps(r, d.start, d.end)) {
            return Err(VerifyError::Denied);
        }
        for h in holes {
            let h_end = h.base.checked_add(h.size).unwrap_or(u64::MAX);
            if overlaps(r, h.base, h_end) {
                return Err(VerifyError::Hole);
            }
        }
    }

    let inside = |r: &MemRegion, p: &Range<u64>| p.start < p.end && r.base <= p.start && p.end <= r.base + r.size;
    let home = regions
        .iter()
        .find(|r| inside(r, &block.image))
        .ok_or(VerifyError::BlockSplit)?;
    if !inside(home, &block.dtb) || !inside(home, &block.window) {
        return Err(VerifyError::BlockSplit);
    }
    if block.image.start < ALIGN || block.dtb.start < ALIGN {
        return Err(VerifyError::ZeroPage);
    }
    Ok(())
}
