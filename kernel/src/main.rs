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
    for r in bootinfo.reserved() {
        let _ = writeln!(con, "k0: dtb reserved {:#x} + {:#x}", r.base, r.size);
    }
    check_memory_map(&mut con, &bootinfo);

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

    // PAC 부트 키 파생: DTB 엔트로피 + CNTPCT + RNDR(있으면)을 SHA-256으로 혼합
    let cntpct: u64;
    // SAFETY: CNTPCT_EL0 읽기는 부작용이 없음
    unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) cntpct, options(nomem, nostack)) };
    let rndr = k0_arch::hardening::rndr();
    let pac_keys = k0_boot::derive_pac_keys(bootinfo.entropy(), &[cntpct, rndr.unwrap_or(0)]);
    let _ = writeln!(
        con,
        "k0: entropy dtb={}B rndr={}",
        bootinfo.entropy().len(),
        if rndr.is_some() { "on" } else { "absent" }
    );
    if bootinfo.entropy().is_empty() && rndr.is_none() {
        // 재료가 CNTPCT뿐이면 키가 추측 가능한 수준이라 명시적으로 드러냄
        let _ = writeln!(con, "k0: WARNING pac key entropy low (cntpct only)");
    }

    let hard = k0_arch::hardening::enable(&pac_keys);
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

    // bootinfo 페이지 프레임: 매핑은 스폰이, 내용 기록은 케이퍼빌리티
    // 부트스트랩 이후가 담당
    let bi_frame = match frames.alloc() {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(con, "k0: bootinfo frame alloc failed: {e:?}");
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
        bi_frame,
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
    // 고정 예약(커널/DTB/윈도우) 뒤에 DTB의 펌웨어 예약 구간을 병합
    const FIXED_RSV: usize = 3;
    let mut rsv_buf =
        [k0_cap::PhysRegion { base: 0, size: 0 }; FIXED_RSV + k0_boot::MAX_RSV_REGIONS];
    rsv_buf[0] = k0_cap::PhysRegion {
        base: kernel_image.start as u64,
        size: (kernel_image.end - kernel_image.start) as u64,
    };
    rsv_buf[1] = k0_cap::PhysRegion {
        base: bootinfo.dtb.start as u64,
        size: (bootinfo.dtb.end - bootinfo.dtb.start) as u64,
    };
    rsv_buf[2] = k0_cap::PhysRegion {
        base: window.start,
        size: window.end - window.start,
    };
    for (dst, r) in rsv_buf[FIXED_RSV..].iter_mut().zip(bootinfo.reserved()) {
        dst.base = r.base;
        dst.size = r.size;
    }
    let reserved = &rsv_buf[..FIXED_RSV + bootinfo.reserved().len()];
    let cnode = match k0_cap::bootstrap(memory, reserved, user_root) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(con, "k0: cap bootstrap failed: {e:?}");
            park()
        }
    };
    let mut untyped_count = 0u32;
    for cap in cnode.slots() {
        if let k0_cap::Cap::Untyped { base, size, .. } = cap {
            untyped_count += 1;
            let _ = writeln!(con, "k0: untyped {base:#x} + {size:#x}");
        }
    }
    let _ = writeln!(
        con,
        "k0: root cnode ready ({} caps, {untyped_count} untyped)",
        cnode.slots().len()
    );

    write_bootinfo(bi_frame, cnode);
    let _ = writeln!(con, "k0: bootinfo page ready");

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
/// 미지의 번호는 제로 트러스트 원칙대로 호출한 태스크를 종료시키고, 그게
/// 마지막 태스크였다면 fail-secure 파킹합니다.
///
/// # Arguments
/// `ctx` - 사용자 컨텍스트, x8 = 번호, x0-x5 = 인자, x0(수신 계열은 x1-x5도) = 반환값
#[unsafe(no_mangle)]
extern "C" fn k0_syscall(ctx: &mut k0_arch::usermode::Context) {
    let mut con = EarlyCon;
    match ctx.x[8] {
        k0_abi::syscall::DEBUG_PUTC => ctx.x[0] = sys_putc(ctx.x[0], ctx.x[1]) as u64,
        k0_abi::syscall::YIELD => {
            ctx.x[0] = 0;
            // SAFETY: 벡터가 컨텍스트 저장을 마친 시스템 콜 컨텍스트임
            unsafe { k0_sched::rotate() };
        }
        k0_abi::syscall::EXIT => {
            let _ = writeln!(con, "k0: task exited (code {})", ctx.x[0]);
            kill_current(&mut con)
        }
        k0_abi::syscall::RETYPE => ctx.x[0] = sys_retype(ctx.x[0], ctx.x[1]) as u64,
        k0_abi::syscall::MAP => {
            ctx.x[0] = sys_map(ctx.x[0], ctx.x[1], ctx.x[2], ctx.x[3]) as u64
        }
        k0_abi::syscall::TCB_CONFIGURE => {
            ctx.x[0] = sys_tcb_configure(ctx.x[0], ctx.x[1], ctx.x[2], ctx.x[3]) as u64
        }
        k0_abi::syscall::TCB_RESUME => ctx.x[0] = sys_tcb_resume(ctx.x[0]) as u64,
        // IPC는 블록 경로에서 반환 레지스터를 미래의 상대가 기록하므로
        // 핸들러가 ctx 기록까지 직접 책임지고, false(교착)만 커널이 파킹
        k0_abi::syscall::SEND => ipc_or_park(&mut con, k0_ipc::sys_send(ctx)),
        k0_abi::syscall::RECV => ipc_or_park(&mut con, k0_ipc::sys_recv(ctx)),
        k0_abi::syscall::CALL => ipc_or_park(&mut con, k0_ipc::sys_call(ctx)),
        k0_abi::syscall::REPLY_RECV => ipc_or_park(&mut con, k0_ipc::sys_reply_recv(ctx)),
        other => {
            let _ = writeln!(con, "k0: unknown syscall {other}, killing task");
            kill_current(&mut con)
        }
    }
}

/// EL0 동기 폴트의 격리 정책 함수입니다. (k0-arch가 링크 계약으로 호출)
///
/// 폴트는 그 태스크의 결함이므로 시스템을 세우지 않고 해당 태스크만
/// 종료합니다(격리). 보류한 응답 자격 정리와 마지막 태스크의 fail-secure
/// 파킹은 kill_current가 담당합니다. 폴트를 핸들러 태스크에 IPC로 전달하는
/// 구조는 설계된 확장 지점입니다.
///
/// # Arguments
/// `ctx` - 폴트 시점의 사용자 컨텍스트(ELR = 폴트 명령)
/// `esr` - ESR_EL1 신드롬
/// `far` - FAR_EL1(어보트 계열에서만 의미 있음)
#[unsafe(no_mangle)]
extern "C" fn k0_fault(ctx: &mut k0_arch::usermode::Context, esr: u64, far: u64) {
    let mut con = EarlyCon;
    let ec = k0_arch::vectors::ec_name((esr >> 26) & 0x3F);
    let _ = writeln!(
        con,
        "k0: task fault {ec} (esr {esr:#x} elr {:#x} far {far:#x}), killing task",
        ctx.elr
    );
    kill_current(&mut con)
}

/// 현재 태스크를 종료시키는 함수입니다. 마지막 태스크면 파킹합니다.
///
/// 죽는 태스크가 보류한 응답 자격은 먼저 정리해 호출자가 NO_REPLY로
/// 깨어나게 합니다(영구 블록 방지).
fn kill_current(con: &mut EarlyCon) {
    // SAFETY: 벡터가 컨텍스트 저장을 마친 시스템 콜 컨텍스트임
    unsafe {
        k0_ipc::abort_reply(&mut *k0_sched::current());
        if !k0_sched::exit_current() {
            let _ = writeln!(con, "k0: all tasks exited");
            park()
        }
    }
}

/// DEBUG_PUTC 시스템 콜 처리 함수입니다.
///
/// Console 케이퍼빌리티 제시가 필요합니다(암묵 권한 없음). 출력 가능한
/// ASCII와 개행만 그대로 내보내고 나머지 바이트는 `?`로 바꿔 터미널 제어
/// 문자 주입을 막습니다.
///
/// # Arguments
/// `slot` - Console 슬롯(x0)
/// `byte` - 출력할 바이트(x1, 하위 8비트)
fn sys_putc(slot: u64, byte: u64) -> i64 {
    let Ok(slot) = usize::try_from(slot) else {
        return k0_abi::err::BAD_SLOT;
    };
    // SAFETY: 단일 코어의 시스템 콜 컨텍스트(DAIF 마스크)라 접근이 배타적임
    let cnode = unsafe { k0_cap::root_mut() };
    match cnode.cap(slot) {
        Some(k0_cap::Cap::Console) => {}
        Some(_) => return k0_abi::err::BAD_CAP,
        None => return k0_abi::err::BAD_SLOT,
    }
    let b = byte as u8;
    let safe = if b == b'\n' || (0x20..=0x7E).contains(&b) { b } else { b'?' };
    EarlyCon.put_byte(safe);
    0
}

/// IPC가 교착(전 태스크 블록)을 보고하면 fail-secure 파킹하는 함수입니다.
///
/// 알림 오브젝트가 생기기 전까지는 블록된 태스크를 깨울 외부 주체가
/// 없으므로 전 태스크 블록은 회복 불가능한 교착입니다.
fn ipc_or_park(con: &mut EarlyCon, ok: bool) {
    if !ok {
        let _ = writeln!(con, "k0: all tasks blocked");
        park()
    }
}

/// 타이머 틱의 선점 정책 함수입니다. (k0-arch가 EL0 IRQ 경로에서 링크
/// 계약으로 호출)
#[unsafe(no_mangle)]
extern "C" fn k0_preempt() {
    // SAFETY: 벡터가 사용자 컨텍스트 저장을 마친 EL0 IRQ/FIQ 컨텍스트임
    unsafe { k0_sched::rotate() };
}

/// TCB_CONFIGURE 시스템 콜 처리 함수입니다.
///
/// 진입 컨텍스트는 커널이 강제합니다. SPSR은 항상 EL0t + DAIF 언마스크로
/// 기록되어 사용자에게서 PSTATE를 절대 받지 않습니다(권한 상승 차단). 주소
/// 공간은 AddrSpace 케이퍼빌리티 제시로만 지정할 수 있고, 구성은 Inactive
/// 상태에서 한 번만 허용됩니다(실행 중 재구성 금지).
///
/// # Arguments
/// `slot` - 재분류된 TCB의 슬롯(x0)
/// `entry` - 진입점 VA(x1, 4바이트 정렬)
/// `stack` - 스택 최상단 VA(x2, 16바이트 정렬)
/// `aspace` - AddrSpace 슬롯(x3)
fn sys_tcb_configure(slot: u64, entry: u64, stack: u64, aspace: u64) -> i64 {
    let g = k0_mm::GRANULE as u64;
    // 비정렬 진입점은 첫 명령에서 PC 정렬 폴트가 되므로 구성 시점에 거부
    if entry % 4 != 0 || entry < g || entry >= 1u64 << 48 {
        return k0_abi::err::BAD_VA;
    }
    if stack % 16 != 0 || stack < g || stack > 1u64 << 48 {
        return k0_abi::err::BAD_VA;
    }
    let (Ok(slot), Ok(aspace)) = (usize::try_from(slot), usize::try_from(aspace)) else {
        return k0_abi::err::BAD_SLOT;
    };
    // SAFETY: 단일 코어의 시스템 콜 컨텍스트(DAIF 마스크)라 접근이 배타적임
    let cnode = unsafe { k0_cap::root_mut() };
    let root_pa = match cnode.cap(aspace) {
        Some(k0_cap::Cap::AddrSpace { root_pa }) => root_pa,
        Some(_) => return k0_abi::err::BAD_CAP,
        None => return k0_abi::err::BAD_SLOT,
    };
    let base = match cnode.cap(slot) {
        Some(k0_cap::Cap::Tcb { base }) => base,
        // RootTcb 포함: 실행 중인 자신의 재구성은 불가
        Some(_) => return k0_abi::err::BAD_CAP,
        None => return k0_abi::err::BAD_SLOT,
    };
    // SAFETY: base는 retype가 소거·별칭 매핑한 전용 프레임이고, 소거 상태가
    //         곧 유효한 Inactive TCB임
    let t = unsafe { &mut *((base + k0_mm::KERNEL_VA_OFFSET) as *mut k0_task::Tcb) };
    if t.state != k0_task::TaskState::Inactive {
        return k0_abi::err::BAD_STATE;
    }
    t.ctx = k0_arch::usermode::Context::zeroed();
    t.ctx.elr = entry;
    t.ctx.sp = stack;
    t.ctx.spsr = 0; // EL0t, DAIF 언마스크, 커널이 강제
    t.ttbr0_pa = root_pa;
    t.state = k0_task::TaskState::Stopped;
    0
}

/// TCB_RESUME 시스템 콜 처리 함수입니다.
///
/// 구성(Stopped)을 마친 TCB만 준비 큐에 넣습니다. 이미 큐에 있거나 실행
/// 중이거나 종료된 TCB는 거부되므로 같은 TCB가 큐에 두 번 들어갈 수
/// 없습니다.
///
/// # Arguments
/// `slot` - 재분류된 TCB의 슬롯(x0)
fn sys_tcb_resume(slot: u64) -> i64 {
    let Ok(slot) = usize::try_from(slot) else {
        return k0_abi::err::BAD_SLOT;
    };
    // SAFETY: 단일 코어의 시스템 콜 컨텍스트(DAIF 마스크)라 접근이 배타적임
    let cnode = unsafe { k0_cap::root_mut() };
    let base = match cnode.cap(slot) {
        Some(k0_cap::Cap::Tcb { base }) => base,
        Some(_) => return k0_abi::err::BAD_CAP,
        None => return k0_abi::err::BAD_SLOT,
    };
    let t = (base + k0_mm::KERNEL_VA_OFFSET) as *mut k0_task::Tcb;
    // SAFETY: t는 retype가 만든 전용 TCB 프레임의 별칭이고, Stopped 검사로
    //         큐 중복 진입이 차단됨
    unsafe {
        if (*t).state != k0_task::TaskState::Stopped {
            return k0_abi::err::BAD_STATE;
        }
        k0_sched::enqueue(t);
    }
    0
}

/// RETYPE 시스템 콜 처리 함수입니다.
///
/// untyped에서 오브젝트를 잘라내기 전에 해당 프레임을 TTBR1 별칭으로
/// 매핑하고 소거합니다(이전 내용 누설 차단). 준비가 실패하면 케이퍼빌리티
/// 상태는 변하지 않습니다.
///
/// # Arguments
/// `slot` - untyped 슬롯(x0)
/// `kind` - 오브젝트 타입(x1)
fn sys_retype(slot: u64, kind: u64) -> i64 {
    let kind = match kind {
        k0_abi::obj::FRAME => k0_cap::ObjKind::Frame,
        k0_abi::obj::PAGE_TABLE => k0_cap::ObjKind::PageTable,
        k0_abi::obj::TCB => k0_cap::ObjKind::Tcb,
        k0_abi::obj::ENDPOINT => k0_cap::ObjKind::Endpoint,
        _ => return k0_abi::err::BAD_TYPE,
    };
    let Ok(slot) = usize::try_from(slot) else {
        return k0_abi::err::BAD_SLOT;
    };
    let g = k0_mm::GRANULE as u64;
    // SAFETY: 단일 코어의 시스템 콜 컨텍스트(DAIF 마스크)라 접근이 배타적임
    let cnode = unsafe { k0_cap::root_mut() };
    let r = cnode.retype(slot, kind, g, |pa| {
        if k0_mm::map_kernel_window(pa..pa + g).is_err() {
            return false;
        }
        // SAFETY: 방금 별칭 매핑을 마친 커널 전용 구간이며, 소거는 이전
        //         내용의 누설을 차단함
        unsafe {
            core::ptr::write_bytes(
                (pa + k0_mm::KERNEL_VA_OFFSET) as *mut u8,
                0,
                k0_mm::GRANULE,
            );
        }
        true
    });
    match r {
        Ok(new_slot) => new_slot as i64,
        Err(k0_cap::RetypeError::BadSlot) => k0_abi::err::BAD_SLOT,
        Err(k0_cap::RetypeError::NotUntyped) => k0_abi::err::NOT_UNTYPED,
        Err(k0_cap::RetypeError::Exhausted) => k0_abi::err::EXHAUSTED,
        Err(k0_cap::RetypeError::OutOfSlots) => k0_abi::err::OUT_OF_SLOTS,
        Err(k0_cap::RetypeError::PrepFailed) => k0_abi::err::KERNEL_RESOURCE,
    }
}

/// MAP 시스템 콜 처리 함수입니다.
///
/// 케이퍼빌리티 종류가 동작을 결정합니다. Frame은 리프 매핑(RO/RW만, 실행
/// 매핑은 만들 수 없음), PageTable은 경로의 첫 빈 레벨에 설치입니다. 대상
/// 주소 공간은 제시한 AddrSpace 케이퍼빌리티로만 정해지며 현재 설치된
/// TTBR0는 참조하지 않습니다(암묵 권한 배제). 현재 공간이 아닌 테이블에
/// 새 항목을 추가하는 것도 무효 항목의 유효화라 TLB 무효화가 필요 없습니다.
///
/// # Arguments
/// `slot` - 케이퍼빌리티 슬롯(x0)
/// `va` - 사용자 VA(x1)
/// `perm` - Frame 권한(x2, PageTable은 무시)
/// `aspace` - 대상 AddrSpace 슬롯(x3)
fn sys_map(slot: u64, va: u64, perm: u64, aspace: u64) -> i64 {
    let (Ok(slot), Ok(aspace)) = (usize::try_from(slot), usize::try_from(aspace)) else {
        return k0_abi::err::BAD_SLOT;
    };
    // SAFETY: 단일 코어의 시스템 콜 컨텍스트(DAIF 마스크)라 접근이 배타적임
    let cnode = unsafe { k0_cap::root_mut() };
    let root = match cnode.cap(aspace) {
        Some(k0_cap::Cap::AddrSpace { root_pa }) => root_pa,
        Some(_) => return k0_abi::err::BAD_CAP,
        None => return k0_abi::err::BAD_SLOT,
    };
    match cnode.cap_mut(slot) {
        Some(k0_cap::Cap::Frame { base, mapped }) => {
            if *mapped {
                return k0_abi::err::ALREADY_MAPPED;
            }
            let perm = match perm {
                k0_abi::perm::RO => k0_mm::UserPerm::RoUser,
                k0_abi::perm::RW => k0_mm::UserPerm::RwUser,
                _ => return k0_abi::err::BAD_PERM,
            };
            // SAFETY: root는 커널이 설치한 사용자 루트고, 걷게 되는 테이블은
            //         전부 부트 윈도우 또는 재분류된(별칭 매핑된) 프레임임
            match unsafe { k0_mm::user_map_frame(root, va, *base, perm) } {
                Ok(()) => {
                    *mapped = true;
                    0
                }
                Err(e) => mmu_err(e),
            }
        }
        Some(k0_cap::Cap::PageTable { base, installed }) => {
            if *installed {
                return k0_abi::err::ALREADY_MAPPED;
            }
            // SAFETY: 위와 동일하고 base는 retype이 소거한 전용 프레임임
            match unsafe { k0_mm::user_install_table(root, va, *base) } {
                Ok(_) => {
                    *installed = true;
                    0
                }
                Err(e) => mmu_err(e),
            }
        }
        Some(_) => k0_abi::err::BAD_CAP,
        None => k0_abi::err::BAD_SLOT,
    }
}

/// 런타임 매핑 경로의 MmuError를 ABI 에러 코드로 바꾸는 함수입니다.
///
/// # Errors
/// `BadTable`은 페이지 테이블 손상을 의미하므로 fail-secure 파킹합니다
fn mmu_err(e: k0_mm::MmuError) -> i64 {
    match e {
        k0_mm::MmuError::Misaligned | k0_mm::MmuError::NullPage => k0_abi::err::BAD_VA,
        k0_mm::MmuError::Overlap => k0_abi::err::OVERLAP,
        k0_mm::MmuError::MissingTable => k0_abi::err::MISSING_TABLE,
        k0_mm::MmuError::BadTable => {
            let mut con = EarlyCon;
            let _ = writeln!(con, "k0: user page table corrupt");
            park()
        }
        _ => k0_abi::err::KERNEL_RESOURCE,
    }
}

/// 케이퍼빌리티 목록을 bootinfo 페이지에 기록하는 함수입니다.
///
/// 페이지는 EL0에 RO로 매핑돼 있고 커널은 TTBR1 별칭으로 씁니다. untyped만
/// base/size를 노출하고 나머지 종류는 커널 내부 주소를 숨깁니다(0).
///
/// # Arguments
/// `frame_pa` - bootinfo 페이지 프레임의 PA(부트 윈도우에서 할당)
/// `cnode` - 부트스트랩을 마친 루트 CNode
fn write_bootinfo(frame_pa: u64, cnode: &k0_cap::CNode) {
    use k0_abi::bootinfo::{cap_kind, CapDesc, Header, VERSION};

    // 슬롯 전수가 페이지 하나에 들어가는지 컴파일 시점에 강제
    const _: () = assert!(
        size_of::<Header>() + k0_cap::CNODE_SLOTS * size_of::<CapDesc>() <= k0_mm::GRANULE
    );

    let hdr = (frame_pa + k0_mm::KERNEL_VA_OFFSET) as *mut Header;
    // SAFETY: 프레임은 부트 윈도우에서 할당돼 별칭 매핑이 있고, EL0에는 RO라
    //         이 기록 경로가 유일한 쓰기임
    unsafe {
        hdr.write(Header {
            version: VERSION,
            frame_size: k0_mm::GRANULE as u64,
            cap_count: cnode.slots().len() as u64,
        });
        let descs = hdr.add(1) as *mut CapDesc;
        for (i, cap) in cnode.slots().iter().enumerate() {
            let d = match cap {
                k0_cap::Cap::Empty => CapDesc {
                    kind: cap_kind::EMPTY,
                    base: 0,
                    size: 0,
                },
                k0_cap::Cap::RootTcb | k0_cap::Cap::Tcb { .. } => CapDesc {
                    kind: cap_kind::TCB,
                    base: 0,
                    size: 0,
                },
                k0_cap::Cap::AddrSpace { .. } => CapDesc {
                    kind: cap_kind::ADDR_SPACE,
                    base: 0,
                    size: 0,
                },
                k0_cap::Cap::Console => CapDesc {
                    kind: cap_kind::CONSOLE,
                    base: 0,
                    size: 0,
                },
                k0_cap::Cap::Untyped { base, size, .. } => CapDesc {
                    kind: cap_kind::UNTYPED,
                    base: *base,
                    size: *size,
                },
                k0_cap::Cap::Frame { .. } => CapDesc {
                    kind: cap_kind::FRAME,
                    base: 0,
                    size: 0,
                },
                k0_cap::Cap::PageTable { .. } => CapDesc {
                    kind: cap_kind::PAGE_TABLE,
                    base: 0,
                    size: 0,
                },
                k0_cap::Cap::Endpoint { .. } => CapDesc {
                    kind: cap_kind::ENDPOINT,
                    base: 0,
                    size: 0,
                },
            };
            descs.add(i).write(d);
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

    let layout = k0_mm::KernelLayout {
        text: text_start..rodata_start,
        rodata: rodata_start..data_start,
        // 두 구간 사이의 가드 페이지는 의도적으로 매핑안함
        rw: [data_start..stack_bottom - granule, stack_bottom..stack_top],
        dtb: dtb.start as u64..dtb.end as u64,
        devices: device_windows(),
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

/// 커널이 디바이스로 매핑하는 MMIO 창(플랫폼 고정 주소)을 주는 함수입니다.
///
/// 진입 페이즈 1의 디바이스 매핑과 DTB 메모리 맵 검증이 같은 목록을 씁니다.
/// 빈 범위(start == end)는 자리 표시자입니다.
fn device_windows() -> [Range<u64>; 3] {
    let uart = k0_arch::earlycon::MMIO_BASE as u64;
    let granule = k0_mm::GRANULE as u64;
    #[cfg(feature = "plat-virt")]
    {
        use k0_arch::irq::gic;
        [
            uart..uart + granule,
            gic::GICD_BASE..gic::GICD_BASE + gic::GICD_SIZE,
            gic::GICR_BASE..gic::GICR_BASE + gic::GICR_SIZE,
        ]
    }
    #[cfg(feature = "plat-apple")]
    [uart..uart + granule, 0..0, 0..0] // AIC는 필요해질 때 추가 ㄱㄱ
}

/// DTB 메모리 맵이 커널의 MMIO 창과 겹치지 않는지 검증하는 함수입니다.
///
/// 메모리 노드는 그대로 untyped가 되어 EL0에 매핑될 수 있으므로, MMIO를
/// 메모리라고 주장하는 DTB는 사용자 공간에 디바이스 접근을 열어 주는
/// 권한 확대입니다. 제로 트러스트 원칙대로 자원 축소가 아닌 확대는
/// 거부합니다.
///
/// # Errors
/// 겹침이 있으면 부트 정보를 신뢰할 수 없으므로 fail-secure 파킹합니다
fn check_memory_map(con: &mut EarlyCon, bootinfo: &k0_boot::BootInfo) {
    for dev in device_windows() {
        if dev.end <= dev.start {
            continue;
        }
        for m in bootinfo.memory() {
            // 파서가 base + size 오버플로를 이미 거부함
            if m.base < dev.end && dev.start < m.base + m.size {
                let _ = writeln!(
                    con,
                    "k0: dtb memory {:#x} + {:#x} overlaps device window {:#x}",
                    m.base, m.size, dev.start
                );
                park()
            }
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
    let align_up = |v: u64| v.checked_add(g - 1).map(|x| x & !(g - 1));
    let dtb = k0_boot::MemRegion {
        base: bootinfo.dtb.start as u64,
        size: (bootinfo.dtb.end - bootinfo.dtb.start) as u64,
    };

    let mut start = align_up(kernel_image.end as u64)?;
    // 장애물(DTB, 펌웨어 예약)을 넘어가며 안정될 때까지 전진, 반복은 유한
    for _ in 0..64 {
        let end = start.checked_add(WINDOW_SIZE)?;
        let mut bumped = false;
        for r in core::iter::once(&dtb).chain(bootinfo.reserved()) {
            // 파서가 base + size 오버플로를 이미 거부함
            let r_end = r.base.checked_add(r.size)?;
            if r_end > start && r.base < end {
                start = start.max(align_up(r_end)?);
                bumped = true;
            }
        }
        if bumped {
            continue;
        }
        if bootinfo
            .memory()
            .iter()
            .any(|m| start >= m.base && end <= m.base + m.size)
        {
            return Some(start..end);
        }
        // 어느 메모리 구간에도 안 들어가면 다음 구간의 시작으로 전진
        let next = bootinfo
            .memory()
            .iter()
            .map(|m| m.base)
            .filter(|&b| b > start)
            .min()?;
        start = align_up(next)?;
    }
    None
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
    let checks: [(&str, bool, bool); 9] = [
        ("utext+r", k0_mm::can_user_read(text), true),
        ("utext+w", k0_mm::can_user_write(text), false),
        ("ustack+w", k0_mm::can_user_write(stack_top - g), true),
        ("uguard+r", k0_mm::can_user_read(guard), false),
        ("ktext+u", k0_mm::can_user_read(ktext_va), false),
        ("binfo+r", k0_mm::can_user_read(k0_abi::bootinfo::VA), true),
        ("binfo+w", k0_mm::can_user_write(k0_abi::bootinfo::VA), false),
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
