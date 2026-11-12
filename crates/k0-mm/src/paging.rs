//! 진입 페이즈 1 초기 페이지 테이블 구성과 MMU 활성화 모듈입니다.
//!
//! # Features
//! 커널 이미지(W^X 분리)만 담는 최소 테이블을 .bss의 정적 풀에서 구성합니다.
//! 블록 매핑 없이 최하위 레벨 페이지로만 매핑해 코드 경로를 하나로 유지합니다.
//! 재분류된 오브젝트의 별칭에 드는 테이블은 정적 풀이 아니라 부팅 때 RAM에서
//! 떼어 둔 예약 구간에서 나옵니다([reserve_tables]). 예약은 untyped로 넘기는
//! 구간마다 그 구간의 모든 그래뉼을 매핑하는 데 들 수 있는 최대 수만큼 잡으므로
//! ([alias_tables_for]), 부팅 뒤에는 어떤 태스크도 untyped 메모리를 쥐는
//! 것만으로 별칭 테이블을 바닥낼 수 없습니다.
//! 이미지는 적재된 물리 주소와 무관하게 TTBR1의 고정 주소 `KERNEL_IMAGE_VA`에
//! 매핑되고, TTBR0에는 전환 순간에만 쓰는 identity 매핑이 놓입니다(진입 페이즈
//! 3에서 사용자 공간용으로 교체). TTBR1의 나머지는 세 구역으로 나뉩니다.
//! 선형 별칭(PA + `KERNEL_VA_OFFSET`)은 커널이 건드리는 RAM(부트 윈도우,
//! 재분류된 오브젝트, DTB)에만 쓰고, 디바이스는 이미지 위쪽의 디바이스 창에
//! 페이지 단위로 모아 매핑합니다. `SCTLR_EL1.WXN`을 켜서 쓰기 가능한 페이지는
//! 하드웨어 수준에서 실행이 거부됩니다. 부트 스택 아래 가드 페이지는 어느
//! 반쪽에도 매핑하지 않아 스택 오버플로가 즉시 폴트가 됩니다.
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
#[cfg(feature = "granule-4k")]
pub const GRANULE: usize = 4096;
#[cfg(feature = "granule-16k")]
pub const GRANULE: usize = 16384;

/// TTBR1 선형 별칭의 VA 오프셋과 이미지의 고정 VA (정의는 k0-abi)
pub use k0_abi::{KERNEL_IMAGE_VA, KERNEL_VA_OFFSET, PA_CEILING};

/// 디바이스 창의 시작 VA입니다. 이미지와 같은 상위 테이블을 나눠 쓰도록
/// 이미지 1GiB 위에 둡니다. 0번 칸은 프로브 페이지입니다.
const DEVICE_VA: u64 = KERNEL_IMAGE_VA + (1 << 30);

/// 디바이스 창의 칸 수(프로브 칸 포함). 커널이 매핑하는 디바이스 페이지는
/// 콘솔 1, GIC 최대 4라서 여유를 둔 값입니다.
const DEVICE_SLOTS: usize = 16;

/// 테이블 예약 구간의 최대 수. untyped로 넘기는 구간마다 하나씩 쓰므로 메모리
/// 구간과 예약 구간이 만들 수 있는 틈의 수(8 + 36)보다 크게 잡음
pub const MAX_TABLE_RESERVES: usize = 64;

/// 물리 주소를 커널이 그 메모리에 닿는 주소로 바꾸는 오프셋을 주는 함수입니다.
///
/// 커널에서는 TTBR1 별칭입니다. 호스트 단위 테스트에서는 테스트가 스레드마다
/// 정하는 값이라, 직접 할당한 메모리를 낮은 가짜 물리 주소에 놓인 예약
/// 구간처럼 다루며 워커를 그대로 돌릴 수 있습니다.
#[cfg(not(test))]
fn alias_offset() -> u64 {
    KERNEL_VA_OFFSET
}
#[cfg(test)]
fn alias_offset() -> u64 {
    TEST_ALIAS_OFFSET.with(core::cell::Cell::get)
}
#[cfg(test)]
std::thread_local! {
    static TEST_ALIAS_OFFSET: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

const PAGE_SHIFT: u32 = GRANULE.trailing_zeros();
const BITS_PER_LEVEL: u32 = PAGE_SHIFT - 3;
const ENTRIES: usize = GRANULE / 8;
const POOL_LEN: usize = 40;
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
    AboveCeiling,
    OutOfTables,
    OutOfFrames,
    Misaligned,
    Overlap,
    NullPage,
    BadTable,
    MissingTable,
}

/// 매핑 대상 커널 레이아웃을 담는 구조체입니다.
///
/// 모든 경계는 이미지가 실제로 적재된 물리 주소이며 그래뉼 정렬이어야 합니다
/// (링커 스크립트가 이미지 안의 간격을, 부트로더가 적재 주소를 맞춤).
/// `rw`의 두 구간 사이(가드 페이지)는 의도적으로 매핑되지 않습니다.
pub struct KernelLayout {
    pub text: Range<u64>,
    pub rodata: Range<u64>,
    pub rw: [Range<u64>; 2],
    /// MMU가 꺼진 동안 커널이 쓴 구간(.bss의 시작부터 이미지 끝까지)
    pub cold: Range<u64>,
}

/// MMU가 켜진 상태를 증명하는 typestate 토큰 구조체입니다.
///
/// 진입 페이즈 2(예외 벡터, GIC)가 이 토큰을 입력으로 받게 됩니다.
pub struct Mmu {
    _sealed: (),
}

#[cfg_attr(feature = "granule-4k", repr(C, align(4096)))]
#[cfg_attr(feature = "granule-16k", repr(C, align(16384)))]
struct PageTable([u64; ENTRIES]);

const EMPTY_TABLE: PageTable = PageTable([0; ENTRIES]);

struct Pool {
    tables: [PageTable; POOL_LEN],
    used: usize,
    enabled: bool,
    root1: usize,
    /// 어떤 사용자 매핑도 없는 빈 TTBR0 루트(주소 공간이 사라질 때의 대피처)
    null_root: usize,
    /// 0번 테이블의 물리 주소. MMU 이전에 한 번 잡아 두고 이후의 테이블
    /// 주소 계산은 전부 이 값에서 출발함(가상 주소 산술을 쓰지 않음)
    base_pa: u64,
    /// 커널 이미지가 차지하는 물리 구간 [시작, 끝)
    image: (u64, u64),
    /// 이미지의 VA에서 PA를 뺀 값
    image_delta: u64,
    /// 디바이스 창에서 쓴 칸 수(0번 프로브 칸 제외)와 각 칸의 물리 페이지
    dev_used: usize,
    dev_pages: [u64; DEVICE_SLOTS],
    /// 프로브 칸의 리프 위치(테이블 번호, 엔트리 번호)와 사용 여부
    probe_leaf: (usize, usize),
    probe_live: bool,
    /// 부팅 때 RAM에서 떼어 둔 테이블 예약 구간들(시작 PA, 테이블 수). 정적
    /// 배열 뒤의 테이블 번호가 이 구간들을 순서대로 가리킴
    ext: [(u64, usize); MAX_TABLE_RESERVES],
    ext_regions: usize,
    /// 예약 구간 전체의 테이블 수와 그중 내준 수
    ext_total: usize,
    ext_used: usize,
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
    null_root: 0,
    base_pa: 0,
    image: (0, 0),
    image_delta: 0,
    dev_used: 0,
    dev_pages: [0; DEVICE_SLOTS],
    probe_leaf: (0, 0),
    probe_live: false,
    ext: [(0, 0); MAX_TABLE_RESERVES],
    ext_regions: 0,
    ext_total: 0,
    ext_used: 0,
}));

pub(crate) fn index(va: u64, level: u32) -> usize {
    let shift = PAGE_SHIFT + BITS_PER_LEVEL * (3 - level);
    let bits = if level == 0 { 48 - shift } else { BITS_PER_LEVEL };
    ((va >> shift) & ((1u64 << bits) - 1)) as usize
}

impl Pool {
    /// 테이블 하나를 내주는 함수입니다. 번호가 `POOL_LEN` 미만이면 정적
    /// 배열의 테이블이고, 그 이상이면 예약 구간의 테이블입니다.
    ///
    /// 예약 구간이 등록된 뒤에는 거기서 먼저 내주므로 부팅 뒤의 별칭 매핑은
    /// 정적 배열을 쓰지 않습니다. 예약 구간의 테이블은 부팅 때 통째로 지우지
    /// 않고 내줄 때 지웁니다.
    fn alloc(&mut self) -> Result<usize, MmuError> {
        if self.ext_used < self.ext_total {
            let i = POOL_LEN + self.ext_used;
            self.ext_used += 1;
            // SAFETY: 예약 구간은 등록할 때 별칭으로 매핑했고 커널만 쓰며,
            //         아직 어느 테이블에서도 가리키지 않는 테이블임
            unsafe {
                core::ptr::write_bytes(self.pa(i).wrapping_add(alias_offset()) as *mut u8, 0, GRANULE);
            }
            return Ok(i);
        }
        if self.used == POOL_LEN {
            return Err(MmuError::OutOfTables);
        }
        let i = self.used;
        self.used += 1;
        Ok(i)
    }

    /// 테이블 번호를 물리 주소로 바꾸는 함수입니다.
    ///
    /// 내준 적 없는 번호를 넘기는 것은 이 모듈의 버그이므로 정지합니다.
    fn pa(&self, i: usize) -> u64 {
        if i < POOL_LEN {
            return self.base_pa + (i * GRANULE) as u64;
        }
        let mut ord = i - POOL_LEN;
        for &(base, count) in &self.ext[..self.ext_regions] {
            if ord < count {
                return base + (ord * GRANULE) as u64;
            }
            ord -= count;
        }
        panic!("kernel table index out of range");
    }

    /// 테이블의 엔트리 배열을 주는 함수입니다.
    fn tbl(&self, i: usize) -> &[u64; ENTRIES] {
        if i < POOL_LEN {
            return &self.tables[i].0;
        }
        // SAFETY: pa()는 등록된 예약 구간 안의 주소만 주고, 그 구간은 별칭으로
        //         매핑돼 있으며 커널만 접근함
        unsafe { &*(self.pa(i).wrapping_add(alias_offset()) as *const [u64; ENTRIES]) }
    }

    /// 테이블의 엔트리 배열을 고칠 수 있게 주는 함수입니다.
    fn tbl_mut(&mut self, i: usize) -> &mut [u64; ENTRIES] {
        if i < POOL_LEN {
            return &mut self.tables[i].0;
        }
        // SAFETY: tbl()과 같고, 풀에 대한 가변 참조가 유일하므로 이 참조도 유일함
        unsafe { &mut *(self.pa(i).wrapping_add(alias_offset()) as *mut [u64; ENTRIES]) }
    }

    /// `va`를 변환하는 리프 엔트리의 위치(테이블 번호, 엔트리 번호)를 찾는
    /// 함수입니다. 중간 테이블이 없으면 `MissingTable`입니다.
    fn leaf_of(&self, root: usize, va: u64) -> Result<(usize, usize), MmuError> {
        let mut t = root;
        for level in 0..3 {
            let entry = self.tbl(t)[index(va, level)];
            if entry & 0b11 != DESC_TABLE {
                return Err(MmuError::MissingTable);
            }
            t = self.index_of(entry & ADDR_MASK)?;
        }
        Ok((t, index(va, 3)))
    }

    fn overlaps_image(&self, start: u64, end: u64) -> bool {
        start < self.image.1 && self.image.0 < end
    }

    /// 테이블 디스크립터가 가리키는 물리 주소를 테이블 번호로 바꾸는 함수입니다.
    ///
    /// 정적 배열이나 예약 구간 안에 있고, 그래뉼에 정렬돼 있고, 이미 내준
    /// 테이블일 때만 받아들입니다. 그 밖의 주소는 풀이 만든 링크가 아닙니다.
    fn index_of(&self, pa: u64) -> Result<usize, MmuError> {
        let g = GRANULE as u64;
        if pa % g != 0 {
            return Err(MmuError::BadTable);
        }
        let base = self.base_pa;
        if pa >= base && pa - base < (POOL_LEN as u64) * g {
            let i = ((pa - base) / g) as usize;
            return if i < self.used { Ok(i) } else { Err(MmuError::BadTable) };
        }
        let mut ord = 0usize;
        for &(start, count) in &self.ext[..self.ext_regions] {
            if pa >= start && pa - start < (count as u64) * g {
                let o = ord + ((pa - start) / g) as usize;
                return if o < self.ext_used { Ok(POOL_LEN + o) } else { Err(MmuError::BadTable) };
            }
            ord += count;
        }
        Err(MmuError::BadTable)
    }

    /// 페이지 하나를 매핑하는 함수입니다. 실패하면 이 호출이 만든 중간
    /// 테이블과 링크를 전부 되돌려 풀 상태를 호출 전과 같게 유지합니다.
    ///
    /// 되돌리기가 없으면 실패한 매핑(재분류 경로의 별칭 매핑 등)이 테이블을
    /// 조금씩 갉아먹어, 부팅 때 구간 크기에 맞춰 잡은 예약이 모자라게 됩니다.
    ///
    /// `same_ok`이면 리프에 이미 완전히 같은 매핑(같은 PA, 같은 속성)이 있는
    /// 경우를 성공으로 칩니다. 회수된 untyped 메모리를 다시 재분류할 때 같은
    /// 물리 프레임의 별칭이 이미 있기 때문입니다. 다른 매핑이 있으면 여전히
    /// `Overlap`입니다.
    fn map_page(
        &mut self,
        root: usize,
        va: u64,
        pa: u64,
        attrs: u64,
        same_ok: bool,
    ) -> Result<(), MmuError> {
        let (used_before, ext_before) = (self.used, self.ext_used);
        // 이번 호출이 새로 쓴 링크(부모 테이블, 인덱스), 레벨당 최대 하나
        let mut links: [(usize, usize); 3] = [(0, 0); 3];
        let mut link_count = 0;

        let r = (|| {
            let mut t = root;
            for level in 0..3 {
                let idx = index(va, level);
                let entry = self.tbl(t)[idx];
                t = if entry == 0 {
                    let child = self.alloc()?;
                    self.tbl_mut(t)[idx] = self.pa(child) | DESC_TABLE;
                    links[link_count] = (t, idx);
                    link_count += 1;
                    child
                } else if entry & 0b11 == DESC_TABLE {
                    self.index_of(entry & ADDR_MASK)?
                } else {
                    return Err(MmuError::BadTable);
                };
            }
            let idx = index(va, 3);
            let old = self.tbl(t)[idx];
            if old == pa | attrs && same_ok {
                return Ok(());
            }
            if old != 0 {
                return Err(MmuError::Overlap);
            }
            self.tbl_mut(t)[idx] = pa | attrs;
            Ok(())
        })();

        if r.is_err() {
            // 새 테이블은 링크 외에 기록된 것이 없으므로 링크만 지우면 다시
            // 소거 상태의 풀 항목이 됨(예약 구간의 테이블은 다음에 내줄 때
            // 다시 소거함). 이번 호출이 받은 테이블은 가장 나중에 내준
            // 것들이라 두 계수를 되돌리는 것으로 충분함
            for &(parent, idx) in &links[..link_count] {
                self.tbl_mut(parent)[idx] = 0;
            }
            self.used = used_before;
            self.ext_used = ext_before;
        }
        r
    }

    fn map_range(
        &mut self,
        root: usize,
        va: u64,
        pa: u64,
        len: u64,
        perm: Perm,
        same_ok: bool,
    ) -> Result<(), MmuError> {
        let g = GRANULE as u64;
        if va % g != 0 || pa % g != 0 || len % g != 0 {
            return Err(MmuError::Misaligned);
        }
        let attrs = perm.attrs();
        let mut off = 0;
        while off < len {
            self.map_page(root, va + off, pa + off, attrs, same_ok)?;
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

    #[cfg(feature = "granule-4k")]
    let granule_ok = matches!((mmfr0 >> 28) & 0xF, 0 | 1); // TGran4
    #[cfg(feature = "granule-16k")]
    let granule_ok = matches!((mmfr0 >> 20) & 0xF, 1 | 2); // TGran16

    if !granule_ok {
        return Err(MmuError::GranuleUnsupported);
    }
    // IPS는 구현된 PARange를 넘지 않게, 최대 48비트(0b101)로 제한
    Ok((mmfr0 & 0xF).min(5))
}

/// FEAT_E0PD 지원 여부를 ID_AA64MMFR2_EL1로 확인하는 함수입니다.
///
/// v8.0 코어에서는 이 ID 레지스터가 0으로 읽히므로 미지원으로 판정됩니다.
/// 베이스라인 어셈블러가 이름을 거부할 수 있어 generic 인코딩을 씁니다.
fn has_e0pd() -> bool {
    let mmfr2: u64;
    // SAFETY: ID 레지스터 읽기는 부작용 없음 (S3_0_C0_C7_2 = ID_AA64MMFR2_EL1)
    unsafe { asm!("mrs {}, S3_0_C0_C7_2", out(reg) mmfr2, options(nomem, nostack)) };
    (mmfr2 >> 60) & 0xF != 0
}

/// TCR_EL1.E0PD1: EL0의 상위 절반(TTBR1) 접근을 변환 없이 즉시 폴트로 만듦
const TCR_E0PD1: u64 = 1 << 56;

fn tcr(ips: u64) -> u64 {
    // TG0과 TG1은 같은 그래뉼이라도 인코딩이 다름에 주의
    #[cfg(feature = "granule-4k")]
    const TG: (u64, u64) = (0b00, 0b10);
    #[cfg(feature = "granule-16k")]
    const TG: (u64, u64) = (0b10, 0b01);

    16                                                      // T0SZ: 48비트 VA
        | 1 << 8 | 1 << 10 | 0b11 << 12 | TG.0 << 14        // IRGN0/ORGN0 WBWA, SH0 inner
        | 16 << 16                                          // T1SZ: 48비트 VA
        | 1 << 24 | 1 << 26 | 0b11 << 28 | TG.1 << 30       // IRGN1/ORGN1 WBWA, SH1 inner
        | ips << 32
}

/// 초기 페이지 테이블을 구성하고 MMU를 활성화하는 함수입니다.
///
/// 이미지를 TTBR0에는 identity로, TTBR1에는 `KERNEL_IMAGE_VA`부터 매핑합니다.
/// MMU가 꺼진 동안 쓴 메모리(.bss, 부트 스택, 이 풀의 테이블)는 캐시를 거치지
/// 않았으므로, 캐시를 켜기 전에 그 구간의 데이터 캐시 라인을 무효화해 펌웨어가
/// 남긴 낡은 라인이 방금 쓴 내용을 가리지 못하게 합니다.
///
/// # Arguments
/// `layout` - 이미지가 적재된 물리 주소로 채운 매핑 대상 레이아웃
///
/// # Errors
/// 그래뉼 미지원, 풀 고갈, 정렬 위반, 물리 주소 상한 초과, 재호출 시
/// `MmuError`. 이 함수가 실패하면 주소 공간 상태를 신뢰할 수 없기 때문에
/// 호출자는 부팅을 중단해야 합니다.
pub fn enable_paging(layout: &KernelLayout) -> Result<Mmu, MmuError> {
    let ips = check_features()?;

    // SAFETY: 단일 부트 코어의 MMU 이전 단계에서만 도달하고 enabled 플래그가
    //         재진입을 거부하기 때문에 이 가변 참조는 유일함
    let pool = unsafe { &mut *POOL.0.get() };
    if pool.enabled {
        return Err(MmuError::AlreadyEnabled);
    }

    let g = GRANULE as u64;
    let load = layout.text.start;
    let end = layout.rw[1].end;
    if load % g != 0 || end % g != 0 || end <= load {
        return Err(MmuError::Misaligned);
    }
    if end > PA_CEILING {
        return Err(MmuError::AboveCeiling);
    }
    // MMU가 꺼져 있어 정적의 주소가 곧 물리 주소임. 이후에는 다시 계산하지 않음
    pool.base_pa = core::ptr::from_ref(&pool.tables[0]) as u64;
    pool.image = (load, end);
    pool.image_delta = KERNEL_IMAGE_VA.wrapping_sub(load);

    let root0 = pool.alloc()?;
    let root1 = pool.alloc()?;
    // 풀 항목은 소거 상태로 시작하므로 그대로 빈 루트임
    let null_root = pool.alloc()?;

    let segments: [(u64, u64, Perm); 4] = [
        (layout.text.start, layout.text.end, Perm::Text),
        (layout.rodata.start, layout.rodata.end, Perm::Ro),
        (layout.rw[0].start, layout.rw[0].end, Perm::Rw),
        (layout.rw[1].start, layout.rw[1].end, Perm::Rw),
    ];
    for (start, end, perm) in segments {
        if end < start {
            return Err(MmuError::Misaligned);
        }
        let len = end - start;
        if len == 0 {
            continue;
        }
        pool.map_range(root0, start, start, len, perm, false)?;
        pool.map_range(root1, start.wrapping_add(pool.image_delta), start, len, perm, false)?;
    }

    let ttbr0 = pool.pa(root0);
    let ttbr1 = pool.pa(root1);
    pool.enabled = true;
    pool.root1 = root1;
    pool.null_root = null_root;

    // E0PD1은 EL0가 커널 절반의 매핑 유무를 변환 타이밍으로 구별하지 못하게 함
    let tcr = if has_e0pd() { tcr(ips) | TCR_E0PD1 } else { tcr(ips) };

    // SAFETY: 구간은 이 이미지의 쓰기 가능한 부분이고, 현재 PC(text)와 SP(부트
    //         스택)를 포함한 필수 매핑이 identity로 구성돼 있으며, 배리어 순서는
    //         ARMv8 MMU 활성화 절차를 따름
    unsafe {
        invalidate_dcache(layout.cold.start, layout.cold.end);
        switch_on(ttbr0, ttbr1, tcr);
    }
    Ok(Mmu { _sealed: () })
}

/// 물리 구간의 데이터 캐시 라인을 무효화하는 함수입니다. (MMU 이전 전용)
///
/// # Safety
/// MMU와 데이터 캐시가 꺼진 상태에서, 메모리의 내용이 곧 진실인 구간에만
/// 써야 합니다. 캐시에만 있는 내용은 버려집니다.
unsafe fn invalidate_dcache(start: u64, end: u64) {
    let ctr: u64;
    // SAFETY: CTR_EL0 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let line = 4u64 << ((ctr >> 16) & 0xF);
    let mut addr = start & !(line - 1);
    while addr < end {
        // SAFETY: 함수 계약대로 MMU 이전의 물리 주소에 대한 캐시 유지보수 명령
        unsafe { asm!("dc ivac, {}", in(reg) addr, options(nostack)) };
        addr += line;
    }
    // SAFETY: 무효화가 끝난 뒤에 캐시를 켜도록 완료를 기다림
    unsafe { asm!("dsb sy", "isb", options(nostack)) };
}

/// 이미지 안의 가상 주소를 물리 주소로 바꾸는 함수입니다.
///
/// higher-half 점프 이후의 링커 심볼 주소에 씁니다. 점프 전에는 심볼 주소가
/// 그대로 물리 주소이므로 부르면 안 됩니다.
pub fn image_pa(va: u64) -> u64 {
    // SAFETY: 읽기만 하고 enable_paging 이후에는 값이 바뀌지 않음
    let pool = unsafe { &*POOL.0.get() };
    va.wrapping_sub(pool.image_delta)
}

/// 커널 이미지가 차지하는 물리 구간을 주는 함수입니다.
pub fn image_range() -> Range<u64> {
    // SAFETY: 읽기만 하고 enable_paging 이후에는 값이 바뀌지 않음
    let pool = unsafe { &*POOL.0.get() };
    pool.image.0..pool.image.1
}

/// MMU 활성화 이후 TTBR1에 커널 RW 매핑을 추가하는 함수입니다.
///
/// 진입 페이즈 3의 프레임 할당자가 쓸 부트 윈도우를 TTBR1 별칭
/// (`PA + KERNEL_VA_OFFSET`)으로 열어 줍니다. 새 매핑 추가만 하기 때문에
/// 기존 변환에 대한 break-before-make는 필요 없습니다. 이미 매핑된 구간과
/// 겹치면 거부합니다.
///
/// # Arguments
/// `window` - 그래뉼 정렬된 물리 구간(커널 이미지/DTB와 겹치지 않아야 함)
///
/// # Errors
/// MMU 비활성 상태, 정렬 위반, 풀 고갈, 기존 매핑과의 겹침 시 `MmuError`
pub fn map_kernel_window(window: Range<u64>) -> Result<(), MmuError> {
    map_alias(window, Perm::Rw, false)
}

/// 재분류(retype)된 오브젝트 한 그래뉼을 TTBR1 별칭으로 여는 함수입니다.
///
/// 같은 물리 프레임이 회수 뒤에 다시 재분류되면 별칭이 이미 있으므로 완전히
/// 같은 매핑은 성공으로 칩니다. 별칭은 한 번 생기면 제거하지 않습니다.
///
/// 필요한 테이블은 부팅 때 등록한 예약 구간에서 나옵니다([reserve_tables]).
/// 커널이 untyped로 넘긴 메모리 안의 주소라면 그 예약이 모자랄 수 없습니다.
///
/// # Arguments
/// `pa` - 그래뉼 정렬된 오브젝트의 PA
///
/// # Errors
/// [map_kernel_window]와 같되 같은 매핑의 재요청은 성공
pub fn alias_object(pa: u64) -> Result<(), MmuError> {
    let end = pa.checked_add(GRANULE as u64).ok_or(MmuError::Misaligned)?;
    map_alias(pa..end, Perm::Rw, true)
}

/// DTB가 놓인 물리 구간을 TTBR1 별칭에 읽기 전용으로 여는 함수입니다.
///
/// 구간은 그래뉼 경계로 넓혀 매핑합니다. 헤더를 읽기 위해 첫 그래뉼만 먼저
/// 열고 크기를 안 뒤 전체를 다시 요청하는 쓰임을 위해, 이미 같은 매핑이 있는
/// 페이지는 성공으로 칩니다.
///
/// # Arguments
/// `range` - DTB의 물리 구간(정렬 불필요)
///
/// # Errors
/// [map_kernel_window]와 같음
pub fn map_dtb(range: Range<u64>) -> Result<(), MmuError> {
    let g = GRANULE as u64;
    let start = range.start & !(g - 1);
    let end = range.end.checked_add(g - 1).ok_or(MmuError::Misaligned)? & !(g - 1);
    map_alias(start..end, Perm::Ro, true)
}

fn map_alias(window: Range<u64>, perm: Perm, same_ok: bool) -> Result<(), MmuError> {
    // SAFETY: 부트 코어의 초기화 시퀀스 또는 이양 후 시스템 콜 컨텍스트에서만
    //         호출됨. 초기화 순서는 kernel_main이 강제하고, 시스템 콜은 단일
    //         코어에서 DAIF 마스크 상태로 실행되므로 이 가변 참조는 유일함
    let pool = unsafe { &mut *POOL.0.get() };
    map_alias_in(pool, window, perm, same_ok)
}

fn map_alias_in(pool: &mut Pool, window: Range<u64>, perm: Perm, same_ok: bool) -> Result<(), MmuError> {
    let g = GRANULE as u64;
    if window.end <= window.start || window.start % g != 0 || window.end % g != 0 {
        return Err(MmuError::Misaligned);
    }
    // 별칭은 이미지의 가상 주소 영역 아래에서 끝나야 함
    if window.end > PA_CEILING {
        return Err(MmuError::AboveCeiling);
    }
    if !pool.enabled {
        return Err(MmuError::NotEnabled);
    }
    // 커널 이미지에는 별칭을 만들지 않음. untyped 목록이 이미지를 빼놓는 것과
    // 별개로, 재분류의 소거가 커널 자신을 지우는 길을 매핑 단계에서도 막음
    if pool.overlaps_image(window.start, window.end) {
        return Err(MmuError::Overlap);
    }

    let root1 = pool.root1;
    let len = window.end - window.start;
    pool.map_range(root1, window.start + KERNEL_VA_OFFSET, window.start, len, perm, same_ok)?;

    // SAFETY: 새로 유효해진 테이블 엔트리를 워커가 보기 전에 쓰기를 완료시킴
    unsafe { asm!("dsb ishst", "isb", options(nostack)) };
    Ok(())
}

/// 구간의 모든 그래뉼을 페이지로 매핑할 때 들 수 있는 테이블 수의 상한을 주는
/// 함수입니다.
///
/// 루트 아래 세 레벨마다, 그 레벨의 테이블 하나가 덮는 구획 가운데 이 구간이
/// 걸치는 구획의 수를 셉니다. 다른 매핑과 위쪽 테이블을 나눠 쓰면 실제로는
/// 덜 들지만 더 드는 일은 없습니다.
///
/// # Arguments
/// `range` - 비어 있지 않은 물리 구간
pub fn alias_tables_for(range: &Range<u64>) -> u64 {
    let mut n = 0;
    for level in 1..=3u32 {
        // 이 레벨의 테이블 하나가 덮는 바이트 수
        let span = (GRANULE as u64) << (BITS_PER_LEVEL * (4 - level));
        n += range.end.div_ceil(span) - range.start / span;
    }
    n
}

/// RAM의 한 구간을 커널 페이지 테이블의 예약 구간으로 등록하는 함수입니다.
///
/// 구간을 별칭으로 매핑하고 그 그래뉼들을 테이블로 내줄 수 있게 합니다. 구간을
/// 매핑하는 데에도 테이블이 들므로, 최하위 테이블 하나가 덮는 크기씩 나눠
/// 매핑하면서 매핑이 끝난 부분부터 바로 등록합니다. 그래서 첫 조각만 그
/// 시점의 여유(정적 풀이나 앞서 등록한 구간)에서 치르고 나머지는 이 구간이
/// 스스로 댑니다.
///
/// 실패하면 그때까지 등록된 부분은 그대로 테이블로 쓰입니다. 호출자는 이
/// 구간이 속한 메모리를 untyped로 넘기면 안 됩니다.
///
/// # Arguments
/// `range` - 그래뉼 정렬된 물리 구간. 커널만 쓰고 untyped에서 빠져 있어야 함
///
/// # Errors
/// 정렬 위반, 상한 초과, 이미지나 기존 매핑과의 겹침, 등록 칸 소진, 첫 조각을
/// 매핑할 테이블 부족 시 `MmuError`
pub fn reserve_tables(range: Range<u64>) -> Result<(), MmuError> {
    // SAFETY: 부트 코어의 초기화 시퀀스에서만 호출되어 이 가변 참조는 유일함
    let pool = unsafe { &mut *POOL.0.get() };
    pool.reserve(range)
}

/// 테이블 예약 구간의 (내준 수, 전체) 쌍을 주는 함수입니다.
pub fn table_reserve_usage() -> (usize, usize) {
    // SAFETY: 읽기만 하고, 호출 지점(부트 시퀀스 또는 시스템 콜 컨텍스트)은
    //         단일 코어라 동시 변이가 없음
    let pool = unsafe { &*POOL.0.get() };
    (pool.ext_used, pool.ext_total)
}

impl Pool {
    fn reserve(&mut self, range: Range<u64>) -> Result<(), MmuError> {
        let g = GRANULE as u64;
        if range.end <= range.start || range.start % g != 0 || range.end % g != 0 {
            return Err(MmuError::Misaligned);
        }
        if self.ext_regions == MAX_TABLE_RESERVES {
            return Err(MmuError::OutOfTables);
        }
        let slot = self.ext_regions;
        self.ext[slot] = (range.start, 0);
        self.ext_regions += 1;

        let span = g * ENTRIES as u64;
        let mut at = range.start;
        while at < range.end {
            let end = range.end.min((at / span + 1) * span);
            if let Err(e) = map_alias_in(self, at..end, Perm::Rw, false) {
                if self.ext[slot].1 == 0 {
                    self.ext_regions -= 1;
                }
                return Err(e);
            }
            let n = ((end - at) / g) as usize;
            self.ext[slot].1 += n;
            self.ext_total += n;
            at = end;
        }
        Ok(())
    }
}

/// 읽기 전용 별칭으로 매핑된 RAM의 한 구간을 0으로 덮는 함수입니다.
///
/// DTB에 남은 시드를 지우는 데 씁니다. 구간이 걸친 페이지마다 별칭의 리프를
/// 잠깐 쓰기 가능으로 바꿨다가 되돌립니다. 같은 물리 페이지에 속성이 다른
/// 두 번째 매핑을 만들지 않으려고 기존 리프를 그 자리에서 바꾸며, 바꿀
/// 때마다 무효 엔트리와 TLB 무효화를 거칩니다(break-before-make). 덮은 뒤에는
/// 그 캐시 라인을 메모리까지 내보내고 캐시에서 비웁니다. 그래서 호출자가 그
/// 구간을 다시 읽으면 캐시에 남은 값이 아니라 메모리의 값을 보게 됩니다.
///
/// # Arguments
/// `range` - 지울 물리 구간(정렬 불필요). 전부 [map_dtb]가 매핑한 페이지
/// 안이어야 함
///
/// # Errors
/// MMU 비활성 시 `NotEnabled`, 구간의 페이지가 매핑돼 있지 않으면
/// `MissingTable`, 읽기 전용 별칭이 아닌 다른 매핑이면 `Overlap`
pub fn wipe_ro_alias(range: Range<u64>) -> Result<(), MmuError> {
    let g = GRANULE as u64;
    // SAFETY: 부트 코어의 초기화 시퀀스에서만 호출되어 이 가변 참조는 유일함
    let pool = unsafe { &mut *POOL.0.get() };
    if !pool.enabled {
        return Err(MmuError::NotEnabled);
    }
    let ctr: u64;
    // SAFETY: CTR_EL0 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let line = 4u64 << ((ctr >> 16) & 0xF);
    let root1 = pool.root1;
    let mut at = range.start;
    while at < range.end {
        let page = at & !(g - 1);
        let end = range.end.min(page + g);
        let va = page + KERNEL_VA_OFFSET;
        let (t, i) = pool.leaf_of(root1, va)?;
        let ro = page | Perm::Ro.attrs();
        if pool.tbl(t)[i] != ro {
            return Err(MmuError::Overlap);
        }
        pool.tbl_mut(t)[i] = 0;
        flush_leaf(va);
        pool.tbl_mut(t)[i] = page | Perm::Rw.attrs();
        // SAFETY: 방금 이 페이지를 쓰기 가능으로 바꿨고, 구간은 그 페이지
        //         안이며 커널만 접근함. 배리어는 새 엔트리를 워커가 보게 하고,
        //         dc cvac는 캐시 유지 보수만 함
        unsafe {
            asm!("dsb ishst", "isb", options(nostack));
            core::ptr::write_bytes((at + KERNEL_VA_OFFSET) as *mut u8, 0, (end - at) as usize);
            let mut p = (at + KERNEL_VA_OFFSET) & !(line - 1);
            while p < end + KERNEL_VA_OFFSET {
                asm!("dc civac, {}", in(reg) p, options(nostack));
                p += line;
            }
            asm!("dsb ish", options(nostack));
        }
        pool.tbl_mut(t)[i] = 0;
        flush_leaf(va);
        pool.tbl_mut(t)[i] = ro;
        // SAFETY: 되돌린 엔트리를 워커가 보기 전에 쓰기를 완료시킴
        unsafe { asm!("dsb ishst", "isb", options(nostack)) };
        at = end;
    }
    Ok(())
}

/// 무효로 만든 리프의 변환을 TLB에서 지우는 함수입니다.
///
/// # Arguments
/// `va` - 그 리프가 변환하던 가상 주소
fn flush_leaf(va: u64) {
    // SAFETY: 무효 엔트리 쓰기를 완료시킨 뒤 해당 VA의 마지막 레벨 변환만
    //         지우고, 그 완료를 기다림 (TLBI의 피연산자는 VA를 12비트 내린 값)
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi vale1is, {va}",
            "dsb ish",
            "isb",
            va = in(reg) (va >> 12) & ((1 << 44) - 1),
            options(nostack),
        );
    }
}

/// 디바이스 레지스터가 있는 물리 페이지를 디바이스 창에 매핑하는 함수입니다.
///
/// `pa`가 속한 그래뉼 페이지 하나를 Device 속성으로 매핑하고 `pa`에 해당하는
/// 가상 주소를 반환합니다. 같은 페이지를 다시 요청하면 기존 매핑을 돌려줍니다.
/// 호출자는 그 구간이 메모리 맵과 겹치지 않음을 먼저 검증해야 합니다.
///
/// # Arguments
/// `pa` - 디바이스 레지스터의 물리 주소(정렬 불필요)
///
/// # Errors
/// MMU 비활성, 물리 주소 폭 초과, 이미지와의 겹침, 창 소진, 풀 고갈 시 `MmuError`
pub fn map_device(pa: u64) -> Result<u64, MmuError> {
    let g = GRANULE as u64;
    let page = pa & !(g - 1);
    // SAFETY: 부트 코어의 초기화 시퀀스에서만 호출되어 이 가변 참조는 유일함
    let pool = unsafe { &mut *POOL.0.get() };
    pool.check_device_page(page)?;
    if let Some(i) = pool.dev_pages[..pool.dev_used].iter().position(|&p| p == page) {
        return Ok(DEVICE_VA + (i as u64 + 1) * g + (pa - page));
    }
    if pool.dev_used + 1 >= DEVICE_SLOTS {
        return Err(MmuError::OutOfTables);
    }
    let va = DEVICE_VA + (pool.dev_used as u64 + 1) * g;
    let root1 = pool.root1;
    pool.map_page(root1, va, page, Perm::Device.attrs(), false)?;
    pool.dev_pages[pool.dev_used] = page;
    pool.dev_used += 1;
    // SAFETY: 새로 유효해진 테이블 엔트리를 워커가 보기 전에 쓰기를 완료시킴
    unsafe { asm!("dsb ishst", "isb", options(nostack)) };
    Ok(va + (pa - page))
}

/// 디바이스 창에 매핑돼 있는 물리 페이지를 매핑한 순서대로 알려 주는 함수입니다.
///
/// 커널이 디바이스 레지스터를 계속 매핑해 두는 길은 [map_device] 하나뿐이므로,
/// 여기에 없는 디바이스 페이지는 커널 주소 공간에 남아 있지 않습니다(프로브
/// 칸은 들여다보는 동안만 살아 있고 [probe_clear]가 지움). 부팅 때 한 줄로
/// 출력해, 이미지가 어떤 장치에 닿을 수 있는지 로그로 확인할 수 있게 합니다.
///
/// # Arguments
/// `f` - 매핑된 페이지의 물리 주소마다 한 번씩 불림
pub fn device_pages(mut f: impl FnMut(u64)) {
    // SAFETY: 부트 코어의 초기화 시퀀스에서만 호출되어 다른 참조가 없고, 읽기만 함
    let pool = unsafe { &*POOL.0.get() };
    for &page in &pool.dev_pages[..pool.dev_used] {
        f(page);
    }
}

/// 프로브 칸을 `pa`가 속한 페이지로 돌려 놓는 함수입니다.
///
/// 크기를 모르는 디바이스 구간(GICv3 리디스트리뷰터)을 통째로 매핑하지 않고
/// 한 페이지씩 들여다보는 데 씁니다. 칸 하나를 다시 가리키게 하므로
/// break-before-make 순서(무효 엔트리, TLB 무효화, 새 엔트리)를 지킵니다.
///
/// # Arguments
/// `pa` - 들여다볼 레지스터의 물리 주소(정렬 불필요)
///
/// # Errors
/// [map_device]와 같음
pub fn probe_map(pa: u64) -> Result<u64, MmuError> {
    let g = GRANULE as u64;
    let page = pa & !(g - 1);
    // SAFETY: 부트 코어의 초기화 시퀀스(인터럽트 마스크 상태)에서만 호출되어
    //         이 가변 참조는 유일하고, 프로브 칸의 VA를 걷는 다른 실행이 없음
    let pool = unsafe { &mut *POOL.0.get() };
    pool.check_device_page(page)?;
    let attrs = Perm::Device.attrs();
    if pool.probe_leaf == (0, 0) {
        let root1 = pool.root1;
        pool.map_page(root1, DEVICE_VA, page, attrs, false)?;
        pool.probe_leaf = pool.leaf_of(root1, DEVICE_VA)?;
    } else {
        pool.retire_probe();
        let (t, i) = pool.probe_leaf;
        pool.tbl_mut(t)[i] = page | attrs;
    }
    pool.probe_live = true;
    // SAFETY: 새 엔트리를 워커가 보기 전에 쓰기를 완료시킴
    unsafe { asm!("dsb ishst", "isb", options(nostack)) };
    Ok(DEVICE_VA + (pa - page))
}

/// 프로브 칸의 매핑을 없애는 함수입니다. 탐색이 끝나면 불러서 마지막으로
/// 들여다본 디바이스 페이지가 매핑된 채 남지 않게 합니다.
pub fn probe_clear() {
    // SAFETY: probe_map과 같은 전제
    let pool = unsafe { &mut *POOL.0.get() };
    pool.retire_probe();
}

impl Pool {
    fn check_device_page(&self, page: u64) -> Result<(), MmuError> {
        if !self.enabled {
            return Err(MmuError::NotEnabled);
        }
        let end = page.checked_add(GRANULE as u64).ok_or(MmuError::Misaligned)?;
        if end > 1u64 << pa_bits() {
            return Err(MmuError::AboveCeiling);
        }
        if self.overlaps_image(page, end) {
            return Err(MmuError::Overlap);
        }
        Ok(())
    }

    /// 프로브 칸의 리프를 무효로 만들고 그 변환을 TLB에서 지움
    fn retire_probe(&mut self) {
        if !self.probe_live {
            return;
        }
        let (t, i) = self.probe_leaf;
        self.tbl_mut(t)[i] = 0;
        self.probe_live = false;
        // SAFETY: 무효 엔트리 쓰기를 완료시킨 뒤 해당 VA의 마지막 레벨 변환만
        //         지우고, 그 완료를 기다림 (TLBI의 피연산자는 VA를 12비트 내린 값)
        unsafe {
            asm!(
                "dsb ishst",
                "tlbi vale1is, {va}",
                "dsb ish",
                "isb",
                va = in(reg) (DEVICE_VA >> 12) & ((1 << 44) - 1),
                options(nostack),
            );
        }
    }
}

/// 사용자 매핑이 하나도 없는 빈 TTBR0 루트의 PA를 주는 함수입니다.
///
/// 현재 설치된 주소 공간이 종료될 때 TTBR0를 여기로 옮겨, 사라진 테이블을
/// 하드웨어가 걷는 일이 없게 합니다.
pub fn null_user_root() -> u64 {
    // SAFETY: 읽기만 하고, 호출 지점은 단일 코어의 시스템 콜 컨텍스트라
    //         동시 변이가 없음
    let pool = unsafe { &*POOL.0.get() };
    pool.pa(pool.null_root)
}

/// 이 코어가 구현한 물리 주소 폭(비트)을 주는 함수입니다. 커널의 변환
/// 설정에 맞춰 최대 48로 제한합니다. 디바이스 주소의 상한으로 쓰이고, RAM의
/// 상한은 이보다 낮은 `PA_CEILING`입니다.
pub fn pa_bits() -> u32 {
    let mmfr0: u64;
    // SAFETY: ID 레지스터 읽기는 부작용 없음
    unsafe { asm!("mrs {}, id_aa64mmfr0_el1", out(reg) mmfr0, options(nomem, nostack)) };
    match mmfr0 & 0xF {
        0 => 32,
        1 => 36,
        2 => 40,
        3 => 42,
        4 => 44,
        _ => 48,
    }
}

/// 커널 정적 페이지 테이블 풀의 (사용량, 전체) 쌍을 주는 함수입니다.
///
/// 부트 로그와 감사에서 재분류 별칭 매핑의 여유를 확인하는 용도입니다.
pub fn table_pool_usage() -> (usize, usize) {
    // SAFETY: 읽기만 하고, 호출 지점(부트 시퀀스 또는 시스템 콜 컨텍스트)은
    //         단일 코어라 동시 변이가 없음
    let pool = unsafe { &*POOL.0.get() };
    (pool.used, POOL_LEN)
}

/// 변환 레지스터를 설정하고 SCTLR_EL1로 MMU를 켜는 함수입니다.
///
/// I-캐시를 켜기 전에 전체 무효화(`ic iallu`)해 부트로더가 남긴 항목을
/// 신뢰하지 않습니다.
///
/// # Safety
/// 호출 시점의 PC와 SP가 identity 매핑에 포함돼 있어야 하며, 부트 경로에서
/// 단 한 번만 호출해야 합니다. MMU가 꺼진 동안 쓴 구간의 D-캐시 라인은 호출
/// 전에 무효화돼 있어야 합니다. (`enable_paging`이 수행)
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
            "ic iallu",
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

#[cfg(test)]
mod tests;
