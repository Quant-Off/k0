//! 부트 프레임 윈도우의 자리를 찾는 모듈입니다.
//!
//! # Features
//! 커널이 부팅 중에 쓰는 고정 크기의 연속 물리 구간(부트 윈도우)을 메모리 맵
//! 안에서 고릅니다. 커널과 UEFI 로더가 같은 함수를 쓰므로, 로더는 커널로
//! 넘어가기 전에 커널이 윈도우를 찾을 수 있는지 그대로 확인할 수 있습니다.

use core::ops::Range;

use crate::MemRegion;

/// 부트 윈도우의 크기입니다. 루트 태스크 적재가 끝나면 더 자라지 않는 고정
/// 예산입니다.
pub const BOOT_WINDOW_SIZE: u64 = 2 * 1024 * 1024;

/// 부트 윈도우가 들어갈 그래뉼 정렬 구간을 메모리 맵 안에서 찾는 함수입니다.
///
/// 커널 이미지 끝 바로 뒤를 먼저 보고, 거기서 위로 자리가 없으면(이미지가
/// RAM의 끝 가까이 적재된 경우) 가장 낮은 메모리부터 다시 찾습니다. 윈도우는
/// 한 메모리 구간 안에 통째로 들어가야 하고, 커널 이미지, DTB, 예약 구간,
/// 물리 주소 0의 그래뉼과 겹치지 않아야 합니다.
///
/// # Arguments
/// `memory` - 선언된 메모리 구간
/// `reserved` - 펌웨어 예약 구간
/// `image` - 커널 이미지의 물리 범위
/// `dtb` - DTB의 물리 범위
/// `granule` - 변환 그래뉼 크기(2의 거듭제곱)
pub fn boot_window(
    memory: &[MemRegion],
    reserved: &[MemRegion],
    image: &Range<u64>,
    dtb: &Range<u64>,
    granule: u64,
) -> Option<Range<u64>> {
    let lowest = memory.iter().map(|m| m.base).min()?;
    scan(memory, reserved, image, dtb, granule, image.end)
        .or_else(|| scan(memory, reserved, image, dtb, granule, lowest))
}

fn scan(
    memory: &[MemRegion],
    reserved: &[MemRegion],
    image: &Range<u64>,
    dtb: &Range<u64>,
    g: u64,
    from: u64,
) -> Option<Range<u64>> {
    let align_up = |v: u64| v.checked_add(g - 1).map(|x| x & !(g - 1));
    let dtb = MemRegion {
        base: dtb.start,
        size: dtb.end.checked_sub(dtb.start)?,
    };
    let image = MemRegion {
        base: image.start,
        size: image.end.checked_sub(image.start)?,
    };
    // 물리 주소 0의 그래뉼은 오브젝트로 내주지 않으므로 윈도우에도 넣지 않음
    let zero = MemRegion { base: 0, size: g };

    let mut start = align_up(from)?;
    // 장애물(DTB, 펌웨어 예약)을 넘어가며 안정될 때까지 전진, 반복은 유한
    for _ in 0..64 {
        let end = start.checked_add(BOOT_WINDOW_SIZE)?;
        let mut bumped = false;
        for r in [&dtb, &image, &zero].into_iter().chain(reserved) {
            // 파서가 base + size 오버플로를 이미 거부함
            let r_end = r.base.checked_add(r.size)?;
            if r_end > start && r.base < end {
                start = start.max(align_up(r_end)?);
                bumped = true;
            }
        }
        if bumped {
            continue;
        }
        if memory.iter().any(|m| start >= m.base && end <= m.base + m.size) {
            return Some(start..end);
        }
        // 어느 메모리 구간에도 안 들어가면 다음 구간의 시작으로 전진
        let next = memory.iter().map(|m| m.base).filter(|&b| b > start).min()?;
        start = align_up(next)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn mem(base: u64, size: u64) -> MemRegion {
        MemRegion { base, size }
    }

    #[test]
    fn the_window_follows_the_image_when_there_is_room() {
        let memory = [mem(0x4000_0000, 512 * MIB)];
        let image = 0x4008_0000..0x4012_6000;
        let dtb = 0x4800_0000..0x4810_0000;
        let w = boot_window(&memory, &[], &image, &dtb, 4096).unwrap();
        assert_eq!(w, 0x4012_6000..0x4032_6000);
        // 16K 그래뉼에서는 16K 경계로 올림
        let w = boot_window(&memory, &[], &image, &dtb, 16384).unwrap();
        assert_eq!(w, 0x4012_8000..0x4032_8000);
    }

    #[test]
    fn the_window_steps_over_the_dtb_and_reservations() {
        let memory = [mem(0x4000_0000, 512 * MIB)];
        let image = 0x4008_0000..0x4010_0000;
        // DTB가 이미지 바로 뒤에 있고 그 뒤에 예약 구간이 이어짐
        let dtb = 0x4010_0000..0x4011_0000;
        let reserved = [mem(0x4020_0000, MIB)];
        let w = boot_window(&memory, &reserved, &image, &dtb, 4096).unwrap();
        assert_eq!(w, 0x4030_0000..0x4050_0000);
    }

    #[test]
    fn the_window_falls_back_to_the_lowest_memory() {
        // 이미지가 RAM의 끝에 붙어 있어 뒤에는 자리가 없음
        let memory = [mem(0x4000_0000, 64 * MIB)];
        let image = 0x43f0_0000..0x4400_0000;
        let dtb = 0x4200_0000..0x4201_0000;
        let w = boot_window(&memory, &[], &image, &dtb, 4096).unwrap();
        assert_eq!(w, 0x4000_0000..0x4020_0000);
    }

    #[test]
    fn the_window_never_covers_physical_address_zero() {
        let memory = [mem(0, 64 * MIB)];
        let image = 0x3f0_0000..0x400_0000;
        let dtb = 0x200_0000..0x201_0000;
        let w = boot_window(&memory, &[], &image, &dtb, 16384).unwrap();
        assert_eq!(w, 0x4000..0x20_4000);
    }

    #[test]
    fn the_window_must_fit_inside_one_region() {
        // 이미지가 든 구간은 3MiB, 다른 구간은 1MiB라 어디에도 2MiB가 안 남음
        let memory = [mem(0x4010_0000, 3 * MIB), mem(0x4800_0000, MIB)];
        let image = 0x4010_0000..0x4030_0000;
        let dtb = 0x4800_0000..0x4801_0000;
        assert_eq!(boot_window(&memory, &[], &image, &dtb, 4096), None);
        // 두 번째 구간이 넉넉하면 거기서 찾음
        let memory = [mem(0x4010_0000, 3 * MIB), mem(0x4800_0000, 8 * MIB)];
        let w = boot_window(&memory, &[], &image, &dtb, 4096).unwrap();
        assert_eq!(w, 0x4801_0000..0x4821_0000);
    }

    #[test]
    fn no_memory_and_overflow_give_no_window() {
        let image = 0x1000..0x2000;
        let dtb = 0x3000..0x4000;
        assert_eq!(boot_window(&[], &[], &image, &dtb, 4096), None);
        // 주소 공간의 끝에서는 윈도우의 끝 계산이 넘치므로 자리가 없음
        let memory = [mem(0xFFFF_FFFF_FFF0_0000, 0xF_0000)];
        let image = 0xFFFF_FFFF_FFF0_0000..0xFFFF_FFFF_FFF8_0000;
        assert_eq!(boot_window(&memory, &[], &image, &dtb, 4096), None);
    }
}
