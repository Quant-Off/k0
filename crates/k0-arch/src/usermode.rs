//! 진입 페이즈 3 사용자 공간(EL0) 진입/복귀와 동기 예외 처리 모듈입니다.
//!
//! # Features
//! 사용자 레지스터 컨텍스트([Context])의 배치를 정의하고, 벡터 테이블의
//! EL0 경로(`vectors.S`의 `__lower_common` / `__user_restore`)가 쓰는
//! `__current_context` 포인터를 관리합니다. EL0 동기 예외는 svc(시스템 콜)와
//! WFx(양보로 취급)만 복귀 가능하고, 그 외(폴트)는 fail-secure 진단으로
//! 수렴합니다. 시스템 콜의 의미는 커널 바이너리가 `k0_syscall`로 정의합니다.
//!
//! # Errors
//! EL0 폴트는 태스크를 살리지 않고 신드롬을 출력한 채 정지합니다. 폴트를
//! IPC로 핸들러 태스크에 전달하는 구조는 설계된 확장 지점입니다.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// EL0 예외 진입 시 벡터가 저장하는 사용자 레지스터 컨텍스트 구조체입니다.
///
/// 필드 오프셋(x: 0..248, sp: 248, elr: 256, spsr: 264)은 `vectors.S`의
/// `__lower_common` / `__user_restore`와 반드시 일치해야 합니다.
#[repr(C)]
pub struct Context {
    pub x: [u64; 31],
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
}

const _: () = assert!(/*core::mem::*/size_of::<Context>() == 272);

impl Context {
    /// 소거된 컨텍스트를 만드는 함수입니다.
    pub const fn zeroed() -> Self {
        Self {
            x: [0; 31],
            sp: 0,
            elr: 0,
            spsr: 0,
        }
    }
}

// 벡터의 EL0 경로가 읽는 현재 태스크 컨텍스트 포인터
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
static __current_context: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    /// 시스템 콜 정책, 커널 바이너리가 정의함 (링크 계약)
    ///
    /// # Safety
    /// `ctx`는 `__current_context`가 가리키는 유효한 컨텍스트입니다.
    fn k0_syscall(ctx: &mut Context);
}

/// 벡터가 저장/복원할 현재 태스크 컨텍스트를 지정하는 함수입니다.
///
/// # Arguments
/// `ctx` - 태스크의 컨텍스트('static 수명, TCB 내부)
pub fn set_current(ctx: *mut Context) {
    __current_context.store(ctx as u64, Ordering::Release);
}

/// 현재 컨텍스트를 복원해 EL0로 진입하는 함수입니다. 복귀하지 않습니다.
///
/// 커널 스택을 최상단으로 되감은 뒤 벡터의 복원 경로(`__user_restore`)를
/// 그대로 사용합니다. 이후 커널 코드는 예외(트랩/인터럽트/시스템 콜)로만
/// 실행됩니다.
///
/// # Safety
/// `set_current`로 유효한 컨텍스트가 지정돼 있어야 하고, 컨텍스트의 ELR/SP가
/// 사용자 매핑(TTBR0)에서 유효해야 합니다. 호출 스택은 되감기므로 이 함수
/// 아래의 스택 프레임을 참조하는 것이 없어야 합니다(발산 경로에서만 호출).
pub unsafe fn enter_user() -> ! {
    // SAFETY: 함수 계약 전제 하에 스택을 되감고 복원 경로로 점프함
    unsafe {
        asm!(
            "adrp x0, __boot_stack_top",
            "add x0, x0, :lo12:__boot_stack_top",
            "mov sp, x0",
            "b __user_restore",
            options(noreturn),
        )
    }
}

/// EL0 동기 예외 벡터가 부르는 디스패처 함수입니다.
///
/// # Arguments
/// `ctx` - 벡터가 저장을 마친 사용자 컨텍스트
#[unsafe(no_mangle)]
extern "C" fn el0_sync(ctx: &mut Context) {
    let esr: u64;
    // SAFETY: 예외 신드롬 레지스터 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, esr_el1", out(reg) esr, options(nomem, nostack)) };

    match (esr >> 26) & 0x3F {
        // svc: ELR은 이미 다음 명령을 가리키고, 정책은 커널이 정의함
        // SAFETY: ctx는 벡터가 방금 저장한 현재 컨텍스트
        0x15 => unsafe { k0_syscall(ctx) },
        // EL0의 WFI/WFE 트랩(SCTLR nTWI/nTWE=0): 양보로 취급하고 다음 명령으로
        0x01 => ctx.elr += 4,
        // 그 외 동기 예외(폴트)는 fail-secure 진단으로 정지
        _ => crate::vectors::exception_fatal(8),
    }
}

/// 사용자 코드 적재 후 I-캐시와 D-캐시를 동기화하는 함수입니다.
///
/// 새로 복사한 코드를 EL0가 실행하기 전에 D-캐시를 PoU까지 클린하고
/// I-캐시를 무효화합니다. QEMU(TCG)는 없어도 동작하지만 실 하드웨어
/// (Apple Silicon)에서는 필수입니다.
///
/// # Arguments
/// `va` - 적재에 사용한(매핑이 유효한) 시작 주소
/// `len` - 길이
pub fn sync_icache(va: usize, len: usize) {
    let ctr: u64;
    // SAFETY: CTR_EL0 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let d_line = 4usize << ((ctr >> 16) & 0xF);
    let i_line = 4usize << (ctr & 0xF);
    let end = va + len;

    let mut p = va & !(d_line - 1);
    while p < end {
        // SAFETY: dc cvau는 캐시 유지 보수만 수행함
        unsafe { asm!("dc cvau, {}", in(reg) p, options(nostack)) };
        p += d_line;
    }
    // SAFETY: 배리어 후 I-캐시 무효화, 마지막 isb로 파이프라인 재인출
    unsafe { asm!("dsb ish", options(nostack)) };
    let mut p = va & !(i_line - 1);
    while p < end {
        // SAFETY: ic ivau는 캐시 유지 보수만 수행함
        unsafe { asm!("ic ivau, {}", in(reg) p, options(nostack)) };
        p += i_line;
    }
    // SAFETY: 위와 동일
    unsafe { asm!("dsb ish", "isb", options(nostack)) };
}
