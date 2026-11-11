//! 진입 페이즈 3 사용자 주소 공간(TTBR0) 구성 모듈입니다.
//!
//! # Features
//! `map_kernel_window`로 TTBR1에 열어 둔 부트 프레임 윈도우에서 프레임을
//! 자르는 범프 할당자([FrameAlloc])와, 그 프레임으로 사용자 TTBR0 페이지
//! 테이블을 만드는 빌더([UserSpace])를 제공합니다. 이양 이후에는 런타임
//! 워커([user_map_frame], [user_install_table], [user_unmap_frame],
//! [user_uninstall_table])가 케이퍼빌리티로 지목된 주소 공간의 루트
//! 테이블을 직접 걷습니다. 런타임 워커는 테이블을 절대 만들지 않으며(중간
//! 테이블은 사용자가 재분류한 PageTable로만 공급됨) 사용자 매핑은 W^X가
//! 강제됩니다. 실행 가능한 매핑은 읽기 전용이고, 어느 프레임을 실행
//! 가능하게 매핑할 수 있는지는 호출자(케이퍼빌리티 권한 검사)가 정합니다.
//! 첫 그래뉼(VA 0)은 매핑을 거부해 사용자 null 포인터 역참조가 항상 폴트가
//! 되게 합니다. [install_user_ttbr0]가 identity 매핑을 사용자 테이블로
//! 교체하는 순간부터 커널은 TTBR1 별칭으로만 동작합니다.
//!
//! 주소 공간은 정적 등록부([AddrSpaces])에서 번호(asid)와 세대를 받습니다.
//! Frame / PageTable 케이퍼빌리티는 매핑 위치를 포인터가 아닌 (asid, 세대)로
//! 기억하므로, 주소 공간이 먼저 사라진 뒤의 해제 요청은 세대 불일치로
//! 걸러져 사라진 테이블을 걷지 않습니다. 하드웨어 ASID는 쓰지 않습니다.
//! TTBR0를 바꿀 때와 매핑을 제거할 때마다 TLB 전체를 무효화하므로 TLB에는
//! 현재 주소 공간의 변환만 남습니다.
//!
//! # Errors
//! 윈도우 고갈, 정렬 위반, null 페이지 매핑, 중복 매핑, 중간 테이블 부재는
//! 전부 `MmuError`로 반환합니다. 부트 경로의 호출자는 실패 시 부팅을
//! 중단해야 하고(fail-secure), 시스템 콜 경로는 에러를 사용자에게
//! 반환합니다.

use core::arch::asm;
use core::cell::UnsafeCell;
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

/// 테이블 프레임의 PA에 더해 커널이 접근할 주소를 얻는 오프셋
///
/// 커널에서는 TTBR1 별칭이고, 호스트 단위 테스트에서는 0이라 테스트가 직접
/// 할당한 메모리를 테이블로 삼아 워커를 그대로 돌릴 수 있습니다.
#[cfg(not(test))]
const TABLE_OFFSET: u64 = KERNEL_VA_OFFSET;
#[cfg(test)]
const TABLE_OFFSET: u64 = 0;

fn entry_ptr(table_pa: u64, idx: usize) -> *mut u64 {
    (table_pa + TABLE_OFFSET + (idx as u64) * 8) as *mut u64
}

/// 새로 유효해진 테이블 엔트리를 워커가 보기 전에 쓰기를 완료시키는 함수입니다.
fn pte_fence() {
    // SAFETY: 배리어는 부작용이 없음
    unsafe { asm!("dsb ishst", "isb", options(nostack)) };
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

    fn map_page(
        &mut self,
        fa: &mut FrameAlloc,
        va: u64,
        pa: u64,
        attrs: u64,
    ) -> Result<(), MmuError> {
        let mut t = self.root;
        for level in 0..3 {
            let p = entry_ptr(t, index(va, level));
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
        let p = entry_ptr(t, index(va, 3));
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

/// 현재 설치된 사용자 루트 테이블(TTBR0)의 PA를 주는 함수입니다.
///
/// 스케줄러가 다음 태스크의 루트와 비교해 TTBR0 교체 여부를 정하고, 주소
/// 공간이 종료될 때 사라질 루트가 설치돼 있는지 확인하는 데 씁니다. 시스템
/// 콜은 이 값을 쓰지 않고 케이퍼빌리티로 대상 주소 공간을 정합니다. BADDR
/// 필드만 남기고 ASID/CnP 비트는 벗겨냅니다.
pub fn current_user_root() -> u64 {
    let ttbr0: u64;
    // SAFETY: TTBR0_EL1 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, ttbr0_el1", out(reg) ttbr0, options(nomem, nostack)) };
    ttbr0 & 0x0000_FFFF_FFFF_FFFE
}

/// 살아 있는 사용자 주소 공간(케이퍼빌리티로 지목된 루트)에 프레임 하나를
/// 리프 매핑하는 함수입니다.
///
/// MAP 시스템 콜 경로의 워커입니다. 중간 테이블을 절대 만들지 않으며,
/// 경로가 비어 있으면 `MissingTable`을 반환해 사용자가 PageTable을 먼저
/// 매핑하도록 강제합니다. [UserPerm]에는 쓰기와 실행을 함께 주는 값이 없지만
/// [UserPerm::TextUser]는 EL0에서 실행 가능한(읽기 전용) 매핑을 만듭니다.
/// 이 함수는 어느 프레임이 실행 가능해도 되는지를 판단하지 않으므로,
/// 호출자가 Frame 케이퍼빌리티의 실행 권한을 먼저 확인해야 합니다.
///
/// # Arguments
/// `root_pa` - 등록부가 살아 있다고 확인한 사용자 L0 테이블의 PA(현재 TTBR0일
/// 필요는 없음)
/// `va` - 사용자 가상 주소(그래뉼 정렬)
/// `pa` - 프레임 PA(그래뉼 정렬)
/// `perm` - 매핑 권한
///
/// # Errors
/// 정렬 위반, null 가드 침범, 48비트 VA 초과 시 `Misaligned`/`NullPage`,
/// 중간 테이블 부재 시 `MissingTable`, 리프 점유 시 `Overlap`, 테이블
/// 손상 시 `BadTable`
///
/// # Safety
/// `root_pa`는 커널이 설치한 진짜 사용자 루트여야 하고, 걷게 되는 모든
/// 테이블 프레임(부트 윈도우 또는 재분류된 PageTable)이 TTBR1 별칭으로
/// 매핑돼 있어야 합니다.
pub unsafe fn user_map_frame(
    root_pa: u64,
    va: u64,
    pa: u64,
    perm: UserPerm,
) -> Result<(), MmuError> {
    let g = GRANULE as u64;
    if va % g != 0 || pa % g != 0 {
        return Err(MmuError::Misaligned);
    }
    if va < g {
        return Err(MmuError::NullPage);
    }
    let end = va.checked_add(g).ok_or(MmuError::Misaligned)?;
    if end > 1u64 << 48 {
        return Err(MmuError::Misaligned);
    }

    let mut t = root_pa;
    for level in 0..3 {
        let p = entry_ptr(t, index(va, level));
        // SAFETY: 함수 계약대로 t는 별칭 매핑된 테이블 프레임임
        let entry = unsafe { p.read_volatile() };
        t = if entry == 0 {
            return Err(MmuError::MissingTable);
        } else if entry & 0b11 == DESC_TABLE {
            entry & ADDR_MASK
        } else {
            return Err(MmuError::BadTable);
        };
    }
    let p = entry_ptr(t, index(va, 3));
    // SAFETY: 위와 동일, 리프 엔트리 중복 검사 후 기록
    unsafe {
        if p.read_volatile() != 0 {
            return Err(MmuError::Overlap);
        }
        p.write_volatile(pa | perm.attrs());
    }
    pte_fence();
    Ok(())
}

/// 살아 있는 사용자 주소 공간의 `va` 경로에서 첫 빈 레벨에 테이블을 설치하는
/// 함수입니다.
///
/// 재분류된 PageTable 오브젝트를 중간 테이블로 연결하는 워커입니다. 경로를
/// 위에서부터 걷다가 처음 만나는 빈 엔트리에 `table_pa`를 걸고 설치된
/// 레벨(0..=2)을 반환합니다. 경로가 리프까지 이미 완성돼 있으면 설치할
/// 자리가 없으므로 `Overlap`입니다.
///
/// # Arguments
/// `root_pa` - 등록부가 살아 있다고 확인한 사용자 L0 테이블의 PA(현재 TTBR0일
/// 필요는 없음)
/// `va` - 테이블이 뒷받침할 사용자 가상 주소
/// `table_pa` - 소거를 마친 테이블 프레임의 PA(그래뉼 정렬)
///
/// # Errors
/// 정렬 위반과 48비트 VA 초과 시 `Misaligned`, 경로 완성 상태면 `Overlap`,
/// 테이블 손상 시 `BadTable`
///
/// # Safety
/// [user_map_frame]과 동일하며, `table_pa`는 소거된 전용 프레임으로서
/// TTBR1 별칭으로 매핑돼 있어야 합니다.
pub unsafe fn user_install_table(root_pa: u64, va: u64, table_pa: u64) -> Result<u32, MmuError> {
    let g = GRANULE as u64;
    if table_pa % g != 0 || va >= 1u64 << 48 {
        return Err(MmuError::Misaligned);
    }

    let mut t = root_pa;
    for level in 0..3 {
        let p = entry_ptr(t, index(va, level));
        // SAFETY: 함수 계약대로 t는 별칭 매핑된 테이블 프레임임
        let entry = unsafe { p.read_volatile() };
        if entry == 0 {
            // SAFETY: 위와 동일, 빈 엔트리에 소거된 테이블 프레임을 연결
            unsafe { p.write_volatile(table_pa | DESC_TABLE) };
            pte_fence();
            return Ok(level);
        }
        if entry & 0b11 == DESC_TABLE {
            t = entry & ADDR_MASK;
        } else {
            return Err(MmuError::BadTable);
        }
    }
    Err(MmuError::Overlap)
}

/// 주소 공간의 `va`에 `pa` 프레임이 매핑돼 있으면 그 리프를 지우는 함수입니다.
///
/// 리프가 다른 프레임을 가리키거나 경로가 끊겨 있으면 아무것도 바꾸지 않고
/// false를 줍니다. 케이퍼빌리티의 매핑 기록이 실제 테이블과 어긋난 경우
/// (위쪽 테이블이 먼저 떼어졌거나 다른 프레임이 그 자리에 들어온 경우)에
/// 남의 매핑을 지우지 않기 위한 검사입니다. 지운 뒤의 TLB 무효화는 호출자가
/// [flush_user_tlb]로 수행합니다.
///
/// # Arguments
/// `root_pa` - 살아 있는 사용자 L0 테이블의 PA
/// `va` - 매핑 기록의 사용자 가상 주소
/// `pa` - 프레임 PA
///
/// # Errors
/// 테이블 손상 시 `BadTable`
///
/// # Safety
/// [user_map_frame]과 동일합니다.
pub unsafe fn user_unmap_frame(root_pa: u64, va: u64, pa: u64) -> Result<bool, MmuError> {
    let g = GRANULE as u64;
    if va % g != 0 || va >= 1u64 << 48 || pa & !ADDR_MASK != 0 {
        return Ok(false);
    }
    let mut t = root_pa;
    for level in 0..3 {
        let p = entry_ptr(t, index(va, level));
        // SAFETY: 함수 계약대로 t는 별칭 매핑된 테이블 프레임임
        let entry = unsafe { p.read_volatile() };
        if entry == 0 {
            return Ok(false);
        }
        if entry & 0b11 != DESC_TABLE {
            return Err(MmuError::BadTable);
        }
        t = entry & ADDR_MASK;
    }
    let p = entry_ptr(t, index(va, 3));
    // SAFETY: 위와 동일, 이 프레임을 가리키는 리프만 지움
    unsafe {
        let entry = p.read_volatile();
        if entry & 0b11 != DESC_PAGE || entry & ADDR_MASK != pa {
            return Ok(false);
        }
        p.write_volatile(0);
    }
    Ok(true)
}

/// 주소 공간에서 중간 테이블 하나의 연결을 끊는 함수입니다.
///
/// `va` 경로의 `level` 위치가 `table_pa`를 가리킬 때만 그 엔트리를 지웁니다.
/// 그렇지 않으면 아무것도 바꾸지 않고 false를 줍니다. 테이블의 내용은
/// 건드리지 않으므로 떼어 낸 테이블의 소거는 호출자가 [zero_table]로
/// 수행합니다.
///
/// # Arguments
/// `root_pa` - 살아 있는 사용자 L0 테이블의 PA
/// `va` - 설치 때 쓴 사용자 가상 주소
/// `level` - 설치된 레벨(0..=2), [user_install_table]의 반환값
/// `table_pa` - 떼어 낼 테이블 프레임의 PA
///
/// # Errors
/// 테이블 손상이나 범위 밖 레벨은 `BadTable`
///
/// # Safety
/// [user_map_frame]과 동일합니다.
pub unsafe fn user_uninstall_table(
    root_pa: u64,
    va: u64,
    level: u32,
    table_pa: u64,
) -> Result<bool, MmuError> {
    if level > 2 {
        return Err(MmuError::BadTable);
    }
    if va >= 1u64 << 48 || table_pa & !ADDR_MASK != 0 {
        return Ok(false);
    }
    let mut t = root_pa;
    for l in 0..level {
        let p = entry_ptr(t, index(va, l));
        // SAFETY: 함수 계약대로 t는 별칭 매핑된 테이블 프레임임
        let entry = unsafe { p.read_volatile() };
        if entry == 0 {
            return Ok(false);
        }
        if entry & 0b11 != DESC_TABLE {
            return Err(MmuError::BadTable);
        }
        t = entry & ADDR_MASK;
    }
    let p = entry_ptr(t, index(va, level));
    // SAFETY: 위와 동일, 이 테이블을 가리키는 엔트리만 지움
    unsafe {
        if p.read_volatile() != table_pa | DESC_TABLE {
            return Ok(false);
        }
        p.write_volatile(0);
    }
    Ok(true)
}

/// 테이블 프레임 하나를 소거하는 함수입니다.
///
/// 주소 공간에서 떼어 낸 PageTable은 항상 소거합니다. 그래야 그 아래에
/// 걸려 있던 매핑이 테이블과 함께 다른 자리나 다른 주소 공간으로 따라가지
/// 못합니다.
///
/// # Safety
/// `table_pa`는 TTBR1 별칭으로 매핑된 전용 테이블 프레임이어야 하고, 살아
/// 있는 주소 공간에서 이미 끊긴 상태여야 합니다.
pub unsafe fn zero_table(table_pa: u64) {
    // SAFETY: 함수 계약대로 별칭 매핑된 전용 프레임임
    unsafe {
        core::ptr::write_bytes((table_pa + TABLE_OFFSET) as *mut u8, 0, GRANULE);
    }
}

/// 사용자 변환이 담긴 TLB 전체를 무효화하는 함수입니다.
///
/// 매핑을 제거한 뒤에 부릅니다. 하드웨어 ASID를 쓰지 않으므로 TLB에는 현재
/// 주소 공간의 변환만 있고, 전체 무효화가 가장 단순하면서 빠뜨릴 수 없는
/// 방법입니다.
pub fn flush_user_tlb() {
    // SAFETY: 테이블 쓰기를 완료시킨 뒤의 TLB 무효화, 커널 매핑은 다음
    //         접근에서 다시 채워짐
    unsafe {
        asm!(
            "dsb ishst",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            options(nostack),
        );
    }
}

/// 현재 설치된 사용자 루트가 `root_pa`이면 빈 루트로 바꾸는 함수입니다.
///
/// 주소 공간이 종료될 때 부릅니다. 그 뒤로는 하드웨어 워커가 사라진
/// 테이블을 걸을 수 없습니다.
///
/// # Safety
/// 단일 코어의 시스템 콜 컨텍스트에서만 호출해야 합니다.
pub unsafe fn detach_user_root(root_pa: u64) {
    if current_user_root() == root_pa {
        // SAFETY: 빈 루트는 커널 풀의 소거된 전용 테이블이고, 커널은 TTBR0에
        //         의존하지 않음
        unsafe { install_user_ttbr0(crate::paging::null_user_root()) };
    }
}

/// 동시에 살아 있을 수 있는 주소 공간 수의 상한(0번은 무효 값으로 비워 둠)
pub const MAX_ADDR_SPACES: usize = 256;

#[derive(Clone, Copy)]
struct AsEntry {
    /// 등록된 루트 테이블의 PA, 0이면 비어 있음
    root: u64,
    /// 이 항목이 재사용될 때마다 증가하는 세대
    generation: u64,
    /// 빈 항목 목록의 다음 번호(0 = 끝)
    next_free: u16,
}

/// 살아 있는 주소 공간의 등록부 구조체입니다.
///
/// 항목 번호가 asid입니다. 빈 항목은 배열 안의 단일 연결 목록으로 엮여
/// 있어 등록과 해제가 탐색 없이 끝납니다. 해제할 때 세대를 올리므로 옛
/// (asid, 세대) 쌍은 다시는 살아 있는 주소 공간과 일치하지 않습니다.
pub struct AddrSpaces {
    entries: [AsEntry; MAX_ADDR_SPACES],
    free_head: u16,
}

impl AddrSpaces {
    /// 전부 비어 있는 등록부를 만드는 함수입니다.
    pub const fn new() -> Self {
        let mut entries = [AsEntry {
            root: 0,
            generation: 1,
            next_free: 0,
        }; MAX_ADDR_SPACES];
        let mut i = 1;
        while i < MAX_ADDR_SPACES - 1 {
            entries[i].next_free = (i + 1) as u16;
            i += 1;
        }
        Self {
            entries,
            free_head: 1,
        }
    }

    /// 루트 테이블을 등록하고 asid를 주는 함수입니다. 빈 항목이 없으면 `None`
    ///
    /// # Arguments
    /// `root_pa` - 등록할 L0 테이블의 PA(0이 아니어야 함)
    pub fn register(&mut self, root_pa: u64) -> Option<u16> {
        let asid = self.free_head;
        if asid == 0 || root_pa == 0 {
            return None;
        }
        let e = self.entries.get_mut(usize::from(asid))?;
        self.free_head = e.next_free;
        e.next_free = 0;
        e.root = root_pa;
        Some(asid)
    }

    /// 등록을 해제하는 함수입니다. 항목이 그 루트의 것이 아니면 아무것도
    /// 바꾸지 않고 false
    ///
    /// 세대가 더 올라갈 수 없게 된 항목은 다시 쓰지 않고 영구히 비워 둡니다.
    ///
    /// # Arguments
    /// `asid` - 해제할 번호
    /// `root_pa` - 그 번호에 등록돼 있어야 하는 루트 테이블의 PA
    pub fn release(&mut self, asid: u16, root_pa: u64) -> bool {
        let free_head = self.free_head;
        let Some(e) = self.entries.get_mut(usize::from(asid)) else {
            return false;
        };
        if asid == 0 || root_pa == 0 || e.root != root_pa {
            return false;
        }
        e.root = 0;
        match e.generation.checked_add(1) {
            Some(g) => {
                e.generation = g;
                e.next_free = free_head;
                self.free_head = asid;
            }
            None => e.next_free = 0,
        }
        true
    }

    /// 살아 있는 항목의 (루트 PA, 세대)를 주는 함수입니다.
    ///
    /// # Arguments
    /// `asid` - 조회할 번호
    pub fn get(&self, asid: u16) -> Option<(u64, u64)> {
        let e = self.entries.get(usize::from(asid))?;
        (asid != 0 && e.root != 0).then_some((e.root, e.generation))
    }

    /// (asid, 세대)가 지금도 살아 있는 주소 공간이면 그 루트 PA를 주는
    /// 함수입니다.
    ///
    /// # Arguments
    /// `asid` - 매핑 기록의 번호
    /// `generation` - 매핑 기록의 세대
    pub fn resolve(&self, asid: u16, generation: u64) -> Option<u64> {
        match self.get(asid) {
            Some((root, g)) if g == generation => Some(root),
            _ => None,
        }
    }
}

impl Default for AddrSpaces {
    fn default() -> Self {
        Self::new()
    }
}

struct SyncCell(UnsafeCell<AddrSpaces>);

/// # Safety
/// 부트 코어의 초기화 시퀀스와 이양 후의 시스템 콜 컨텍스트(단일 코어, 예외
/// 진입 시 DAIF 마스크)에서만 접근하므로 동시 접근이 없습니다.
unsafe impl Sync for SyncCell {}

static ADDR_SPACES: SyncCell = SyncCell(UnsafeCell::new(AddrSpaces::new()));

/// 커널의 주소 공간 등록부에 접근하는 함수입니다.
///
/// # Safety
/// 단일 코어의 부트 시퀀스나 시스템 콜 컨텍스트에서만 호출해야 하고, 반환된
/// 참조를 그 밖으로 유출하거나 겹치게 만들면 안 됩니다.
pub unsafe fn addr_spaces() -> &'static mut AddrSpaces {
    // SAFETY: 함수 계약대로 이 접근은 배타적임
    unsafe { &mut *ADDR_SPACES.0.get() }
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

/// AT S1E1RP로 PAN을 반영한 EL1 읽기 가능성을 확인하는 함수입니다.
///
/// 일반 AT S1E1R은 PSTATE.PAN을 무시하기 때문에 PAN 집행의 자가 검증에는
/// 이 변형이 필요합니다. FEAT_PAN2 이상에서만 존재하므로 호출 전에 지원
/// 여부를 확인해야 합니다(미지원 코어에서는 미정의 명령). mnemonic이
/// 베이스라인(v8.0) 어셈블러에서 거부되기 때문에 generic sys 인코딩을
/// 사용합니다.
///
/// # Arguments
/// `va` - 검사할 가상 주소
pub fn can_read_pan_checked(va: u64) -> bool {
    let par: u64;
    // SAFETY: AT는 변환 시도만 하고 폴트를 일으키지 않으며 결과는 PAR_EL1에
    //         남음, 호출자가 FEAT_PAN2 지원을 확인한 뒤에만 도달함
    unsafe {
        asm!(
            "sys #0, c7, c9, #0, {va}", // at s1e1rp
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

#[cfg(test)]
mod tests;
