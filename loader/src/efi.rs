//! 이 로더가 쓰는 만큼의 UEFI 인터페이스입니다.
//!
//! # Features
//! 시스템 테이블, 부트 서비스의 일부, 텍스트 출력, 구성 테이블, 난수
//! 프로토콜의 구조를 UEFI 규격의 배치대로 손으로 옮겼습니다. 외부 크레이트를
//! 쓰지 않습니다. 펌웨어를 부르는 unsafe는 이 모듈의 메서드 안에 모아 두고,
//! 바깥에는 결과만 내줍니다.
//!
//! 펌웨어는 이 로더보다 먼저, 같거나 더 높은 권한으로 실행됩니다. 그래서
//! 펌웨어가 준 포인터와 값은 규격대로라고 믿습니다. 값이 틀렸을 때를 걸러내는
//! 검사는 순수 로직 크레이트(k0-efi)가 맡습니다.

use core::ffi::c_void;
use core::fmt;
use core::sync::atomic::{AtomicPtr, Ordering};

pub type Handle = *mut c_void;

/// EFI_STATUS 값입니다. 최상위 비트가 서면 오류입니다.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Status(pub usize);

impl Status {
    pub const SUCCESS: Status = Status(0);
    const ERROR: usize = 1 << (usize::BITS - 1);
    pub const LOAD_ERROR: Status = Status(Self::ERROR | 1);
    pub const INVALID_PARAMETER: Status = Status(Self::ERROR | 2);
    pub const BUFFER_TOO_SMALL: Status = Status(Self::ERROR | 5);
}

impl fmt::Debug for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "efi status {:#x}", self.0)
    }
}

#[repr(C)]
struct TableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
}

#[repr(C)]
struct TextOutput {
    reset: usize,
    output_string: extern "efiapi" fn(*mut TextOutput, *const u16) -> Status,
}

#[repr(C)]
struct BootServices {
    header: TableHeader,
    raise_tpl: usize,
    restore_tpl: usize,
    allocate_pages: extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    free_pages: extern "efiapi" fn(u64, usize) -> Status,
    get_memory_map: extern "efiapi" fn(*mut usize, *mut u8, *mut usize, *mut usize, *mut u32) -> Status,
    allocate_pool: usize,
    free_pool: usize,
    create_event: usize,
    set_timer: usize,
    wait_for_event: usize,
    signal_event: usize,
    close_event: usize,
    check_event: usize,
    install_protocol_interface: usize,
    reinstall_protocol_interface: usize,
    uninstall_protocol_interface: usize,
    handle_protocol: usize,
    reserved: usize,
    register_protocol_notify: usize,
    locate_handle: usize,
    locate_device_path: usize,
    install_configuration_table: usize,
    load_image: usize,
    start_image: usize,
    exit: usize,
    unload_image: usize,
    exit_boot_services: extern "efiapi" fn(Handle, usize) -> Status,
    get_next_monotonic_count: usize,
    stall: usize,
    set_watchdog_timer: extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    connect_controller: usize,
    disconnect_controller: usize,
    open_protocol: usize,
    close_protocol: usize,
    open_protocol_information: usize,
    protocols_per_handle: usize,
    locate_handle_buffer: usize,
    locate_protocol: extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
}

/// EFI_GUID의 메모리 배치 그대로의 16바이트입니다.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid(pub [u8; 16]);

/// 펌웨어가 디바이스 트리를 넘길 때 구성 테이블에 쓰는 GUID
/// (b1b621d5-f19c-41a5-830b-d9152c69aae0)
pub const DTB_TABLE: Guid =
    Guid([0xd5, 0x21, 0xb6, 0xb1, 0x9c, 0xf1, 0xa5, 0x41, 0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0]);

/// 펌웨어가 ACPI 2.0 이상의 RSDP를 넘길 때 구성 테이블에 쓰는 GUID
/// (8868e871-e4f1-11d3-bc22-0080c73c8881)
pub const ACPI_TABLE: Guid =
    Guid([0x71, 0xe8, 0x68, 0x88, 0xf1, 0xe4, 0xd3, 0x11, 0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81]);

/// EFI_RNG_PROTOCOL의 GUID (3152bca5-eade-433d-862e-c01cdc291f44)
const RNG_PROTOCOL: Guid =
    Guid([0xa5, 0xbc, 0x52, 0x31, 0xde, 0xea, 0x3d, 0x43, 0x86, 0x2e, 0xc0, 0x1c, 0xdc, 0x29, 0x1f, 0x44]);

#[repr(C)]
struct ConfigurationTable {
    guid: Guid,
    table: *mut c_void,
}

#[repr(C)]
struct RngProtocol {
    get_info: usize,
    get_rng: extern "efiapi" fn(*mut RngProtocol, *const Guid, usize, *mut u8) -> Status,
}

#[repr(C)]
pub struct SystemTable {
    header: TableHeader,
    firmware_vendor: *const u16,
    firmware_revision: u32,
    console_in_handle: Handle,
    console_in: *mut c_void,
    console_out_handle: Handle,
    console_out: *mut TextOutput,
    standard_error_handle: Handle,
    standard_error: *mut TextOutput,
    runtime_services: *mut c_void,
    boot_services: *mut BootServices,
    number_of_table_entries: usize,
    configuration_table: *mut ConfigurationTable,
}

/// AllocatePages의 할당 방식: 주어진 주소 이하에서 아무 곳
const ALLOCATE_MAX_ADDRESS: u32 = 1;
/// EfiLoaderData 메모리 타입
const LOADER_DATA: u32 = 2;
/// "IBI SYST": 시스템 테이블 헤더의 서명
const SYSTEM_TABLE_SIGNATURE: u64 = 0x5453_5953_2049_4249;

pub const PAGE: u64 = 4096;

/// GetMemoryMap이 돌려준 맵의 크기 정보입니다.
#[derive(Clone, Copy)]
pub struct MapInfo {
    pub size: usize,
    pub key: usize,
    pub stride: usize,
}

/// 부트 서비스가 살아 있는 동안의 펌웨어 핸들입니다.
#[derive(Clone, Copy)]
pub struct Firmware {
    image: Handle,
    table: *mut SystemTable,
}

impl Firmware {
    /// 진입점이 받은 인자로 핸들을 만드는 함수입니다.
    ///
    /// # Safety
    /// `table`은 펌웨어가 이 이미지의 진입점에 넘긴 시스템 테이블 포인터여야
    /// 하고, ExitBootServices가 성공한 뒤에는 이 핸들의 어느 메서드도 부르면
    /// 안 됩니다.
    pub unsafe fn new(image: Handle, table: *mut SystemTable) -> Option<Self> {
        if table.is_null() {
            return None;
        }
        // SAFETY: 호출자가 펌웨어의 시스템 테이블임을 보장함
        let st = unsafe { &*table };
        if st.header.signature != SYSTEM_TABLE_SIGNATURE || st.boot_services.is_null() {
            return None;
        }
        Some(Self { image, table })
    }

    fn boot(&self) -> &BootServices {
        // SAFETY: new가 널이 아님을 확인했고, 부트 서비스 테이블은
        //         ExitBootServices 전까지 유효함(이 핸들의 사용 조건)
        unsafe { &*(*self.table).boot_services }
    }

    /// 펌웨어 콘솔에 쓰는 출력 핸들을 주는 함수입니다.
    pub fn console(&self) -> Console {
        // SAFETY: 시스템 테이블은 이 핸들의 사용 조건에 따라 유효함
        Console { out: unsafe { (*self.table).console_out } }
    }

    /// 구성 테이블에서 GUID가 같은 항목의 포인터를 찾는 함수입니다.
    ///
    /// # Arguments
    /// `guid` - 찾을 테이블의 GUID
    pub fn configuration_table(&self, guid: &Guid) -> Option<*const u8> {
        // SAFETY: 시스템 테이블이 유효하고, 구성 테이블은 항목 수만큼의 배열임
        let (count, base) = unsafe { ((*self.table).number_of_table_entries, (*self.table).configuration_table) };
        if base.is_null() {
            return None;
        }
        (0..count)
            // SAFETY: i는 항목 수 안쪽
            .map(|i| unsafe { &*base.add(i) })
            .find(|entry| entry.guid == *guid)
            .map(|entry| entry.table as *const u8)
            .filter(|p| !p.is_null())
    }

    /// 주어진 주소 아래에서 EfiLoaderData 페이지를 할당하는 함수입니다.
    ///
    /// # Arguments
    /// `pages` - 4KiB 페이지 수
    /// `below` - 할당 구간의 끝이 넘으면 안 되는 주소
    pub fn allocate_below(&self, pages: usize, below: u64) -> Result<u64, Status> {
        let mut address = below.saturating_sub(1);
        match (self.boot().allocate_pages)(ALLOCATE_MAX_ADDRESS, LOADER_DATA, pages, &mut address) {
            Status::SUCCESS => Ok(address),
            e => Err(e),
        }
    }

    /// 할당한 페이지를 돌려주는 함수입니다.
    ///
    /// # Arguments
    /// `address` - 할당이 돌려준 시작 주소
    /// `pages` - 할당할 때의 페이지 수
    pub fn free(&self, address: u64, pages: usize) {
        let _ = (self.boot().free_pages)(address, pages);
    }

    /// 메모리 맵에 필요한 버퍼 크기와 디스크립터 간격을 묻는 함수입니다.
    pub fn memory_map_size(&self) -> Result<MapInfo, Status> {
        let (mut size, mut key, mut stride, mut version) = (0usize, 0usize, 0usize, 0u32);
        match (self.boot().get_memory_map)(&mut size, core::ptr::null_mut(), &mut key, &mut stride, &mut version) {
            Status::BUFFER_TOO_SMALL if stride != 0 => Ok(MapInfo { size, key, stride }),
            e => Err(e),
        }
    }

    /// 메모리 맵을 버퍼에 받는 함수입니다. 아무것도 할당하지 않습니다.
    ///
    /// # Arguments
    /// `buffer` - 맵을 받을 버퍼
    pub fn memory_map(&self, buffer: &mut [u8]) -> Result<MapInfo, Status> {
        let (mut size, mut key, mut stride, mut version) = (buffer.len(), 0usize, 0usize, 0u32);
        match (self.boot().get_memory_map)(&mut size, buffer.as_mut_ptr(), &mut key, &mut stride, &mut version) {
            Status::SUCCESS if size <= buffer.len() => Ok(MapInfo { size, key, stride }),
            Status::SUCCESS => Err(Status::BUFFER_TOO_SMALL),
            e => Err(e),
        }
    }

    /// 부트 서비스를 끝내는 함수입니다.
    ///
    /// 성공하면 이 핸들과 콘솔을 더 써서는 안 됩니다. 실패한 뒤에도 펌웨어가
    /// 이미 일부를 정리했을 수 있으므로, 호출자는 실패를 펌웨어로 돌아가는
    /// 사유로 삼지 않습니다.
    ///
    /// # Arguments
    /// `key` - 방금 받은 메모리 맵의 키
    pub fn exit_boot_services(&self, key: usize) -> Status {
        (self.boot().exit_boot_services)(self.image, key)
    }

    /// 펌웨어의 감시 타이머를 끄는 함수입니다.
    pub fn disable_watchdog(&self) {
        let _ = (self.boot().set_watchdog_timer)(0, 0, 0, core::ptr::null());
    }

    /// 펌웨어의 난수 프로토콜에서 바이트를 받는 함수입니다. 프로토콜이 없거나
    /// 실패하면 `false`를 주고 버퍼를 0으로 지웁니다.
    ///
    /// # Arguments
    /// `out` - 난수를 받을 버퍼
    pub fn random(&self, out: &mut [u8]) -> bool {
        let mut interface: *mut c_void = core::ptr::null_mut();
        let found = (self.boot().locate_protocol)(&RNG_PROTOCOL, core::ptr::null_mut(), &mut interface);
        if found != Status::SUCCESS || interface.is_null() {
            return false;
        }
        let rng = interface as *mut RngProtocol;
        // SAFETY: LocateProtocol이 성공하며 돌려준 EFI_RNG_PROTOCOL 인터페이스임
        let status = unsafe { ((*rng).get_rng)(rng, core::ptr::null(), out.len(), out.as_mut_ptr()) };
        // 실패했거나, 성공이라면서 한 가지 값으로만 채운 출력은 엔트로피로 치지 않음
        if status != Status::SUCCESS || out.iter().all(|&b| b == out[0]) {
            wipe(out);
            return false;
        }
        true
    }
}

/// 패닉 처리기가 쓸 펌웨어 콘솔입니다. 부트 서비스가 살아 있는 동안만 값이
/// 있습니다.
static PANIC_CONSOLE: AtomicPtr<TextOutput> = AtomicPtr::new(core::ptr::null_mut());

/// 펌웨어 콘솔 출력입니다. `\n`은 `\r\n`으로 나갑니다.
pub struct Console {
    out: *mut TextOutput,
}

impl Console {
    /// 이 콘솔을 패닉 처리기가 쓸 수 있게 등록하는 함수입니다.
    pub fn arm_for_panic(&self) {
        PANIC_CONSOLE.store(self.out, Ordering::Relaxed);
    }

    /// 패닉 처리기가 더는 콘솔을 쓰지 못하게 하는 함수입니다. 부트 서비스를
    /// 끝내려고 시도하기 전에 부릅니다.
    pub fn disarm_for_panic() {
        PANIC_CONSOLE.store(core::ptr::null_mut(), Ordering::Relaxed);
    }

    /// 패닉 처리기용 콘솔을 주는 함수입니다. 등록돼 있지 않으면 `None`
    pub fn for_panic() -> Option<Console> {
        let out = PANIC_CONSOLE.load(Ordering::Relaxed);
        (!out.is_null()).then_some(Console { out })
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.out.is_null() {
            return Ok(());
        }
        let mut buffer = [0u16; 96];
        let mut used = 0;
        let flush = |buffer: &mut [u16; 96], used: &mut usize| {
            buffer[*used] = 0;
            // SAFETY: out은 펌웨어의 텍스트 출력 프로토콜이고 버퍼는 0으로 끝남
            let _ = unsafe { ((*self.out).output_string)(self.out, buffer.as_ptr()) };
            *used = 0;
        };
        for unit in s.encode_utf16() {
            if unit == u16::from(b'\n') {
                buffer[used] = u16::from(b'\r');
                used += 1;
            }
            buffer[used] = unit;
            used += 1;
            if used >= buffer.len() - 3 {
                flush(&mut buffer, &mut used);
            }
        }
        if used > 0 {
            flush(&mut buffer, &mut used);
        }
        Ok(())
    }
}

/// 메모리 맵이 허락하는 범위만 읽게 해 주는 통로입니다. ACPI 테이블의 주소는
/// 펌웨어가 준 값이라 틀릴 수 있으므로, 맵에 RAM이나 펌웨어 메모리로 올라 있는
/// 한 디스크립터 안에 통째로 든 범위만 읽습니다. 디바이스 레지스터와 맵에 없는
/// 주소는 읽지 않습니다.
pub struct MapMemory<'a> {
    pub map: k0_efi::memmap::Map<'a>,
}

impl k0_efi::acpi::Memory for MapMemory<'_> {
    fn bytes(&self, addr: u64, len: usize) -> Option<&[u8]> {
        let end = addr.checked_add(len as u64)?;
        // 로더·부트 서비스·런타임 서비스의 코드와 데이터, 빈 메모리, ACPI 재사용과 NVS
        let readable = |kind: u32| matches!(kind, 1..=7 | 9 | 10);
        self.map.iter().find(|d| readable(d.kind) && d.start <= addr && end <= d.end)?;
        // SAFETY: 범위 전체가 펌웨어의 메모리 맵에 메모리로 올라 있는 한
        //         디스크립터 안에 있고, UEFI는 그런 메모리를 identity로 매핑해 둠
        Some(unsafe { core::slice::from_raw_parts(addr as *const u8, len) })
    }
}

/// 비밀이 지나간 버퍼를 지우는 함수입니다. 쓰기가 최적화로 사라지지 않게
/// 휘발성 쓰기를 쓰고 뒤에 울타리를 둡니다.
///
/// # Arguments
/// `buffer` - 지울 버퍼
pub fn wipe(buffer: &mut [u8]) {
    for b in buffer.iter_mut() {
        // SAFETY: b는 유효한 가변 참조에서 온 포인터임
        unsafe { core::ptr::write_volatile(b, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}
