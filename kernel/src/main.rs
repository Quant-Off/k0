//! K0 커널 바이너리 크레이트입니다.
//!
//! 이 크레이트는 의도적으로 얇게 설계되었습니다. 이 크레이트는 진입 페이즈 0 어셈블리 포함,
//! 초기화 시퀀스의 순서 강제, fail-secure panic 핸들러만 담습니다. 모든 실제 로직은
//! `crates/` 아래의 라이브러리 크레이트(k0-*)에 위치해 있습니다.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::panic::PanicInfo;

use k0_arch::earlycon::EarlyCon;

mod boot; // arch/aarch64/boot.S를 global_asm! 으로 포함

// 링커 스크립트가 export하는 심볼들
unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
    static __boot_stack_bottom: u8;
}

/// 진입 페이즈 0(`boot.S`)에서 점프해 들어오는 유일한 Rust 진입점입니다.
///
/// 진입 조건은 다음과 같습니다. (`boot.S`가 보장)
/// - EL1h, DAIF 전부 마스크, MMU/캐시 OFF
/// - `sp` = `__boot_stack_top`, `.bss` 소거(zeroize) 완료
/// - `dtb_phys` = 부트로더가 전달한 DTB 물리 주소
#[unsafe(no_mangle)]
extern "C" fn kernel_init(dtb_phys: usize) -> ! {
    // 진입 페이즈 1 이후는 typestate 패턴으로 순서를 컴파일 타임에 강제
    // 각 단계가 반환하는 토큰이 다음 단계의 입력이 됨
    // 아래는 그냥 스케치임.
    //
    // let verified = k0_boot::verify_root_task(&bootinfo);
    // let mmu      = k0_mm::enable_paging(&bootinfo);
    // let traps    = k0_arch::install_vectors(&mmu);
    // let irq      = k0_arch::init_gic(&traps);
    // let caps     = k0_cap::bootstrap(&mmu, &bootinfo);
    // let root     = k0_task::spawn_root(&caps, verified);
    // k0_sched::start(root, irq)

    let mut con = EarlyCon;
    let _ = writeln!(con, "k0: entry phase 1 (pre-MMU)");
    let _ = writeln!(con, "k0: dtb = {dtb_phys:#x}");

    // 링커 스크립트가 정의한 심볼의 주소만 취하고 역참조하지 않음
    let kernel_image = (&raw const __kernel_start as usize)..(&raw const __kernel_end as usize);

    match k0_boot::parse(dtb_phys, kernel_image) {
        Ok(bootinfo) => {
            for r in bootinfo.memory() {
                let _ = writeln!(con, "k0: memory {:#x} + {:#x}", r.base, r.size);
            }
        }
        Err(e) => {
            // 부트 정보 없이는 진행할 수 없으므로 fail-secure 파킹
            let _ = writeln!(con, "k0: dtb rejected: {e:?}");
        }
    }
    park()
}

/// 고보안 시스템의 `panic`은 fail-secure 이어야 하기 때문에
/// 인터럽트 마스크(interrupt mask) -> (필요 시 민감 상태 zeroize) -> 정지
/// 와 같이 동작하며, 프로덕션 프로파일에서는 어떤 정보도 출력하지 않습니다.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    unsafe { core::arch::asm!("msr DAIFSet, #0xf", options(nomem, nostack)) };
    park()
}

#[inline(always)]
fn park() -> ! {
    loop {
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}
