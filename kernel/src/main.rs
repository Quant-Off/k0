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
    static __rodata_start: u8;
    static __data_start: u8;
    static __boot_stack_bottom: u8;
    static __boot_stack_top: u8;
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
            let _mmu = enable_mmu(&mut con, &bootinfo);
            // TODO: 다음 작업에서 예외 벡터 설치(진입 페이즈 2), higher-half 점프
        }
        Err(e) => {
            // 부트 정보 없이는 진행할 수 없으므로 fail-secure 파킹
            let _ = writeln!(con, "k0: dtb rejected: {e:?}");
        }
    }
    park()
}

/// 초기 페이지 테이블을 구성해 MMU를 켜고 W^X 상태를 자가 검증하는 함수입니다.
///
/// 검증은 AT S1E1R/S1E1W 명령으로 수행하므로 실제 폴트 없이 권한을 확인할 수
/// 있습니다. (예외 벡터는 진입 페이즈 2에서 설치되기 때문에 지금 폴트는 곧 행)
///
/// # Errors
/// 활성화 실패나 검증 불일치는 주소 공간을 신뢰할 수 없다는 의미이기 때문에
/// fail-secure 파킹으로 이어집니다.
fn enable_mmu(con: &mut EarlyCon, bootinfo: &k0_boot::BootInfo) -> k0_mm::Mmu {
    let text_start = &raw const __kernel_start as u64;
    let rodata_start = &raw const __rodata_start as u64;
    let data_start = &raw const __data_start as u64;
    let stack_bottom = &raw const __boot_stack_bottom as u64;
    let stack_top = &raw const __boot_stack_top as u64;
    let granule = k0_mm::GRANULE as u64;

    let layout = k0_mm::KernelLayout {
        text: text_start..rodata_start,
        rodata: rodata_start..data_start,
        // 두 구간 사이의 가드 페이지는 의도적으로 매핑안함
        rw: [data_start..stack_bottom - granule, stack_bottom..stack_top],
        dtb: bootinfo.dtb.start as u64..bootinfo.dtb.end as u64,
        mmio: k0_arch::earlycon::MMIO_BASE as u64,
    };

    let mmu = match k0_mm::enable_paging(&layout) {
        Ok(mmu) => mmu,
        Err(e) => {
            let _ = writeln!(con, "k0: mmu enable failed: {e:?}");
            park()
        }
    };

    // (이름, 실측, 기대) 형태의 W^X / 가드 / higher-half 검증표
    let checks: [(&str, bool, bool); 6] = [
        ("text+r", k0_mm::can_read(text_start), true),
        ("text+w", k0_mm::can_write(text_start), false),
        ("rodata+w", k0_mm::can_write(rodata_start), false),
        ("guard+r", k0_mm::can_read(stack_bottom - granule), false),
        ("stack+w", k0_mm::can_write(stack_top - granule), true),
        ("hi+r", k0_mm::can_read(text_start + k0_mm::KERNEL_VA_OFFSET), true),
    ];
    let mut ok = true;
    for (name, got, want) in checks {
        if got != want {
            ok = false;
            let _ = writeln!(con, "k0: w^x check fail: {name} (got {got}, want {want})");
        }
    }
    if !ok {
        park()
    }
    let _ = writeln!(con, "k0: mmu on (granule {}K, wxn, w^x checks pass)", k0_mm::GRANULE / 1024);
    mmu
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
