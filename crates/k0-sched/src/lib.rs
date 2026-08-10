//! 진입 페이즈 3의 스케줄러 크레이트입니다.
//!
//! # Features
//! 커널에서 사용자 공간으로 제어권을 이양(handoff)합니다. 이양 이후 커널은
//! 트랩/인터럽트/시스템 콜에만 반응하는 수동적 존재가 됩니다. 지금은 단일
//! 태스크라 선점 시 복귀 대상이 항상 같고, 실행 큐와 시간 할당은 다중
//! 태스크 확장과 함께 추가됩니다.

#![no_std]

use k0_task::Tcb;

/// 루트 태스크로 제어권을 이양하는 함수입니다. 복귀하지 않습니다.
///
/// 벡터의 EL0 경로가 저장/복원할 컨텍스트를 지정한 뒤 EL0로 진입합니다.
/// 이후 커널 코드는 예외로만 실행됩니다.
///
/// # Arguments
/// `tcb` - 스폰을 마친 루트 태스크의 TCB
///
/// # Safety
/// TCB의 주소 공간(TTBR0)이 설치된 뒤에만 호출해야 합니다. 커널의 identity
/// 의존이 남아 있으면 안 됩니다(호출 순서는 kernel_main이 강제).
pub unsafe fn handoff(tcb: &'static mut Tcb) -> ! {
    k0_arch::usermode::set_current(&raw mut tcb.ctx);
    // SAFETY: 함수 계약대로 컨텍스트와 주소 공간이 준비된 발산 경로에서 호출됨
    unsafe { k0_arch::usermode::enter_user() }
}
