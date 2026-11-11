//! 커널에 넘길 DTB를 만들고 넘기기 전에 확인하는 모듈입니다.
//!
//! # Features
//! 펌웨어의 DTB와 UEFI 메모리 맵에서 커널용 DTB를 만든 뒤, 커널이 부팅 때
//! 하는 판정을 같은 코드로 미리 돌립니다. 파서, 플랫폼 발견, 메모리 맵 대조,
//! 부트 윈도우 탐색이 모두 커널의 것입니다. 여기에 계획과 다른 방법으로 쓴
//! 검증([crate::verify])을 더합니다. 하나라도 실패하면 로더는 펌웨어 콘솔이
//! 아직 살아 있을 때 사유를 출력하고 부팅을 거부합니다.
//!
//! 펌웨어 DTB의 예약 구간과 디바이스는 커널의 파서가 읽은 결과를 그대로
//! 씁니다. 그래서 커널이 거부할 펌웨어 DTB는 로더도 거부합니다.

use core::ops::Range;

use k0_abi::PA_CEILING;
use k0_boot::{
    boot_window, discover_blob, parse_blob, BootError, DiscoverError, Fdt, Layout, MemRegion, NoConsole, Platform,
    ValidateError, BOOT_WINDOW_SIZE, MAX_DTB_SIZE, MAX_RSV_REGIONS,
};

use crate::fdtw::{rewrite, root_cells, Edits, RewriteError};
use crate::memmap::{plan, Limits, Map, Plan, PlanError, Scratch, ALIGN};
use crate::verify::{verify, Block, VerifyError};

/// 디바이스 구간의 최대 수입니다(콘솔, 디스트리뷰터, CPU 인터페이스 또는
/// 리디스트리뷰터 구간들).
const MAX_DEVICE_REGIONS: usize = 2 + k0_boot::MAX_REDIST_REGIONS;

/// 준비가 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareError {
    /// 고정 블록의 배치가 맞지 않음(정렬, 크기, 주소 계산)
    BadLayout,
    /// 호출자가 준 작업 공간이 모자람
    ScratchTooSmall,
    /// 펌웨어의 DTB를 커널의 파서가 거부함
    FirmwareDtb(BootError),
    /// 펌웨어의 DTB에서 커널이 플랫폼을 찾지 못함
    FirmwareDiscover(DiscoverError),
    /// 메모리 계획 실패
    Plan(PlanError),
    /// DTB 재작성 실패
    Rewrite(RewriteError),
    /// 만든 DTB를 커널의 파서가 거부함
    Output(BootError),
    /// 만든 DTB의 메모리가 계획과 다르거나 예약 구간이 남아 있음
    MemoryMismatch,
    /// 만든 DTB에서 커널이 플랫폼을 찾지 못함
    OutputDiscover(DiscoverError),
    /// 재작성 전후로 발견한 플랫폼이 달라짐
    PlatformChanged,
    /// 커널의 메모리 맵 대조가 거부함
    Validate(ValidateError),
    /// 커널이 부트 윈도우를 찾지 못함
    NoWindow,
    /// 독립 검증이 거부함
    Verify(VerifyError),
}

/// 준비에 필요한 입력입니다.
pub struct Request<'a> {
    /// 펌웨어가 준 DTB 전체(헤더의 totalsize 길이)
    pub firmware_dtb: &'a [u8],
    /// ExitBootServices에 쓸 키와 같은 시점의 메모리 맵
    pub map: Map<'a>,
    /// 고정 블록의 시작 물리 주소. 블록은 DTB 버퍼, 이미지 영역, 여유 순서
    pub block_base: u64,
    /// 이미지 영역의 크기(헤더의 image_size를 정렬 단위로 올린 값)
    pub image_size: u64,
    /// 이미지 영역 뒤 여유 공간의 크기
    pub tail_size: u64,
    /// 내장한 커널의 변환 그래뉼
    pub granule: u64,
    /// 이 CPU의 물리 주소 폭이 정하는 디바이스 주소의 상한
    pub device_limit: u64,
    /// `rng-seed`에 더할 엔트로피, 없으면 빈 슬라이스
    pub seed: &'a [u8],
}

/// 준비에 쓰는 작업 공간입니다.
pub struct Work<'a> {
    pub points: &'a mut [u64],
    pub ranges: &'a mut [MemRegion],
    pub holes: &'a mut [MemRegion],
}

impl Work<'_> {
    /// 디스크립터 수에 필요한 작업 공간의 크기를 (points, ranges, holes) 순으로
    /// 주는 함수입니다.
    ///
    /// # Arguments
    /// `descriptors` - 메모리 맵의 디스크립터 수
    pub fn need(descriptors: usize) -> (usize, usize, usize) {
        let holes = MAX_RSV_REGIONS + MAX_DEVICE_REGIONS;
        let (points, ranges) = Scratch::need(descriptors, holes);
        (points, ranges, descriptors.saturating_add(holes))
    }
}

/// 준비가 끝난 결과입니다.
#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    /// 출력 버퍼에 쓴 DTB의 길이
    pub dtb_len: usize,
    /// 커널에 넘긴 메모리 계획
    pub plan: Plan,
    /// 커널이 발견하게 될 플랫폼
    pub platform: Platform,
    /// 콘솔이 없으면 그 이유
    pub no_console: Option<NoConsole>,
    /// 쓰지 않고 남겨 둔 부트 서비스 메모리의 어림 크기
    pub boot_services_bytes: u64,
}

/// DTB 헤더의 앞 8바이트에서 전체 길이를 읽는 함수입니다. 로더가 펌웨어의
/// 포인터에서 읽을 범위를 정할 때 씁니다.
///
/// # Arguments
/// `head` - DTB의 처음 8바이트 이상
pub fn firmware_dtb_size(head: &[u8]) -> Option<usize> {
    if k0_boot::be32(head, 0).ok()? != 0xd00d_feed {
        return None;
    }
    let total = k0_boot::be32(head, 4).ok()?;
    (40..=MAX_DTB_SIZE).contains(&total).then_some(total as usize)
}

fn device_regions(p: &Platform) -> ([MemRegion; MAX_DEVICE_REGIONS], usize) {
    let mut out = [MemRegion { base: 0, size: 0 }; MAX_DEVICE_REGIONS];
    let mut n = 0;
    let mut push = |base: u64, size: u64| {
        if n < out.len() {
            out[n] = MemRegion { base, size };
            n += 1;
        }
    };
    if let Some(c) = &p.console {
        push(c.reg.base, c.reg.size);
    }
    match &p.intc {
        k0_boot::Intc::GicV2 { dist, cpu } => {
            push(dist.base, dist.size);
            push(cpu.base, cpu.size);
        }
        k0_boot::Intc::GicV3 { dist, .. } => push(dist.base, dist.size),
    }
    for r in p.intc.redist() {
        push(r.base, r.size);
    }
    (out, n)
}

/// 커널에 넘길 DTB를 만들고 커널의 판정을 미리 돌리는 함수입니다.
///
/// 같은 입력에는 같은 출력을 내고 아무것도 할당하지 않으므로, ExitBootServices가
/// 맵이 바뀌었다고 거부하면 새 맵으로 다시 부를 수 있습니다.
///
/// # Arguments
/// `req` - 펌웨어 DTB, 메모리 맵, 고정 블록의 배치
/// `work` - 작업 공간([Work::need] 이상)
/// `out` - 고정 블록의 맨 앞에 있는 DTB 버퍼
///
/// # Errors
/// 어느 단계든 실패하면 `PrepareError`. 호출자는 부팅을 거부해야 합니다.
pub fn prepare(req: &Request<'_>, work: Work<'_>, out: &mut [u8]) -> Result<Prepared, PrepareError> {
    let Work { points, ranges, holes } = work;
    let dtb_cap = out.len() as u64;
    let aligned = |v: u64| v != 0 && v % ALIGN == 0;
    if req.block_base % ALIGN != 0 || !aligned(dtb_cap) || !aligned(req.image_size) {
        return Err(PrepareError::BadLayout);
    }
    if req.tail_size % ALIGN != 0 || req.tail_size < BOOT_WINDOW_SIZE + ALIGN {
        return Err(PrepareError::BadLayout);
    }
    let image_start = req.block_base.checked_add(dtb_cap).ok_or(PrepareError::BadLayout)?;
    let image_end = image_start.checked_add(req.image_size).ok_or(PrepareError::BadLayout)?;
    let block_end = image_end.checked_add(req.tail_size).ok_or(PrepareError::BadLayout)?;
    let block = MemRegion { base: req.block_base, size: block_end - req.block_base };
    let image = image_start..image_end;

    // 1. 펌웨어 DTB를 커널의 파서와 발견으로 읽음. 예약과 디바이스의 출처는 이것 하나
    let fw = req.firmware_dtb;
    let fw_info = parse_blob(fw, 0..fw.len()).map_err(PrepareError::FirmwareDtb)?;
    let fw_found = discover_blob(fw).map_err(PrepareError::FirmwareDiscover)?;
    let (devices, device_count) = device_regions(&fw_found.platform);
    let hole_count = fw_info.reserved().len() + device_count;
    if holes.len() < hole_count {
        return Err(PrepareError::ScratchTooSmall);
    }
    holes[..fw_info.reserved().len()].copy_from_slice(fw_info.reserved());
    holes[fw_info.reserved().len()..hole_count].copy_from_slice(&devices[..device_count]);

    // 2. 계획과 재작성
    let fdt = Fdt::new(fw).map_err(PrepareError::FirmwareDtb)?;
    let (addr_cells, size_cells) = root_cells(&fdt).map_err(PrepareError::Rewrite)?;
    let limits = Limits::from_cells(addr_cells, size_cells);
    let plan = plan(&req.map, &holes[..hole_count], &block, &limits, Scratch { points, ranges })
        .map_err(PrepareError::Plan)?;
    let edits = Edits { memory: plan.regions(), seed: req.seed };
    let dtb_len = rewrite(fw, &edits, out).map_err(PrepareError::Rewrite)?;
    let dtb = req.block_base..req.block_base + dtb_len as u64;

    // 3. 만든 DTB에 커널의 파서와 발견을 다시 돌림
    let made = &out[..dtb_len];
    let info = parse_blob(made, dtb.start as usize..dtb.end as usize).map_err(PrepareError::Output)?;
    if info.memory() != plan.regions() || !info.reserved().is_empty() {
        return Err(PrepareError::MemoryMismatch);
    }
    let found = discover_blob(made).map_err(PrepareError::OutputDiscover)?;
    if found != fw_found {
        return Err(PrepareError::PlatformChanged);
    }

    // 4. 커널이 찾을 부트 윈도우와, 계획과 다른 방법의 검증
    let window: Range<u64> =
        boot_window(info.memory(), info.reserved(), &image, &dtb, req.granule).ok_or(PrepareError::NoWindow)?;
    let parts = Block { dtb: dtb.clone(), image: image.clone(), window };
    verify(info.memory(), &req.map, &holes[..hole_count], &parts, &limits).map_err(PrepareError::Verify)?;

    // 5. 커널의 메모리 맵 대조. 출력에는 예약 구간이 없으므로, 펌웨어가 쓰는
    //    RAM(디바이스 레지스터가 아닌 거부 디스크립터)과 펌웨어 DTB의 예약을
    //    예약으로 넘겨 그 위에 놓인 디바이스를 커널보다 엄격하게 걸러냄
    let mut n = fw_info.reserved().len();
    for d in req.map.iter().filter(|d| !d.usable() && !d.is_mmio()) {
        if n == holes.len() {
            return Err(PrepareError::ScratchTooSmall);
        }
        holes[n] = MemRegion { base: d.start, size: d.end - d.start };
        n += 1;
    }
    let layout = Layout {
        memory: info.memory(),
        reserved: &holes[..n],
        image,
        dtb,
        granule: req.granule,
        ram_limit: PA_CEILING,
        device_limit: req.device_limit,
    };
    found.platform.validate(&layout).map_err(PrepareError::Validate)?;

    Ok(Prepared {
        dtb_len,
        plan,
        platform: found.platform,
        no_console: found.no_console,
        boot_services_bytes: req.map.bytes_where(|d| d.is_boot_services()),
    })
}
