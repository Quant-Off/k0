//! 진입 페이즈 1 초기 페이지 테이블 구성과 MMU 활성화 모듈입니다.
//!
//! # Features
//! 커널 이미지(W^X 분리), DTB(RO), earlycon MMIO(Device)만 담는 최소 테이블을
//! .bss의 정적 풀에서 구성합니다. 블록 매핑 없이 최하위 레벨 페이지로만 매핑해
//! 코드 경로를 하나로 유지합니다. TTBR0은 identity 매핑(전환과 MMIO용, 진입
//! 페이즈 3에서 사용자 공간용으로 교체 예정), TTBR1은 higher-half 선형
//! 별칭(PA + `KERNEL_VA_OFFSET`)입니다. `SCTLR_EL1.WXN`을 켜서 쓰기 가능한
//! 페이지는 하드웨어 수준에서 실행이 거부됩니다. 부트 스택 아래 가드 페이지는
//! 어느 반쪽에도 매핑하지 않아 스택 오버플로가 즉시 폴트가 됩니다.
//!
//! # Errors
//! 테이블 풀 고갈, 정렬 위반, 중복 매핑, 미지원 그래뉼, 재진입은 전부
//! `MmuError`로 반환합니다. 호출자는 실패 시 부팅을 중단해야 합니다(fail-secure).
//!
//! # Examples
//! ```rust,ignore
//! let layout = k0_mm::KernelLayout { /* 링커 심볼로 채움 */ };
//! let _mmu = k0_mm::enable_paging(&layout)?;
//! assert!(!k0_mm::can_write(text_start));
//! ```

use core::arch::asm;
use core::cell::UnsafeCell;
use core::ops::Range;

/// 변환 그래뉼(페이지 크기)
#[cfg(feature = "plat-virt")]
pub const GRANULE: usize = 4096;
#[cfg(feature = "plat-apple")]
pub const GRANULE: usize = 16384;

/// TTBR1 higher-half 선형 별칭의 VA 오프셋 (정의는 k0-abi, 링커 스크립트와 일치)
pub use k0_abi::KERNEL_VA_OFFSET;

const PAGE_SHIFT: u32 = GRANULE.trailing_zeros();
const BITS_PER_LEVEL: u32 = PAGE_SHIFT - 3;
const ENTRIES: usize = GRANULE / 8;
const POOL_LEN: usize = 24;
pub(crate) const ADDR_MASK: u64 = ((1u64 << 48) - 1) & !(GRANULE as u64 - 1);

pub(crate) const DESC_TABLE: u64 = 0b11;
pub(crate) const DESC_PAGE: u64 = 0b11;
pub(crate) const ATTR_AF: u64 = 1 << 10;
pub(crate) const SH_INNER: u64 = 0b11 << 8;
const SH_OUTER: u64 = 0b10 << 8;
const AP_RO: u64 = 0b10 << 6;
const AP_RW: u64 = 0b00 << 6;
/// EL1 RO + EL0 RO, 사용자 텍스트/rodata용
pub(crate) const AP_RO_ALL: u64 = 0b11 << 6;
/// EL1 RW + EL0 RW, 사용자 데이터/스택용
pub(crate) const AP_RW_ALL: u64 = 0b01 << 6;
pub(crate) const PXN: u64 = 1 << 53;
pub(crate) const UXN: u64 = 1 << 54;
pub(crate) const IDX_NORMAL: u64 = 0 << 2;
const IDX_DEVICE: u64 = 1 << 2;

/// MAIR_EL1: idx0 = Normal WB WA, idx1 = Device-nGnRE
const MAIR: u64 = 0xFF | (0x04 << 8);

const SCTLR_M: u64 = 1 << 0;
const SCTLR_C: u64 = 1 << 2;
const SCTLR_I: u64 = 1 << 12;
const SCTLR_WXN: u64 = 1 << 19;

/// 매핑 권한을 나타내는 열거형입니다.
///
/// 모든 커널 매핑은 UXN=1이며, 실행 가능한 것은 [Perm::Text] 하나뿐입니다.
#[derive(Clone, Copy)]
enum Perm {
    Text,
    Ro,
    Rw,
    Device,
}

impl Perm {
    fn attrs(self) -> u64 {
        match self {
            Perm::Text => DESC_PAGE | ATTR_AF | SH_INNER | AP_RO | IDX_NORMAL | UXN,
            Perm::Ro => DESC_PAGE | ATTR_AF | SH_INNER | AP_RO | IDX_NORMAL | UXN | PXN,
            Perm::Rw => DESC_PAGE | ATTR_AF | SH_INNER | AP_RW | IDX_NORMAL | UXN | PXN,
            Perm::Device => DESC_PAGE | ATTR_AF | SH_OUTER | AP_RW | IDX_DEVICE | UXN | PXN,
        }
    }
}

/// MMU 활성화가 거부되거나 실패한 이유를 나타내는 열거형입니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmuError {
    AlreadyEnabled,
    NotEnabled,
    GranuleUnsupported,
    OutOfTables,
    OutOfFrames,
    Misaligned,
    Overlap,
    NullPage,
    BadTable,
}

/// 매핑 대상 커널 레이아웃을 담는 구조체입니다.
///
/// 모든 경계는 링커 스크립트가 그래뉼 정렬을 보장하는 물리 주소입니다.
/// `rw`의 두 구간 사이(가드 페이지)는 의도적으로 매핑되지 않습니다.
/// `devices`는 그래뉼 정렬된 MMIO 범위이며 빈 범위(start == end)는 무시됩니다.
pub struct KernelLayout {
    pub text: Range<u64>,
    pub rodata: Range<u64>,
    pub rw: [Range<u64>; 2],
    pub dtb: Range<u64>,
    pub devices: [Range<u64>; 3],
}

/// MMU가 켜진 상태를 증명하는 typestate 토큰 구조체입니다.
///
/// 진입 페이즈 2(예외 벡터, GIC)가 이 토큰을 입력으로 받게 됩니다.
pub struct Mmu {
    _sealed: (),
}

#[cfg_attr(feature = "plat-virt", repr(C, align(4096)))]
#[cfg_attr(feature = "plat-apple", repr(C, align(16384)))]
struct PageTable([u64; ENTRIES]);

const EMPTY_TABLE: PageTable = PageTable([0; ENTRIES]);

struct Pool {
    tables: [PageTable; POOL_LEN],
    used: usize,
    enabled: bool,
    root1: usize,
}

struct SyncCell(UnsafeCell<Pool>);

/// # Safety
/// 단일 부트 코어가 MMU 이전 단계에서 enable_paging을 통해서만 접근하고,
/// enabled 플래그가 재진입을 거부하므로 동시 접근이 없습니다.
unsafe impl Sync for SyncCell {}

static POOL: SyncCell = SyncCell(UnsafeCell::new(Pool {
    tables: [EMPTY_TABLE; POOL_LEN],
    used: 0,
    enabled: false,
    root1: 0,
}));

pub(crate) fn index(va: u64, level: u32) -> usize {
    let shift = PAGE_SHIFT + BITS_PER_LEVEL * (3 - level);
    let bits = if level == 0 { 48 - shift } else { BITS_PER_LEVEL };
    ((va >> shift) & ((1u64 << bits) - 1)) as usize
}

impl Pool {
    fn alloc(&mut self) -> Result<usize, MmuError> {
        if self.used == POOL_LEN {
            return Err(MmuError::OutOfTables);
        }
        let i = self.used;
        self.used += 1;
        Ok(i)
    }

    fn pa(&self, i: usize) -> u64 {
        // 점프 전에는 심볼 주소가 곧 PA고, 점프 후에는 VA라 오프셋을 벗겨냄
        core::ptr::from_ref(&self.tables[i]) as u64 & !KERNEL_VA_OFFSET
    }

    fn index_of(&self, pa: u64) -> Result<usize, MmuError> {
        let base = self.pa(0);
        if pa < base {
            return Err(MmuError::BadTable);
        }
        let off = pa - base;
        let i = (off / GRANULE as u64) as usize;
        if off % GRANULE as u64 != 0 || i >= self.used {
            return Err(MmuError::BadTable);
        }
        Ok(i)
    }

    fn map_page(&mut self, root: usize, va: u64, pa: u64, attrs: u64) -> Result<(), MmuError> {
        let mut t = root;
        for level in 0..3 {
            let idx = index(va, level);
            let entry = self.tables[t].0[idx];
            t = if entry == 0 {
                let child = self.alloc()?;
                self.tables[t].0[idx] = self.pa(child) | DESC_TABLE;
                child
            } else if entry & 0b11 == DESC_TABLE {
                self.index_of(entry & ADDR_MASK)?
            } else {
                return Err(MmuError::BadTable);
            };
        }
        let idx = index(va, 3);
        if self.tables[t].0[idx] != 0 {
            return Err(MmuError::Overlap);
        }
        self.tables[t].0[idx] = pa | attrs;
        Ok(())
    }

    fn map_range(&mut self, root: usize, va: u64, pa: u64, len: u64, perm: Perm) -> Result<(), MmuError> {
        let g = GRANULE as u64;
        if va % g != 0 || pa % g != 0 || len % g != 0 {
            return Err(MmuError::Misaligned);
        }
        let attrs = perm.attrs();
        let mut off = 0;
        while off < len {
            self.map_page(root, va + off, pa + off, attrs)?;
            off += g;
        }
        Ok(())
    }
}

/// ID_AA64MMFR0_EL1로 그래뉼 지원을 확인하고 IPS 값을 계산하는 함수입니다.
///
/// # Errors
/// 현재 코어가 선택된 그래뉼을 지원하지 않으면 `GranuleUnsupported`
fn check_features() -> Result<u64, MmuError> {
    let mmfr0: u64;
    // SAFETY: ID 레지스터 읽기는 부작용 없음
    unsafe { asm!("mrs {}, id_aa64mmfr0_el1", out(reg) mmfr0, options(nomem, nostack)) };

    #[cfg(feature = "plat-virt")]
    let granule_ok = matches!((mmfr0 >> 28) & 0xF, 0 | 1); // TGran4
    #[cfg(feature = "plat-apple")]
    let granule_ok = matches!((mmfr0 >> 20) & 0xF, 1 | 2); // TGran16

    if !granule_ok {
        return Err(MmuError::GranuleUnsupported);
    }
    // IPS는 구현된 PARange를 넘지 않게, 최대 48비트(0b101)로 제한
    Ok((mmfr0 & 0xF).min(5))
}

fn tcr(ips: u64) -> u64 {
    // TG0과 TG1은 같은 그래뉼이라도 인코딩이 다름에 주의
    #[cfg(feature = "plat-virt")]
    const TG: (u64, u64) = (0b00, 0b10);
    #[cfg(feature = "plat-apple")]
    const TG: (u64, u64) = (0b10, 0b01);

    16                                                      // T0SZ: 48비트 VA
        | 1 << 8 | 1 << 10 | 0b11 << 12 | TG.0 << 14        // IRGN0/ORGN0 WBWA, SH0 inner
        | 16 << 16                                          // T1SZ: 48비트 VA
        | 1 << 24 | 1 << 26 | 0b11 << 28 | TG.1 << 30       // IRGN1/ORGN1 WBWA, SH1 inner
        | ips << 32
}

/// 초기 페이지 테이블을 구성하고 MMU를 활성화하는 함수입니다.
///
/// # Arguments
/// `layout` - 링커 심볼과 부트 정보로 채운 매핑 대상 레이아웃
///
/// # Errors
/// 그래뉼 미지원, 풀 고갈, 정렬 위반, 중복 매핑, 재호출 시 `MmuError`.
/// 이 함수가 실패하면 주소 공간 상태를 신뢰할 수 없기 때문에 호출자는
/// 부팅을 중단해야 합니다.
pub fn enable_paging(layout: &KernelLayout) -> Result<Mmu, MmuError> {
    let ips = check_features()?;

    // SAFETY: 단일 부트 코어의 MMU 이전 단계에서만 도달하고 enabled 플래그가
    //         재진입을 거부하기 때문에 이 가변 참조는 유일함
    let pool = unsafe { &mut *POOL.0.get() };
    if pool.enabled {
        return Err(MmuError::AlreadyEnabled);
    }

    let root0 = pool.alloc()?;
    let root1 = pool.alloc()?;

    let g = GRANULE as u64;
    let dtb_base = layout.dtb.start & !(g - 1);
    let dtb_end = layout.dtb.end.div_ceil(g) * g;

    let segments: [(u64, u64, Perm); 5] = [
        (layout.text.start, layout.text.end, Perm::Text),
        (layout.rodata.start, layout.rodata.end, Perm::Ro),
        (layout.rw[0].start, layout.rw[0].end, Perm::Rw),
        (layout.rw[1].start, layout.rw[1].end, Perm::Rw),
        (dtb_base, dtb_end, Perm::Ro),
    ];
    for (start, end, perm) in segments {
        if end < start {
            return Err(MmuError::Misaligned);
        }
        let len = end - start;
        if len == 0 {
            continue;
        }
        pool.map_range(root0, start, start, len, perm)?;
        pool.map_range(root1, start + KERNEL_VA_OFFSET, start, len, perm)?;
    }
    for dev in &layout.devices {
        if dev.end <= dev.start {
            continue;
        }
        let len = dev.end - dev.start;
        pool.map_range(root0, dev.start, dev.start, len, Perm::Device)?;
        pool.map_range(root1, dev.start + KERNEL_VA_OFFSET, dev.start, len, Perm::Device)?;
    }

    let ttbr0 = pool.pa(root0);
    let ttbr1 = pool.pa(root1);
    pool.enabled = true;
    pool.root1 = root1;

    // SAFETY: 현재 PC(text)와 SP(부트 스택)를 포함한 필수 매핑이 identity로
    //         구성돼 있고, 배리어 순서는 ARMv8 MMU 활성화 절차를 따름
    unsafe { switch_on(ttbr0, ttbr1, tcr(ips)) };
    Ok(Mmu { _sealed: () })
}

/// MMU 활성화 이후 TTBR1에 커널 RW 매핑(부트 프레임 윈도우)을 추가하는 함수입니다.
///
/// 진입 페이즈 3의 프레임 할당자가 쓸 물리 구간을 TTBR1 별칭
/// (`PA + KERNEL_VA_OFFSET`)으로 열어 줍니다. 새 매핑 추가만 하기 때문에
/// 기존 변환에 대한 break-before-make는 필요 없습니다.
///
/// # Arguments
/// `window` - 그래뉼 정렬된 물리 구간(커널 이미지/DTB와 겹치지 않아야 함)
///
/// # Errors
/// MMU 비활성 상태, 정렬 위반, 풀 고갈, 기존 매핑과의 겹침 시 `MmuError`
pub fn map_kernel_window(window: Range<u64>) -> Result<(), MmuError> {
    let g = GRANULE as u64;
    if window.end <= window.start || window.start % g != 0 || window.end % g != 0 {
        return Err(MmuError::Misaligned);
    }

    // SAFETY: 단일 부트 코어의 초기화 시퀀스에서만 호출됨(순서는 kernel_main이
    //         강제), 예외 핸들러는 풀을 건드리지 않으므로 이 가변 참조는 유일함
    let pool = unsafe { &mut *POOL.0.get() };
    if !pool.enabled {
        return Err(MmuError::NotEnabled);
    }

    let root1 = pool.root1;
    let len = window.end - window.start;
    pool.map_range(root1, window.start + KERNEL_VA_OFFSET, window.start, len, Perm::Rw)?;

    // SAFETY: 새로 유효해진 테이블 엔트리를 워커가 보기 전에 쓰기를 완료시킴
    unsafe { asm!("dsb ishst", "isb", options(nostack)) };
    Ok(())
}

/// 변환 레지스터를 설정하고 SCTLR_EL1로 MMU를 켜는 함수입니다.
///
/// # Safety
/// 호출 시점의 PC와 SP가 identity 매핑에 포함돼 있어야 하며, 부트 경로에서
/// 단 한 번만 호출해야 합니다. 실 하드웨어에서는 진입 전 캐시가 무효화돼
/// 있어야 합니다. (m1n1은 페이로드 진입 전에 이를 보장)
unsafe fn switch_on(ttbr0: u64, ttbr1: u64, tcr: u64) {
    let mut sctlr: u64;
    // SAFETY: 시스템 레지스터 읽기, 부작용 없음
    unsafe { asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack)) };
    sctlr |= SCTLR_M | SCTLR_C | SCTLR_I | SCTLR_WXN;

    // SAFETY: 함수 계약(Docstring 내 Safety) 전제 하에 아키텍처 절차대로 MMU를 킴
    unsafe {
        asm!(
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {t0}",
            "msr ttbr1_el1, {t1}",
            "dsb ishst",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            "msr sctlr_el1, {sctlr}",
            "isb",
            mair = in(reg) MAIR,
            tcr = in(reg) tcr,
            t0 = in(reg) ttbr0,
            t1 = in(reg) ttbr1,
            sctlr = in(reg) sctlr,
            options(nostack),
        );
    }
}

/// AT S1E1R로 해당 VA가 EL1에서 읽기 가능한지 확인하는 함수입니다.
///
/// # Arguments
/// `va` - 검사할 가상 주소
pub fn can_read(va: u64) -> bool {
    let par: u64;
    // SAFETY: AT는 변환 시도만 하고 폴트를 일으키지 않으며 결과는 PAR_EL1에 남음
    unsafe {
        asm!(
            "at s1e1r, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack),
        );
    }
    par & 1 == 0
}

/// AT S1E1W로 해당 VA가 EL1에서 쓰기 가능한지 확인하는 함수입니다.
///
/// # Arguments
/// `va` - 검사할 가상 주소
pub fn can_write(va: u64) -> bool {
    let par: u64;
    // SAFETY: AT는 변환 시도만 하고 폴트를 일으키지 않으며 결과는 PAR_EL1에 남음
    unsafe {
        asm!(
            "at s1e1w, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack),
        );
    }
    par & 1 == 0
}
