//! 커널로 넘어가는 마지막 단계입니다.
//!
//! # Features
//! 커널은 MMU와 데이터 캐시가 꺼진 채, 예외가 모두 마스크된 채, x0에 DTB의
//! 물리 주소를 받고 시작합니다. UEFI는 MMU와 캐시를 켠 채 이 로더를 실행하므로
//! 넘어가기 직전에 그 상태를 만듭니다.
//!
//! MMU를 끄면 데이터 접근이 캐시를 거치지 않습니다. 그러니 방금 복사한 커널
//! 이미지와 방금 쓴 DTB가 캐시에만 있고 메모리에는 아직 없다면 커널은 옛
//! 내용을 읽게 됩니다. 그래서 끄기 전에 그 구간을 메모리까지 밀어냅니다.
//! QEMU의 TCG에는 캐시가 없어 이 단계가 빠져도 부팅이 되므로, 부팅 시험은
//! 이 단계의 증거가 되지 못합니다. 순서는 Linux가 MMU를 끄고 커널에 들어가던
//! 때의 arm64 EFI 진입 코드를 따랐고, 빌드 뒤 디스어셈블리로 명령의 존재와
//! 순서를 검사합니다.
//!
//! 타이머와 인터럽트 컨트롤러는 건드리지 않습니다. 커널이 인터럽트를 풀기
//! 전에 인터럽트 컨트롤러를 스스로 정리하고, EL1 게스트에서는 물리 타이머
//! 레지스터 접근이 하이퍼바이저로 트랩될 수 있기 때문입니다.

use core::arch::{asm, global_asm};
use core::ops::Range;

// 트램펄린은 이 PE의 .text 안에 있음. UEFI는 identity 매핑으로 실행하므로
// MMU를 끈 뒤에도 PC가 그대로 유효함. 스택과 데이터는 쓰지 않음.
//   x0 = 커널 진입 주소, x1 = DTB 물리 주소
// 진입 EL은 그대로 넘김(커널이 EL1과 EL2 진입을 모두 처리)
global_asm!(
    r#"
    .section .text.k0_handoff, "ax"
    .balign 64
    .global k0_handoff
k0_handoff:
    msr     daifset, #0xf
    mov     x9, x0
    mov     x0, x1
    mrs     x4, CurrentEL
    cmp     x4, #8
    b.ne    1f
    mrs     x4, sctlr_el2
    bic     x4, x4, #1
    bic     x4, x4, #4
    dsb     sy
    isb
    msr     sctlr_el2, x4
    isb
    b       2f
1:  mrs     x4, sctlr_el1
    bic     x4, x4, #1
    bic     x4, x4, #4
    dsb     sy
    isb
    msr     sctlr_el1, x4
    isb
2:  mov     x1, xzr
    mov     x2, xzr
    mov     x3, xzr
    br      x9
    .global k0_handoff_end
k0_handoff_end:
    .balign 64
"#
);

unsafe extern "C" {
    fn k0_handoff(entry: u64, dtb: u64) -> !;
    fn k0_handoff_end();
}

/// 구간의 데이터 캐시 줄을 메모리까지 밀어내는 함수입니다.
///
/// # Arguments
/// `range` - 밀어낼 가상 주소 구간(identity 매핑이라 물리 주소와 같음)
///
/// # Safety
/// 구간 전체가 매핑된 메모리여야 합니다.
#[inline(never)]
unsafe fn clean_to_memory(range: &Range<u64>) {
    let ctr: u64;
    // SAFETY: CTR_EL0 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let line = 4u64 << ((ctr >> 16) & 0xF);
    let mut at = range.start & !(line - 1);
    while at < range.end {
        // SAFETY: 호출자가 구간이 매핑돼 있음을 보장함
        unsafe { asm!("dc cvac, {}", in(reg) at, options(nostack)) };
        at += line;
    }
    // SAFETY: 배리어는 메모리를 건드리지 않음
    unsafe { asm!("dsb sy", options(nostack)) };
}

/// 캐시를 정리하고 MMU를 끈 뒤 커널로 넘어가는 함수입니다. 돌아오지 않습니다.
///
/// # Arguments
/// `image` - 커널 이미지 영역 전체(BSS와 부트 스택 자리 포함)
/// `dtb` - 커널에 넘길 DTB가 든 버퍼 전체
///
/// # Safety
/// ExitBootServices가 성공한 뒤에만 부를 수 있습니다. `image`의 처음에는 이
/// CPU에서 실행할 수 있는 커널 Image가 복사돼 있어야 하고, 두 구간은 이
/// 로더가 할당한 메모리여야 합니다.
pub unsafe fn enter_kernel(image: &Range<u64>, dtb: &Range<u64>) -> ! {
    // SAFETY: 부트 서비스가 끝났으므로 인터럽트를 받을 곳이 없음. 트램펄린도
    //         처음에 다시 마스크함
    unsafe { asm!("msr daifset, #0xf", options(nomem, nostack)) };
    let trampoline = k0_handoff as usize as u64..k0_handoff_end as usize as u64 + 64;
    // SAFETY: 세 구간 모두 이 로더의 메모리(할당한 블록과 자신의 .text)임
    unsafe {
        clean_to_memory(image);
        clean_to_memory(dtb);
        clean_to_memory(&trampoline);
        // 복사한 커널 코드에 대해 남아 있을 수 있는 명령 캐시 항목을 버림
        asm!("ic ialluis", "dsb ish", "isb", options(nostack));
        k0_handoff(image.start, dtb.start)
    }
}

/// 부트 서비스를 끝낸 뒤 더 진행할 수 없을 때 멈추는 함수입니다.
pub fn park() -> ! {
    loop {
        // SAFETY: 인터럽트를 마스크하고 기다리는 것은 다른 상태를 바꾸지 않음
        unsafe { asm!("msr daifset, #0xf", "wfe", options(nomem, nostack)) };
    }
}

/// 현재 예외 레벨을 주는 함수입니다.
pub fn current_el() -> u64 {
    let v: u64;
    // SAFETY: CurrentEL 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, CurrentEL", out(reg) v, options(nomem, nostack)) };
    (v >> 2) & 3
}

/// ID_AA64MMFR0_EL1을 읽는 함수입니다.
pub fn mmfr0() -> u64 {
    let v: u64;
    // SAFETY: ID 레지스터 읽기는 부작용이 없음
    unsafe { asm!("mrs {}, id_aa64mmfr0_el1", out(reg) v, options(nomem, nostack)) };
    v
}
