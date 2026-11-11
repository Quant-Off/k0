//! 로더의 실행 흐름입니다.
//!
//! # Features
//! 진입점 `efi_main`과, 펌웨어를 부르는 순서가 여기 있습니다. 계산은 전부
//! k0-efi가 하고, 이 모듈은 펌웨어에서 입력을 받아 넘기고 결과를 메모리에
//! 놓는 일만 합니다.

use core::fmt::{self, Write};
use core::ops::Range;

use k0_boot::{Fdt, MemRegion, BOOT_WINDOW_SIZE};
use k0_efi::fdtw::root_cells;
use k0_efi::image::{check_image, ImageError};
use k0_efi::memmap::{Limits, Map, PlanError, ALIGN};
use k0_efi::prepare::{firmware_dtb_size, prepare, PrepareError, Prepared, Request, Work};
use sha2::{Digest, Sha256};

use crate::efi::{wipe, Console, Firmware, Handle, Status, SystemTable, DTB_TABLE, PAGE};
use crate::handoff::{current_el, enter_kernel, mmfr0, park};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/kernel_gen.rs"));
}
use generated::{KERNEL_GRANULE, KERNEL_IMAGE, KERNEL_IMAGE_SIZE, KERNEL_SHA256};

// 루트 태스크는 내장한 커널 Image 안에 있음. 로더에 또 실리면 빌드 구성이 틀린 것
const _: () = assert!(!k0_boot::HAS_ROOTTASK);

/// 블록을 다른 자리에 다시 잡아 보는 횟수의 상한
const PLACE_ATTEMPTS: usize = 4;
/// ExitBootServices를 다시 시도하는 횟수의 상한
const EXIT_ATTEMPTS: usize = 4;
/// 맵 버퍼에 두는 여유 디스크립터 수(할당과 펌웨어의 타이머 처리로 맵이 늘어남)
const MAP_SLACK: usize = 32;
/// 로더가 `rng-seed`에 더하는 바이트 수
const SEED_LEN: usize = 32;

/// 부팅을 거부하는 이유를 나타내는 열거형입니다.
enum Refusal {
    Image(ImageError),
    ImageChanged,
    ImageCorrupt,
    Granule,
    NoDeviceTree,
    BadDeviceTree,
    DeviceTreeInBlock,
    Firmware(&'static str, Status),
    Prepare(PrepareError),
    NoPlace,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Image(e) => write!(f, "embedded kernel image header: {e:?}"),
            Refusal::ImageChanged => f.write_str("embedded kernel image differs from the one checked at build time"),
            Refusal::ImageCorrupt => f.write_str("embedded kernel image is corrupt (sha-256 mismatch)"),
            Refusal::Granule => write!(f, "this cpu has no {}K translation granule", KERNEL_GRANULE / 1024),
            Refusal::NoDeviceTree => f.write_str(
                "the firmware provides no device tree (ACPI-only firmware is not supported, QEMU needs -machine acpi=off)",
            ),
            Refusal::BadDeviceTree => f.write_str("the firmware device tree has a bad header"),
            Refusal::DeviceTreeInBlock => f.write_str("the firmware device tree lies in memory the firmware gave to the loader"),
            Refusal::Firmware(what, status) => write!(f, "{what} failed: {status:?}"),
            Refusal::Prepare(e) => write!(f, "device tree for the kernel: {e:?}"),
            Refusal::NoPlace => f.write_str("no usable memory for the kernel image block"),
        }
    }
}

/// 할당 하나를 나타내는 구조체입니다.
#[derive(Clone, Copy)]
struct Pages {
    base: u64,
    count: usize,
}

/// 실패했을 때 되돌릴 것들을 모아 둔 상태입니다.
struct Loader {
    fw: Firmware,
    con: Console,
    work: Option<Pages>,
    block: Option<Pages>,
    /// 블록 안의 DTB 버퍼(거부할 때 엔트로피가 남지 않게 지움)
    dtb: Range<u64>,
    seed: [u8; SEED_LEN],
    seed_len: usize,
}

/// 블록의 배치와 작업 공간입니다.
struct Ready {
    firmware_dtb: &'static [u8],
    map_buffer: &'static mut [u8],
    points: &'static mut [u64],
    ranges: &'static mut [MemRegion],
    holes: &'static mut [MemRegion],
    dtb: Range<u64>,
    image: Range<u64>,
    tail_size: u64,
    device_limit: u64,
}

impl Ready {
    /// 로더가 할당해 쓰는 블록 전체의 범위
    fn block(&self) -> Range<u64> {
        self.dtb.start..self.image.end + self.tail_size
    }
}

fn align_up(v: u64, to: u64) -> u64 {
    (v + to - 1) & !(to - 1)
}

/// 내장한 커널 Image를 확인하는 함수입니다.
fn check_kernel() -> Result<(), Refusal> {
    let info = check_image(KERNEL_IMAGE).map_err(Refusal::Image)?;
    if info.image_size != KERNEL_IMAGE_SIZE || info.granule != KERNEL_GRANULE {
        return Err(Refusal::ImageChanged);
    }
    let hash: [u8; 32] = Sha256::digest(KERNEL_IMAGE).into();
    if hash != KERNEL_SHA256 {
        return Err(Refusal::ImageCorrupt);
    }
    // 커널의 enable_paging과 같은 판정. 여기서 걸러야 사유가 화면에 남음
    let id = mmfr0();
    let supported = match KERNEL_GRANULE {
        4096 => matches!((id >> 28) & 0xF, 0 | 1),
        _ => matches!((id >> 20) & 0xF, 1 | 2),
    };
    if supported { Ok(()) } else { Err(Refusal::Granule) }
}

/// 이 CPU의 물리 주소 폭이 정하는 디바이스 주소의 상한을 주는 함수입니다.
fn device_limit() -> u64 {
    let bits = match mmfr0() & 0xF {
        0 => 32,
        1 => 36,
        2 => 40,
        3 => 42,
        4 => 44,
        _ => 48,
    };
    1 << bits
}

/// 펌웨어가 구성 테이블로 준 디바이스 트리를 찾는 함수입니다.
fn firmware_dtb(fw: &Firmware) -> Result<&'static [u8], Refusal> {
    let ptr = fw.configuration_table(&DTB_TABLE).ok_or(Refusal::NoDeviceTree)?;
    // SAFETY: 펌웨어가 DTB 테이블로 준 포인터라 적어도 헤더는 읽을 수 있음
    let head = unsafe { core::slice::from_raw_parts(ptr, 8) };
    let size = firmware_dtb_size(head).ok_or(Refusal::BadDeviceTree)?;
    // SAFETY: 헤더의 totalsize만큼이 DTB임. 이 메모리는 부트 서비스가 끝날
    //         때까지 유효하고, 로더는 그 뒤에 이 슬라이스를 읽지 않음
    Ok(unsafe { core::slice::from_raw_parts(ptr, size) })
}

/// 맵 버퍼와 계획의 작업 공간을 한 번에 할당하는 함수입니다.
fn allocate_work(
    l: &mut Loader,
) -> Result<(&'static mut [u8], &'static mut [u64], &'static mut [MemRegion], &'static mut [MemRegion]), Refusal> {
    let info = l.fw.memory_map_size().map_err(|e| Refusal::Firmware("GetMemoryMap", e))?;
    let descriptors = info.size / info.stride + MAP_SLACK;
    let map_bytes = align_up((descriptors * info.stride) as u64, 8) as usize;
    let (np, nr, nh) = Work::need(descriptors);
    let total = map_bytes + np * 8 + (nr + nh) * core::mem::size_of::<MemRegion>();
    let count = align_up(total as u64, PAGE) as usize / PAGE as usize;
    let base = l
        .fw
        .allocate_below(count, k0_efi::memmap::Limits::from_cells(2, 2).max_addr)
        .map_err(|e| Refusal::Firmware("AllocatePages", e))?;
    l.work = Some(Pages { base, count });

    let start = base as *mut u8;
    // SAFETY: 방금 할당한 count 페이지를 0으로 채운 뒤, 겹치지 않는 네 구간으로
    //         나눠 씀. 페이지 정렬이라 u64와 MemRegion의 정렬을 만족하고, 0은
    //         두 타입 모두에 유효한 값임
    unsafe {
        core::ptr::write_bytes(start, 0, count * PAGE as usize);
        let points = start.add(map_bytes) as *mut u64;
        let ranges = points.add(np) as *mut MemRegion;
        let holes = ranges.add(nr);
        Ok((
            core::slice::from_raw_parts_mut(start, map_bytes),
            core::slice::from_raw_parts_mut(points, np),
            core::slice::from_raw_parts_mut(ranges, nr),
            core::slice::from_raw_parts_mut(holes, nh),
        ))
    }
}

/// 지금의 메모리 맵으로 커널용 DTB를 만들고 미리 검사하는 함수입니다. 맵의
/// 키도 함께 돌려줍니다. 아무것도 할당하지 않습니다.
fn prepare_now(l: &Loader, r: &mut Ready) -> Result<(Prepared, usize), Refusal> {
    let info = l.fw.memory_map(r.map_buffer).map_err(|e| Refusal::Firmware("GetMemoryMap", e))?;
    let map = Map::new(&r.map_buffer[..info.size], info.stride).map_err(|e| Refusal::Prepare(PrepareError::Plan(e)))?;
    let request = Request {
        firmware_dtb: r.firmware_dtb,
        map,
        block_base: r.dtb.start,
        image_size: r.image.end - r.image.start,
        image_len: KERNEL_IMAGE_SIZE,
        tail_size: r.tail_size,
        granule: KERNEL_GRANULE,
        device_limit: r.device_limit,
        seed: &l.seed[..l.seed_len],
    };
    let work = Work { points: &mut *r.points, ranges: &mut *r.ranges, holes: &mut *r.holes };
    // SAFETY: DTB 버퍼는 이 로더가 할당한 블록의 맨 앞 구간이고, 이 슬라이스가
    //         살아 있는 동안 다른 참조가 없음
    let out = unsafe { core::slice::from_raw_parts_mut(r.dtb.start as *mut u8, (r.dtb.end - r.dtb.start) as usize) };
    let prepared = prepare(&request, work, out).map_err(Refusal::Prepare)?;
    Ok((prepared, info.key))
}

/// 부트 서비스를 끝내기 직전까지의 모든 준비를 하는 함수입니다.
fn get_ready(l: &mut Loader) -> Result<Ready, Refusal> {
    check_kernel()?;
    let firmware_dtb = firmware_dtb(&l.fw)?;
    let fdt = Fdt::new(firmware_dtb).map_err(|e| Refusal::Prepare(PrepareError::FirmwareDtb(e)))?;
    let (addr_cells, size_cells) = root_cells(&fdt).map_err(|e| Refusal::Prepare(PrepareError::Rewrite(e)))?;
    let mut below = Limits::from_cells(addr_cells, size_cells).max_addr;

    l.seed_len = if l.fw.random(&mut l.seed) { SEED_LEN } else { 0 };
    let entropy = if l.seed_len != 0 { "from the firmware" } else { "none from the firmware" };
    let _ = writeln!(l.con, "k0-loader: entropy {entropy}");

    let (map_buffer, points, ranges, holes) = allocate_work(l)?;

    // 재작성기는 문자열 블록을 버퍼 끝에 모았다가 옮기므로 입력의 두 배면 넉넉함
    let dtb_cap = align_up(2 * firmware_dtb.len() as u64 + 64 * 1024, ALIGN);
    let image_size = align_up(KERNEL_IMAGE_SIZE, ALIGN);
    let tail_size = BOOT_WINDOW_SIZE + ALIGN;
    // 펌웨어는 4KiB 정렬만 보장하므로 한 단위에서 한 페이지를 뺀 만큼 더 받아 맞춤
    let count = ((dtb_cap + image_size + tail_size + ALIGN - PAGE) / PAGE) as usize;

    let mut ready = Ready {
        firmware_dtb,
        map_buffer,
        points,
        ranges,
        holes,
        dtb: 0..0,
        image: 0..0,
        tail_size,
        device_limit: device_limit(),
    };
    for _ in 0..PLACE_ATTEMPTS {
        let raw = l.fw.allocate_below(count, below).map_err(|e| Refusal::Firmware("AllocatePages", e))?;
        let base = align_up(raw, ALIGN);
        l.block = Some(Pages { base: raw, count });
        ready.dtb = base..base + dtb_cap;
        ready.image = ready.dtb.end..ready.dtb.end + image_size;
        l.dtb = ready.dtb.clone();
        // 펌웨어가 준 DTB가 방금 받은 메모리와 겹치면 펌웨어의 맵이 틀린 것.
        // 읽으면서 덮어쓰게 되므로 진행하지 않음
        let source = firmware_dtb.as_ptr() as u64..firmware_dtb.as_ptr() as u64 + firmware_dtb.len() as u64;
        let work = l.work.map_or(0..0, |w| w.base..w.base + w.count as u64 * PAGE);
        let taken = raw..raw + count as u64 * PAGE;
        if [&taken, &work].iter().any(|r| source.start < r.end && r.start < source.end) {
            return Err(Refusal::DeviceTreeInBlock);
        }
        match prepare_now(l, &mut ready) {
            Ok((p, _)) => {
                report(l, &ready, &p);
                // SAFETY: 이미지 영역은 방금 할당한 블록 안이고 image_size는 파일
                //         길이 이상임(헤더 검사). 파일 뒤(BSS와 부트 스택 자리)는
                //         0으로 채움
                unsafe {
                    let dst = ready.image.start as *mut u8;
                    core::ptr::copy_nonoverlapping(KERNEL_IMAGE.as_ptr(), dst, KERNEL_IMAGE.len());
                    core::ptr::write_bytes(dst.add(KERNEL_IMAGE.len()), 0, image_size as usize - KERNEL_IMAGE.len());
                }
                return Ok(ready);
            }
            // 펌웨어의 예약이나 디바이스 구간이 블록에 걸침. 더 낮은 곳에 다시 잡음
            Err(Refusal::Prepare(PrepareError::Plan(PlanError::BlockNotUsable))) => {
                // SAFETY: DTB 버퍼는 아직 풀지 않은 블록 안의 구간임
                wipe(unsafe { core::slice::from_raw_parts_mut(ready.dtb.start as *mut u8, dtb_cap as usize) });
                l.fw.free(raw, count);
                l.block = None;
                below = raw;
            }
            Err(e) => return Err(e),
        }
    }
    Err(Refusal::NoPlace)
}

/// 커널에 넘길 내용을 펌웨어 콘솔에 알리는 함수입니다.
fn report(l: &mut Loader, r: &Ready, p: &Prepared) {
    const MIB: u64 = 1024 * 1024;
    let _ = writeln!(l.con, "k0-loader: platform {}", p.platform);
    if let Some(why) = p.no_console {
        let _ = writeln!(l.con, "k0-loader: the kernel will have no console ({why:?})");
    }
    let _ = writeln!(
        l.con,
        "k0-loader: memory {} regions, {} of {} MiB usable kept, {} ranges dropped, {} MiB of boot services memory left unused",
        p.plan.regions().len(),
        p.plan.kept_bytes / MIB,
        p.plan.usable_bytes / MIB,
        p.plan.dropped,
        p.boot_services_bytes / MIB,
    );
    let _ = writeln!(l.con, "k0-loader: image at {:#x}, dtb at {:#x}", r.image.start, r.dtb.start);
}

/// 부트 서비스를 끝내고 커널로 넘어가는 함수입니다.
fn exit_and_enter(l: &mut Loader, mut ready: Ready) -> Result<core::convert::Infallible, Refusal> {
    let _ = writeln!(l.con, "k0-loader: exiting boot services");
    l.fw.disable_watchdog();
    // 여기서부터는 펌웨어 콘솔에 기대지 않음
    Console::disarm_for_panic();
    let mut attempted = false;
    for _ in 0..EXIT_ATTEMPTS {
        // 맵이 바뀌었을 수 있으므로 새 맵으로 DTB를 다시 만듦. 여기서부터 성공할
        // 때까지 할당도 출력도 하지 않음
        let key = match prepare_now(l, &mut ready) {
            Ok((_, key)) => key,
            Err(_) if attempted => park(),
            Err(e) => return Err(e),
        };
        attempted = true;
        match l.fw.exit_boot_services(key) {
            Status::SUCCESS => {
                wipe(&mut l.seed);
                // SAFETY: 부트 서비스가 끝났고, 블록은 이 로더가 할당해 채운
                //         메모리이며, 이미지는 헤더와 해시와 그래뉼 지원을 확인함
                unsafe { enter_kernel(&ready.block(), ready.image.start, ready.dtb.start) }
            }
            Status::INVALID_PARAMETER => {}
            _ => park(),
        }
    }
    park()
}

/// 부팅을 거부하는 함수입니다. 사유를 출력하고, 남긴 것을 지우고 돌려준 뒤
/// 펌웨어에 오류를 반환합니다.
fn refuse(l: &mut Loader, why: &Refusal) -> Status {
    let _ = writeln!(l.con, "k0-loader: refused: {why}");
    wipe(&mut l.seed);
    if let Some(p) = l.block.take() {
        if l.dtb.end > l.dtb.start {
            // SAFETY: DTB 버퍼는 아직 풀지 않은 블록 안의 구간임
            wipe(unsafe { core::slice::from_raw_parts_mut(l.dtb.start as *mut u8, (l.dtb.end - l.dtb.start) as usize) });
        }
        l.fw.free(p.base, p.count);
    }
    if let Some(p) = l.work.take() {
        l.fw.free(p.base, p.count);
    }
    if cfg!(feature = "refuse-halt") {
        park()
    }
    Status::LOAD_ERROR
}

/// UEFI가 부르는 진입점입니다.
///
/// # Arguments
/// `image` - 이 이미지의 핸들
/// `table` - 시스템 테이블
#[unsafe(no_mangle)]
pub extern "efiapi" fn efi_main(image: Handle, table: *mut SystemTable) -> Status {
    // SAFETY: 펌웨어가 진입점에 넘긴 시스템 테이블이고, 부트 서비스가 끝난 뒤에는
    //         이 핸들을 쓰지 않음(exit_and_enter가 곧바로 커널로 넘어감)
    let Some(fw) = (unsafe { Firmware::new(image, table) }) else {
        return Status::LOAD_ERROR;
    };
    let mut l =
        Loader { fw, con: fw.console(), work: None, block: None, dtb: 0..0, seed: [0; SEED_LEN], seed_len: 0 };
    l.con.arm_for_panic();
    // 펌웨어가 화면 제어 문자열을 남긴 줄과 섞이지 않게 빈 줄부터 씀
    let _ = writeln!(
        l.con,
        "\nk0-loader: start at el{}, kernel image {} KiB with {}K granule, sha-256 {:02x}{:02x}{:02x}{:02x}",
        current_el(),
        KERNEL_IMAGE_SIZE / 1024,
        KERNEL_GRANULE / 1024,
        KERNEL_SHA256[0],
        KERNEL_SHA256[1],
        KERNEL_SHA256[2],
        KERNEL_SHA256[3],
    );
    let result = match get_ready(&mut l) {
        Ok(ready) => exit_and_enter(&mut l, ready),
        Err(e) => Err(e),
    };
    match result {
        Ok(never) => match never {},
        Err(why) => refuse(&mut l, &why),
    }
}

/// 패닉 처리기입니다. 부트 서비스가 살아 있으면 위치를 펌웨어 콘솔에 남기고
/// 멈춥니다. 펌웨어의 감시 타이머가 살아 있는 시점이라면 기계가 재시작됩니다.
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    if let Some(mut con) = Console::for_panic() {
        Console::disarm_for_panic();
        match info.location() {
            Some(at) => {
                let _ = writeln!(con, "k0-loader: panic at {}:{}", at.file(), at.line());
            }
            None => {
                let _ = writeln!(con, "k0-loader: panic");
            }
        }
    }
    park()
}
