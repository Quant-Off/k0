//! K0 커널 바이너리 크레이트입니다.
//!
//! 이 크레이트는 의도적으로 얇게 설계되었습니다. 이 크레이트는 진입 페이즈 0 어셈블리 포함,
//! 초기화 시퀀스의 순서 강제, fail-secure panic 핸들러만 담습니다. 모든 실제 로직은
//! `crates/` 아래의 라이브러리 크레이트(k0-*)에 위치해 있습니다.
//!
//! 초기화는 higher-half 점프를 경계로 두 단으로 나뉩니다. 커널은 higher-half VA에
//! 링크되지만 물리 주소로 적재되기 때문에, 점프 전 구간(`kernel_init`)에는 제약이
//! 하나 있습니다. 주소를 만드는 코드(adr / adrp)는 PC 상대라 물리 주소가 맞게
//! 나오지만, 포인터 값을 담고 있는 데이터(fat 포인터 static, vtable, 이중
//! 참조 리터럴 등)는 링크된 절대 VA로 재배치되어 있어 점프 전에 역참조하면
//! 주소 크기 폴트가 납니다. 그래서 점프 전에는 문자열 리터럴 출력과 정수
//! 연산만 쓰고, DTB 파싱/루트 태스크 검증/`writeln!`은 전부 점프
//! 이후(`kernel_main`)로 미룹니다.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ops::Range;

use k0_arch::earlycon::EarlyCon;

mod boot; // arch/aarch64/boot.S를 global_asm! 으로 포함
mod panic; // fail-secure panic 핸들러와 파킹 루프

use panic::park;

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
///
/// 이 함수는 점프 전 구간이기 때문에 재배치된 데이터를 건드리지 않습니다.
#[unsafe(no_mangle)]
extern "C" fn kernel_init(dtb_phys: usize) -> ! {
    // 예외 벡터 최우선 설치 (지금은 물리 주소, 점프 후 VA로 재설치)
    // 이 전의 폴트는 진단 불가능한 행이 됨
    let _traps = k0_arch::vectors::install();

    let mut con = EarlyCon;
    con.put_str("k0: entry phase 1 (pre-MMU)\n");
    con.put_str("k0: dtb = ");
    con.put_hex(dtb_phys as u64);
    con.put_str("\n");

    // 본 파싱은 점프 이후에 하고, 여기서는 매핑에 필요한 크기만 헤더에서 얻음
    let dtb = match k0_boot::dtb_span(dtb_phys) {
        Ok(dtb) => dtb,
        Err(e) => {
            con.put_str("k0: dtb header rejected, code = ");
            con.put_hex(e as u64);
            con.put_str("\n");
            park()
        }
    };

    enable_mmu(&mut con, &dtb);
    con.put_str("k0: jumping to higher half\n");

    // SAFETY: enable_mmu가 TTBR1에 커널 별칭을 매핑했고 kernel_main의 링크
    //         주소는 higher-half VA임. SP도 같은 오프셋으로 올려 별칭 위로 옮기고,
    //         x0으로 DTB 물리 주소를 그대로 넘김 (extern "C" 첫 인자)
    unsafe {
        core::arch::asm!(
            "add sp, sp, {off}",
            "movz x8, #:abs_g3:kernel_main",
            "movk x8, #:abs_g2_nc:kernel_main",
            "movk x8, #:abs_g1_nc:kernel_main",
            "movk x8, #:abs_g0_nc:kernel_main",
            "br x8",
            off = in(reg) k0_mm::KERNEL_VA_OFFSET,
            in("x0") dtb_phys,
            options(noreturn),
        )
    }
}

/// higher-half로 점프한 뒤의 초기화 후반부 함수입니다.
///
/// PC와 SP가 TTBR1 별칭 위에 있으므로 여기서부터 재배치된 데이터가 유효해져
/// 전체 Rust를 제약 없이 쓸 수 있습니다. 점프가 typestate 토큰 전달을
/// 끊으므로 초기화 순서는 이 함수의 호출 순서로 강제합니다.
///
/// # Arguments
/// `dtb_phys` - `kernel_init`이 x0으로 넘긴 DTB 물리 주소
#[unsafe(no_mangle)]
extern "C" fn kernel_main(dtb_phys: usize) -> ! {
    // 벡터 재설치: 같은 테이블이지만 이제 PC가 higher-half라 VBAR도 VA가 됨
    let traps = k0_arch::vectors::install();
    // 콘솔도 TTBR1 별칭으로 이행, TTBR0을 회수해도 살아 있어야 함
    k0_arch::earlycon::use_higher_half();

    let mut con = EarlyCon;
    let pc: u64;
    // SAFETY: 현재 PC 근처 주소를 읽기만 함
    unsafe { core::arch::asm!("adr {}, .", out(reg) pc, options(nomem, nostack)) };
    let _ = writeln!(con, "k0: higher half (pc = {pc:#x})");

    check_wx(&mut con);

    // DTB도 TTBR1 별칭으로 읽음(겹침 검사와 메모리 맵은 물리 주소 기준)
    let kernel_image = pa(&raw const __kernel_start)..pa(&raw const __kernel_end);
    let bootinfo = match k0_boot::parse(
        dtb_phys,
        kernel_image.clone(),
        k0_mm::KERNEL_VA_OFFSET as usize,
    ) {
        Ok(bootinfo) => bootinfo,
        Err(e) => {
            // 부트 정보 없이는 진행할 수 없어서 fail-secure 파킹
            let _ = writeln!(con, "k0: dtb rejected: {e:?}");
            park()
        }
    };
    for r in bootinfo.memory() {
        let _ = writeln!(con, "k0: memory {:#x} + {:#x}", r.base, r.size);
    }

    // 루트 태스크 무결성(손상) 검사: 빌드 및 적재 경로의 변형을 걸러내는 단계
    // 커널 이미지 자체를 수정할 수 있는 공격자는 부트 체인의 커널 서명
    // 검증만이 막을 수 있음(해시 기준값도 이 이미지의 일부이기 때문)
    let root_task = match k0_boot::verify_root_task() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = writeln!(con, "k0: root task rejected: {e:?}");
            park()
        }
    };
    let _ = write!(con, "k0: root task integrity ok ({} bytes, sha256 ", root_task.image.len());
    for b in &root_task.sha256[..4] {
        let _ = write!(con, "{b:02x}");
    }
    let _ = writeln!(con, "..)");

    let hard = k0_arch::hardening::enable();
    let _ = writeln!(
        con,
        "k0: pac={} bti={} pan={}",
        if hard.pac { "on" } else { "absent" },
        if hard.bti { "present" } else { "absent" },
        if hard.pan { "on" } else { "absent" }
    );

    match k0_arch::irq::init(&traps) {
        Ok(_irq) => {
            let _ = writeln!(con, "k0: irq on (timer 1s)");
        }
        Err(e) => {
            let _ = writeln!(con, "k0: irq init failed: {e:?}");
            park()
        }
    }

    //
    // 진입 페이즈 3
    // 케이퍼빌리티 부트스트랩, 루트 태스크 스폰, 이양
    //

    // 부트 프레임 윈도우: 루트 태스크 적재와 사용자 페이지 테이블 전용
    let window = match pick_window(&bootinfo, &kernel_image) {
        Some(w) => w,
        None => {
            let _ = writeln!(con, "k0: no frame window in memory map");
            park()
        }
    };
    if let Err(e) = k0_mm::map_kernel_window(window.clone()) {
        let _ = writeln!(con, "k0: window map failed: {e:?}");
        park()
    }
    let mut frames = match k0_mm::FrameAlloc::new(window.clone()) {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(con, "k0: frame alloc failed: {e:?}");
            park()
        }
    };

    // 세그먼트 메타데이터 변환(W^X는 빌드 시점에 검사 완료)
    let mut seg_buf = [k0_task::LoadSeg {
        va: 0,
        memsz: 0,
        kind: k0_task::SegKind::Rw,
    }; 4];
    if root_task.segments.len() > seg_buf.len() {
        let _ = writeln!(con, "k0: too many root task segments");
        park()
    }
    for (dst, s) in seg_buf.iter_mut().zip(root_task.segments) {
        dst.va = s.va;
        dst.memsz = s.memsz;
        dst.kind = match s.kind {
            k0_boot::RtSegKind::Text => k0_task::SegKind::Text,
            k0_boot::RtSegKind::Ro => k0_task::SegKind::Ro,
            k0_boot::RtSegKind::Rw => k0_task::SegKind::Rw,
        };
    }
    let segs = &seg_buf[..root_task.segments.len()];

    let (tcb, user_root) = match k0_task::spawn_root(
        root_task.image,
        root_task.base,
        root_task.entry,
        segs,
        &mut frames,
    ) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(con, "k0: root task spawn failed: {e:?}");
            park()
        }
    };
    let _ = writeln!(
        con,
        "k0: root task loaded (entry {:#x}, ttbr0 {:#x})",
        root_task.entry, user_root
    );

    // 케이퍼빌리티 부트스트랩: 커널/DTB/윈도우를 뺀 물리 메모리 전부가 untyped
    let mut mem_buf = [k0_cap::PhysRegion { base: 0, size: 0 }; k0_boot::MAX_MEM_REGIONS];
    for (dst, r) in mem_buf.iter_mut().zip(bootinfo.memory()) {
        dst.base = r.base;
        dst.size = r.size;
    }
    let memory = &mem_buf[..bootinfo.memory().len()];
    let reserved = [
        k0_cap::PhysRegion {
            base: kernel_image.start as u64,
            size: (kernel_image.end - kernel_image.start) as u64,
        },
        k0_cap::PhysRegion {
            base: bootinfo.dtb.start as u64,
            size: (bootinfo.dtb.end - bootinfo.dtb.start) as u64,
        },
        k0_cap::PhysRegion {
            base: window.start,
            size: window.end - window.start,
        },
    ];
    let cnode = match k0_cap::bootstrap(memory, &reserved, user_root) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(con, "k0: cap bootstrap failed: {e:?}");
            park()
        }
    };
    let mut untyped_count = 0u32;
    for cap in cnode.slots() {
        if let k0_cap::Cap::Untyped { base, size } = cap {
            untyped_count += 1;
            let _ = writeln!(con, "k0: untyped {base:#x} + {size:#x}");
        }
    }
    let _ = writeln!(
        con,
        "k0: root cnode ready ({} caps, {untyped_count} untyped)",
        cnode.slots().len()
    );

    // TTBR0 교체: 이 순간부터 identity 매핑은 없음
    // SAFETY: 콘솔/GIC/DTB/페이지 테이블 접근이 전부 TTBR1 별칭으로 이행을
    //         마쳤고, user_root는 spawn_root가 완성한 사용자 테이블임
    unsafe { k0_mm::install_user_ttbr0(user_root) };

    check_user_wx(&mut con, segs, &hard);

    let _ = writeln!(con, "k0: entering root task (el0)");
    // SAFETY: 주소 공간 설치와 컨텍스트 준비가 끝난 발산 지점에서의 이양
    unsafe { k0_sched::handoff(tcb) }
}

/// EL0 svc 트랩의 시스템 콜 정책 함수입니다. (k0-arch가 링크 계약으로 호출)
///
/// 미지의 번호는 제로 트러스트 원칙대로 태스크를 정지시킵니다. 다중 태스크가
/// 생기면 정지 범위는 해당 태스크로 좁아집니다.
///
/// # Arguments
/// `ctx` - 사용자 컨텍스트, x8 = 번호, x0 = 인자와 반환값
#[unsafe(no_mangle)]
extern "C" fn k0_syscall(ctx: &mut k0_arch::usermode::Context) {
    let mut con = EarlyCon;
    match ctx.x[8] {
        k0_abi::syscall::DEBUG_PUTC => {
            con.put_byte(ctx.x[0] as u8);
            ctx.x[0] = 0;
        }
        // 단일 태스크라 양보 대상이 없어 즉시 재개
        k0_abi::syscall::YIELD => ctx.x[0] = 0,
        k0_abi::syscall::EXIT => {
            let _ = writeln!(con, "k0: root task exited (code {})", ctx.x[0]);
            park()
        }
        other => {
            let _ = writeln!(con, "k0: unknown syscall {other}");
            park()
        }
    }
}

/// 링커 심볼의 주소를 물리 주소로 환산하는 함수입니다.
///
/// 점프 이후에는 심볼 주소가 higher-half VA로 나오기 때문에 오프셋을 뺍니다.
/// 점프 전에는 그대로 물리 주소이기 때문에 호출하면 안 됩니다.
///
/// # Arguments
/// `sym` - 링커 심볼의 주소
fn pa(sym: *const u8) -> usize {
    sym as usize - k0_mm::KERNEL_VA_OFFSET as usize
}

/// 초기 페이지 테이블을 구성해 MMU를 켜는 함수입니다. (점프 전 구간)
///
/// # Arguments
/// `dtb` - RO로 매핑할 DTB 물리 범위
///
/// # Errors
/// 활성화 실패는 주소 공간을 신뢰할 수 없다는 의미이기 때문에 fail-secure
/// 파킹으로 이어집니다.
fn enable_mmu(con: &mut EarlyCon, dtb: &Range<usize>) {
    // 점프 전이므로 심볼 주소가 곧 물리 주소다
    let text_start = &raw const __kernel_start as u64;
    let rodata_start = &raw const __rodata_start as u64;
    let data_start = &raw const __data_start as u64;
    let stack_bottom = &raw const __boot_stack_bottom as u64;
    let stack_top = &raw const __boot_stack_top as u64;
    let granule = k0_mm::GRANULE as u64;

    let uart = k0_arch::earlycon::MMIO_BASE as u64;
    #[cfg(feature = "plat-virt")]
    let devices = {
        use k0_arch::irq::gic;
        [
            uart..uart + granule,
            gic::GICD_BASE..gic::GICD_BASE + gic::GICD_SIZE,
            gic::GICR_BASE..gic::GICR_BASE + gic::GICR_SIZE,
        ]
    };
    #[cfg(feature = "plat-apple")]
    let devices = [uart..uart + granule, 0..0, 0..0]; // AIC는 필요해질 때 추가 ㄱㄱ

    let layout = k0_mm::KernelLayout {
        text: text_start..rodata_start,
        rodata: rodata_start..data_start,
        // 두 구간 사이의 가드 페이지는 의도적으로 매핑안함
        rw: [data_start..stack_bottom - granule, stack_bottom..stack_top],
        dtb: dtb.start as u64..dtb.end as u64,
        devices,
    };

    match k0_mm::enable_paging(&layout) {
        Ok(_mmu) => {
            con.put_str("k0: mmu on (identity + higher-half alias)\n");
        }
        Err(e) => {
            con.put_str("k0: mmu enable failed, code = ");
            con.put_hex(e as u64);
            con.put_str("\n");
            park()
        }
    }
}

/// 부트 프레임 윈도우로 쓸 물리 구간을 고르는 함수입니다.
///
/// 커널 이미지 끝 바로 뒤(DTB와 겹치면 DTB 끝 뒤)의 그래뉼 정렬 구간을
/// 메모리 맵 안에서 찾습니다. 윈도우는 루트 태스크 적재가 끝나면 더 자라지
/// 않는 고정 예산입니다.
///
/// # Arguments
/// `bootinfo` - 파싱을 마친 부트 정보
/// `kernel_image` - 커널 이미지의 물리 범위
fn pick_window(bootinfo: &k0_boot::BootInfo, kernel_image: &Range<usize>) -> Option<Range<u64>> {
    const WINDOW_SIZE: u64 = 2 * 1024 * 1024;
    let g = k0_mm::GRANULE as u64;
    let dtb_start = bootinfo.dtb.start as u64;
    let dtb_end = bootinfo.dtb.end as u64;

    let mut start = (kernel_image.end as u64).div_ceil(g) * g;
    if start < dtb_end && dtb_start < start.checked_add(WINDOW_SIZE)? {
        start = dtb_end.div_ceil(g) * g;
    }
    let end = start.checked_add(WINDOW_SIZE)?;
    bootinfo
        .memory()
        .iter()
        .any(|r| start >= r.base && end <= r.base + r.size)
        .then_some(start..end)
}

/// 사용자 매핑의 W^X / 가드 / 격리를 AT 명령으로 자가 검증하는 함수입니다.
///
/// `install_user_ttbr0` 이후에 호출해야 합니다. 커널 텍스트가 EL0에서
/// 보이지 않는 것, identity 매핑이 회수된 것, PAN이 켜졌으면 커널(EL1)의
/// 사용자 페이지 접근이 거부되는 것까지 함께 확인합니다. PAN 집행 검사는
/// PSTATE.PAN을 반영하는 AT S1E1RP가 있어야(FEAT_PAN2) 가능하고, 없으면
/// 일반 AT S1E1R로 페이지 권한만 확인합니다.
///
/// # Arguments
/// `segs` - 스폰에 사용한 세그먼트 메타데이터
/// `hard` - hardening 실측 결과(기대값 계산과 검사 방법 선택에 사용)
///
/// # Errors
/// 검증 불일치는 사용자 주소 공간을 신뢰할 수 없다는 의미이기 때문에
/// fail-secure 파킹으로 이어집니다.
fn check_user_wx(con: &mut EarlyCon, segs: &[k0_task::LoadSeg], hard: &k0_arch::hardening::Hardening) {
    let Some(text) = segs
        .iter()
        .find(|s| matches!(s.kind, k0_task::SegKind::Text))
        .map(|s| s.va)
    else {
        let _ = writeln!(con, "k0: user w^x check fail: no text segment");
        park()
    };
    let g = k0_mm::GRANULE as u64;
    let ktext_va = &raw const __kernel_start as u64;
    let stack_top = k0_task::USER_STACK_TOP;
    let guard = stack_top - k0_task::USER_STACK_SIZE - g;

    // PAN 집행 검사: S1E1RP가 있으면 PAN이 특권 접근을 실제로 거부하는지,
    //              없으면 페이지 권한 그대로(EL1은 사용자 RO 페이지를 읽을 수 있음)
    let utext_k = if hard.pan2 {
        ("utext+kp", k0_mm::can_read_pan_checked(text), false)
    } else {
        ("utext+k", k0_mm::can_read(text), true)
    };

    // (이름, 실측, 기대) 형태의 검증표
    let checks: [(&str, bool, bool); 7] = [
        ("utext+r", k0_mm::can_user_read(text), true),
        ("utext+w", k0_mm::can_user_write(text), false),
        ("ustack+w", k0_mm::can_user_write(stack_top - g), true),
        ("uguard+r", k0_mm::can_user_read(guard), false),
        ("ktext+u", k0_mm::can_user_read(ktext_va), false),
        utext_k,
        ("id+r", k0_mm::can_read(ktext_va - k0_mm::KERNEL_VA_OFFSET), false),
    ];
    let mut ok = true;
    for (name, got, want) in checks {
        if got != want {
            ok = false;
            let _ = writeln!(con, "k0: user w^x check fail: {name} (got {got}, want {want})");
        }
    }
    if !ok {
        park()
    }
    let _ = writeln!(con, "k0: user w^x checks pass");
}

/// W^X / 가드 / 양쪽 절반 매핑을 AT 명령으로 자가 검증하는 함수입니다.
///
/// higher-half에서 실행되기 때문에 링커 심볼은 VA로 계산되고, identity 
/// 쪽은 오프셋을 빼서 검사합니다.
///
/// # Errors
/// 검증 불일치는 주소 공간을 신뢰할 수 없다는 의미이기 때문에 fail-secure
/// 파킹으로 이어집니다.
fn check_wx(con: &mut EarlyCon) {
    let text = &raw const __kernel_start as u64;
    let rodata = &raw const __rodata_start as u64;
    let stack_bottom = &raw const __boot_stack_bottom as u64;
    let stack_top = &raw const __boot_stack_top as u64;
    let granule = k0_mm::GRANULE as u64;

    // (이름, 실측, 기대) 형태의 검증표
    let checks: [(&str, bool, bool); 6] = [
        ("text+r", k0_mm::can_read(text), true),
        ("text+w", k0_mm::can_write(text), false),
        ("rodata+w", k0_mm::can_write(rodata), false),
        ("guard+r", k0_mm::can_read(stack_bottom - granule), false),
        ("stack+w", k0_mm::can_write(stack_top - granule), true),
        ("id+r", k0_mm::can_read(text - k0_mm::KERNEL_VA_OFFSET), true),
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
    let _ = writeln!(con, "k0: w^x checks pass (granule {}K, wxn)", k0_mm::GRANULE / 1024);
}
