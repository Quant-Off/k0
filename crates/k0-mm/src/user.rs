//! 진입 페이즈 3 사용자 주소 공간(TTBR0) 구성 모듈입니다.
//!
//! # Features
//! `map_kernel_window`로 TTBR1에 열어 둔 부트 프레임 윈도우에서 프레임을
//! 자르는 범프 할당자([FrameAlloc])와, 그 프레임으로 사용자 TTBR0 페이지
//! 테이블을 만드는 빌더([UserSpace])를 제공합니다. 사용자 매핑은 W^X가
//! 강제되며 실행 가능한 조합은 [UserPerm::TextUser](EL0 전용, 커널에서는
//! PXN) 하나뿐입니다. 첫 그래뉼(VA 0)은 매핑을 거부해 사용자 null 포인터
//! 역참조가 항상 폴트가 되게 합니다. [install_user_ttbr0]가 identity 매핑을
//! 사용자 테이블로 교체하는 순간부터 커널은 TTBR1 별칭으로만 동작합니다.
//!
//! # Errors
//! 윈도우 고갈, 정렬 위반, null 페이지 매핑, 중복 매핑은 전부 `MmuError`로
//! 반환합니다. 호출자는 실패 시 부팅을 중단해야 합니다(fail-secure).

use core::arch::asm;
use core::ops::Range;

use crate::paging::{
    index, MmuError, ADDR_MASK, AP_RO_ALL, AP_RW_ALL, ATTR_AF, DESC_PAGE, DESC_TABLE, GRANULE,
    IDX_NORMAL, KERNEL_VA_OFFSET, PXN, SH_INNER, UXN,
};

/// 사용자(EL0) 매핑 권한을 나타내는 열거형입니다.
///
/// 실행 가능한 것은 [UserPerm::TextUser] 하나뿐이고, 그마저 커널(EL1)에서는
/// PXN이라 실행이 거부됩니다. 쓰기 가능한 매핑은 전부 UXN+PXN입니다.
#[derive(Clone, Copy)]
pub enum UserPerm {
    TextUser,
    RoUser,
    RwUser,
}

impl UserPerm {
    fn attrs(self) -> u64 {
        match self {
            UserPerm::TextUser => DESC_PAGE | ATTR_AF | SH_INNER | AP_RO_ALL | IDX_NORMAL | PXN,
            UserPerm::RoUser => {
                DESC_PAGE | ATTR_AF | SH_INNER | AP_RO_ALL | IDX_NORMAL | UXN | PXN
            }
            UserPerm::RwUser => {
                DESC_PAGE | ATTR_AF | SH_INNER | AP_RW_ALL | IDX_NORMAL | UXN | PXN
            }
        }
    }
}

/// 부트 프레임 윈도우에서 그래뉼 단위 프레임을 자르는 범프 할당자 구조체입니다.
///
/// 반환된 프레임은 TTBR1 별칭(`PA + KERNEL_VA_OFFSET`)으로 접근 가능하고
/// 소거(zeroize)된 상태입니다. 해제는 없습니다. 남는 물리 메모리 전부는
/// untyped로 루트 태스크에 넘어가는 구조라서, 이 할당자는 루트 태스크
/// 적재에 쓰인 뒤 버려집니다.
pub struct FrameAlloc {
    start: u64,
    next: u64,
    end: u64,
}

impl FrameAlloc {
    /// `map_kernel_window`로 매핑을 마친 윈도우 위에 할당자를 만드는 함수입니다.
    ///
    /// # Arguments
    /// `window` - 그래뉼 정렬된 물리 구간
    ///
    /// # Errors
    /// 정렬 위반이나 빈 구간이면 `Misaligned`
    pub fn new(window: Range<u64>) -> Result<Self, MmuError> {
        let g = GRANULE as u64;
        if window.start % g != 0 || window.end % g != 0 || window.end <= window.start {
            return Err(MmuError::Misaligned);
        }
        Ok(Self {
            start: window.start,
            next: window.start,
            end: window.end,
        })
    }

    /// 연속 프레임 `n`개를 할당하고 소거해 첫 프레임의 PA를 주는 함수입니다.
    ///
    /// # Arguments
    /// `n` - 그래뉼 프레임 수
    ///
    /// # Errors
    /// 윈도우가 부족하면 `OutOfFrames`
    pub fn alloc_contig(&mut self, n: u64) -> Result<u64, MmuError> {
        let g = GRANULE as u64;
        let len = n.checked_mul(g).ok_or(MmuError::OutOfFrames)?;
        if len == 0 {
            return Err(MmuError::OutOfFrames);
        }
        let base = self.next;
        let end = base.checked_add(len).ok_or(MmuError::OutOfFrames)?;
        if end > self.end {
            return Err(MmuError::OutOfFrames);
        }
        self.next = end;
        // SAFETY: 이 구간은 방금 예약됐고 윈도우 전체가 TTBR1에 RW로 매핑돼 있음
        unsafe {
            core::ptr::write_bytes((base + KERNEL_VA_OFFSET) as *mut u8, 0, len as usize);
        }
        Ok(base)
    }

    /// 프레임 하나를 할당하는 함수입니다.
    ///
    /// # Errors
    /// 윈도우가 부족하면 `OutOfFrames`
    pub fn alloc(&mut self) -> Result<u64, MmuError> {
        self.alloc_contig(1)
    }

    /// 지금까지 소비한 물리 구간을 주는 함수입니다.
    pub fn used(&self) -> Range<u64> {
        self.start..self.next
    }
}

/// 사용자 TTBR0 페이지 테이블을 만드는 빌더 구조체입니다.
///
/// 테이블 프레임은 전부 [FrameAlloc]에서 나오고, 커널은 TTBR1 별칭으로만
/// 테이블을 씁니다. 완성된 테이블은 [install_user_ttbr0]로 설치합니다.
pub struct UserSpace {
    root: u64,
}

impl UserSpace {
    /// 빈 루트(L0) 테이블 하나로 사용자 주소 공간을 시작하는 함수입니다.
    ///
    /// # Arguments
    /// `fa` - 테이블 프레임을 공급할 할당자
    ///
    /// # Errors
    /// 윈도우가 부족하면 `OutOfFrames`
    pub fn new(fa: &mut FrameAlloc) -> Result<Self, MmuError> {
        Ok(Self { root: fa.alloc()? })
    }

    /// 루트 테이블의 PA(TTBR0에 설치할 값)를 주는 함수입니다.
    pub fn root_pa(&self) -> u64 {
        self.root
    }

    fn entry_ptr(table_pa: u64, idx: usize) -> *mut u64 {
        (table_pa + KERNEL_VA_OFFSET + (idx as u64) * 8) as *mut u64
    }

    fn map_page(
        &mut self,
        fa: &mut FrameAlloc,
        va: u64,
        pa: u64,
        attrs: u64,
    ) -> Result<(), MmuError> {
        let mut t = self.root;
        for level in 0..3 {
            let p = Self::entry_ptr(t, index(va, level));
            // SAFETY: t는 이 빌더가 할당한 윈도우 안의 테이블 프레임이라
            //         별칭으로 접근 가능함
            let entry = unsafe { p.read_volatile() };
            t = if entry == 0 {
                let child = fa.alloc()?;
                // SAFETY: 위와 동일, 새 테이블 프레임을 하위 테이블로 연결
                unsafe { p.write_volatile(child | DESC_TABLE) };
                child
            } else if entry & 0b11 == DESC_TABLE {
                entry & ADDR_MASK
            } else {
                return Err(MmuError::BadTable);
            };
        }
        let p = Self::entry_ptr(t, index(va, 3));
        // SAFETY: 위와 동일, 리프 엔트리 중복 검사 후 기록
        unsafe {
            if p.read_volatile() != 0 {
                return Err(MmuError::Overlap);
            }
            p.write_volatile(pa | attrs);
        }
        Ok(())
    }

    /// 사용자 구간 `[va, va+len)`을 물리 `[pa, pa+len)`에 매핑하는 함수입니다.
    ///
    /// # Arguments
    /// `fa` - 중간 테이블 프레임을 공급할 할당자
    /// `va` - 사용자 가상 주소(그래뉼 정렬)
    /// `pa` - 물리 주소(그래뉼 정렬)
    /// `len` - 길이(그래뉼 배수)
    /// `perm` - 매핑 권한
    ///
    /// # Errors
    /// 정렬 위반, 첫 그래뉼(null 가드) 침범, 48비트 사용자 VA 초과, 중복 매핑,
    /// 프레임 고갈 시 `MmuError`
    pub fn map_range(
        &mut self,
        fa: &mut FrameAlloc,
        va: u64,
        pa: u64,
        len: u64,
        perm: UserPerm,
    ) -> Result<(), MmuError> {
        let g = GRANULE as u64;
        if va % g != 0 || pa % g != 0 || len % g != 0 || len == 0 {
            return Err(MmuError::Misaligned);
        }
        if va < g {
            return Err(MmuError::NullPage);
        }
        let end = va.checked_add(len).ok_or(MmuError::Misaligned)?;
        if end > 1u64 << 48 {
            return Err(MmuError::Misaligned);
        }
        let attrs = perm.attrs();
        let mut off = 0;
        while off < len {
            self.map_page(fa, va + off, pa + off, attrs)?;
            off += g;
        }
        Ok(())
    }
}

/// TTBR0을 identity에서 사용자 테이블로 교체하는 함수입니다.
///
/// 교체 후 TLB 전체를 무효화해 잔존 identity 변환을 제거합니다. 이 시점부터
/// 물리(identity) 주소 접근은 전부 무효합니다.
///
/// # Arguments
/// `root_pa` - 사용자 L0 테이블의 PA
///
/// # Safety
/// 커널의 모든 접근 경로(콘솔, GIC, DTB, 페이지 테이블)가 TTBR1 별칭으로
/// 이행을 마친 뒤에만 호출해야 합니다. `root_pa`는 [UserSpace]가 완성한
/// 유효한 루트 테이블이어야 합니다.
pub unsafe fn install_user_ttbr0(root_pa: u64) {
    // SAFETY: 함수 계약대로 identity 의존이 남아있지 않은 시점에 호출됨
    unsafe {
        asm!(
            "dsb ishst",
            "msr ttbr0_el1, {t0}",
            "isb",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            t0 = in(reg) root_pa,
            options(nostack),
        );
    }
}

/// AT S1E0R로 해당 VA가 EL0에서 읽기 가능한지 확인하는 함수입니다.
///
/// # Arguments
/// `va` - 검사할 가상 주소
pub fn can_user_read(va: u64) -> bool {
    let par: u64;
    // SAFETY: AT는 변환 시도만 하고 폴트를 일으키지 않으며 결과는 PAR_EL1에 남음
    unsafe {
        asm!(
            "at s1e0r, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack),
        );
    }
    par & 1 == 0
}

/// AT S1E0W로 해당 VA가 EL0에서 쓰기 가능한지 확인하는 함수입니다.
///
/// # Arguments
/// `va` - 검사할 가상 주소
pub fn can_user_write(va: u64) -> bool {
    let par: u64;
    // SAFETY: AT는 변환 시도만 하고 폴트를 일으키지 않으며 결과는 PAR_EL1에 남음
    unsafe {
        asm!(
            "at s1e0w, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack),
        );
    }
    par & 1 == 0
}
