//! K0 커널 바이너리 크레이트입니다.
//!
//! 이 크레이트는 의도적으로 얇게 설계되었습니다. 이 크레이트는 진입 페이즈 0 어셈블리 포함,
//! 초기화 시퀀스의 순서 강제, 시스템 콜 정책(`syscall`), 오브젝트 종료 처리(`object`),
//! fail-secure panic 핸들러만 담습니다. 그 밖의 로직은 `crates/` 아래의 라이브러리
//! 크레이트(k0-*)에 위치해 있습니다.
//!
//! 초기화는 higher-half 점프를 경계로 두 단으로 나뉩니다. 커널은 고정 가상 주소에
//! 링크되지만 부트로더가 고른 임의의 물리 주소에 적재되기 때문에, 점프 전
//! 구간(`kernel_init`)에는 제약이 하나 있습니다. 주소를 만드는 코드(adr / adrp)는 PC 상대라 물리 주소가 맞게
//! 나오지만, 포인터 값을 담고 있는 데이터(fat 포인터 static, vtable, 이중
//! 참조 리터럴 등)는 링크된 절대 VA로 재배치되어 있어 점프 전에 역참조하면
//! 주소 크기 폴트가 납니다. 그래서 점프 전에는 문자열 리터럴 출력과 정수
//! 연산만 쓰고, DTB 파싱/루트 태스크 검증/`writeln!`은 전부 점프
//! 이후(`kernel_main`)로 미룹니다.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ops::Range;
use core::sync::atomic::{AtomicU64, Ordering};

use k0_arch::earlycon::EarlyCon;

mod boot; // arch/aarch64/boot.S를 global_asm! 으로 포함
mod object; // 케이퍼빌리티 제거에 따른 오브젝트 종료 처리
mod panic; // fail-secure panic 핸들러와 파킹 루프
mod syscall; // 시스템 콜과 EL0 폴트의 커널 정책

use panic::park;

// 링커 스크립트가 export하는 심볼들
unsafe extern "C" {
    static __kernel_start: u8;
    static __kernel_end: u8;
    static __rodata_start: u8;
    static __data_start: u8;
    static __bss_start: u8;
    static __boot_stack_bottom: u8;
    static __boot_stack_top: u8;
}

/// 진입 시점의 예외 레벨(1 또는 2)입니다. `boot.S`가 .bss 소거 뒤에 기록합니다.
#[unsafe(no_mangle)]
static K0_ENTRY_EL: AtomicU64 = AtomicU64::new(0);

/// EL2 벡터가 남기는 기록(ESR_EL2, ELR_EL2, FAR_EL2)입니다. 강하 뒤에 EL2로
/// 들어온 일이 있었다면 그 원인이 여기 남고 코어는 정지해 있습니다.
#[unsafe(no_mangle)]
static K0_EL2_TRAP: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// 진입 페이즈 0(`boot.S`)에서 점프해 들어오는 유일한 Rust 진입점입니다.
///
/// 진입 조건은 다음과 같습니다. (`boot.S`가 보장)
/// - EL1h, DAIF 전부 마스크, MMU/캐시 OFF
/// - `sp` = `__boot_stack_top`, `.bss` 소거(zeroize) 완료
/// - `dtb_phys` = 부트로더가 전달한 DTB 물리 주소
///
/// 이 함수는 점프 전 구간이기 때문에 재배치된 데이터를 건드리지 않습니다.
/// DTB도 읽지 않습니다. 부트로더가 DTB를 캐시에만 써 두었을 수 있어, MMU와
/// 캐시를 켠 뒤에 매핑을 통해서만 읽습니다. 콘솔을 아직 모르므로 여기서의
/// 출력은 부트 로그에만 쌓였다가 콘솔이 붙을 때 재생됩니다.
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

    let load_pa = enable_mmu(&mut con);
    con.put_str("k0: jumping to higher half\n");

    // SAFETY: enable_mmu가 TTBR1의 고정 주소에 이미지를 매핑했고 kernel_main의
    //         링크 주소가 그 안에 있음. SP도 같은 차이만큼 옮겨 이미지 매핑 위로
    //         올리고, x0으로 DTB 물리 주소를 그대로 넘김 (extern "C" 첫 인자).
    //         입력을 x9로 고정해 분기 주소를 만드는 x8과 겹치지 않게 함
    unsafe {
        core::arch::asm!(
            "add sp, sp, x9",
            "movz x8, #:abs_g3:kernel_main",
            "movk x8, #:abs_g2_nc:kernel_main",
            "movk x8, #:abs_g1_nc:kernel_main",
            "movk x8, #:abs_g0_nc:kernel_main",
            "br x8",
            in("x9") k0_mm::KERNEL_IMAGE_VA.wrapping_sub(load_pa),
            in("x0") dtb_phys,
            options(noreturn),
        )
    }
}

/// higher-half로 점프한 뒤의 초기화 후반부 함수입니다.
///
/// PC와 SP가 이미지의 고정 매핑 위에 있으므로 여기서부터 재배치된 데이터가
/// 유효해져 전체 Rust를 제약 없이 쓸 수 있습니다. 점프가 typestate 토큰 전달을
/// 끊으므로 초기화 순서는 이 함수의 호출 순서로 강제합니다.
///
/// # Arguments
/// `dtb_phys` - `kernel_init`이 x0으로 넘긴 DTB 물리 주소
#[unsafe(no_mangle)]
extern "C" fn kernel_main(dtb_phys: usize) -> ! {
    // 벡터 재설치: 같은 테이블이지만 이제 PC가 higher-half라 VBAR도 VA가 됨
    let traps = k0_arch::vectors::install();

    let mut con = EarlyCon;
    let pc: u64;
    // SAFETY: 현재 PC 근처 주소를 읽기만 함
    unsafe { core::arch::asm!("adr {}, .", out(reg) pc, options(nomem, nostack)) };
    let _ = writeln!(con, "k0: higher half (pc = {pc:#x})");

    check_wx(&mut con);

    // DTB는 여기서 처음 읽음. 별칭 매핑으로 읽고 결과의 메모리 맵은 물리 주소 기준
    let image = k0_mm::image_range();
    let kernel_image = image.start as usize..image.end as usize;
    map_dtb(&mut con, dtb_phys);
    let mut bootinfo = match k0_boot::parse(
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

    // 디바이스 발견, 메모리 맵과의 대조, 고정 모드 대조, 콘솔 연결
    let (platform, counter_hz) = bring_up_platform(&mut con, dtb_phys, &bootinfo, &image);

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

    // PAC 부트 키 파생: DTB 엔트로피 + 가상 카운터 + RNDR(있으면)을 SHA-256으로 혼합
    let cntpct: u64;
    // SAFETY: CNTVCT_EL0 읽기는 부작용이 없음
    unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) cntpct, options(nomem, nostack)) };
    let rndr = k0_arch::hardening::rndr();
    let mut pac_keys = k0_boot::derive_pac_keys(bootinfo.entropy(), &[cntpct, rndr.unwrap_or(0)]);
    let _ = writeln!(
        con,
        "k0: entropy dtb={}B rndr={}",
        bootinfo.entropy().len(),
        if rndr.is_some() { "on" } else { "absent" }
    );
    if bootinfo.entropy().is_empty() && rndr.is_none() {
        // 재료가 카운터뿐이면 키가 추측 가능한 수준이라 명시적으로 드러냄
        let _ = writeln!(con, "k0: WARNING pac key entropy low (counter only)");
    }

    // 루트 태스크에 넘길 시드. PAC 키와 같은 DTB 재료를 쓰지만 도메인이 달라
    // 서로 독립이고, RNDR이 있으면 여기에만 쓰는 256비트를 따로 뽑아 넣음
    let mut fresh = [0u64; 4];
    let mut rndr_full = true;
    for w in fresh.iter_mut() {
        match k0_arch::hardening::rndr() {
            Some(v) => *w = v,
            None => rndr_full = false,
        }
    }
    let mut root_seed =
        k0_boot::derive_root_seed(bootinfo.entropy(), &[cntpct, fresh[0], fresh[1], fresh[2], fresh[3]]);
    let seed_info = (bootinfo.rng_seed_len() as u64 & k0_abi::bootinfo::SEED_RNG_SEED_BYTES)
        | if rndr_full { k0_abi::bootinfo::SEED_RNDR } else { 0 };
    for w in fresh.iter_mut() {
        // SAFETY: w는 이 스택 프레임의 유효한 배열 원소임
        unsafe { core::ptr::write_volatile(w, 0) };
    }

    let hard = k0_arch::hardening::enable(&pac_keys);
    // 키는 이제 시스템 레지스터에만 있어야 함. 부트 스택은 이후 시스템 콜
    // 스택으로 재사용되므로 사본을 남기지 않음 (volatile이라 소거가 최적화로
    // 사라지지 않음)
    for k in pac_keys.iter_mut() {
        // SAFETY: k는 이 스택 프레임의 유효한 배열 원소임
        unsafe { core::ptr::write_volatile(k, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    let _ = writeln!(
        con,
        "k0: pac={} bti={} pan={}",
        if hard.pac { "on" } else { "absent" },
        if hard.bti { "present" } else { "absent" },
        if hard.pan { "on" } else { "absent" }
    );

    match setup_irq(&traps, &platform, counter_hz) {
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
    // 윈도우 탐색은 k0-boot에 있어 UEFI 로더가 같은 함수로 미리 확인함
    let window = match k0_boot::boot_window(
        bootinfo.memory(),
        bootinfo.reserved(),
        &(kernel_image.start as u64..kernel_image.end as u64),
        &(bootinfo.dtb.start as u64..bootinfo.dtb.end as u64),
        k0_mm::GRANULE as u64,
    ) {
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
    let (pool_used, pool_len) = k0_mm::table_pool_usage();
    let _ = writeln!(con, "k0: kernel table pool {pool_used}/{pool_len}");
    let mut frames = match k0_mm::FrameAlloc::new(window.clone()) {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(con, "k0: frame alloc failed: {e:?}");
            park()
        }
    };

    // bootinfo 페이지, 루트 TCB, 루트 CNode의 프레임. 전부 부트 윈도우에서
    // 나오므로 소거돼 있고(소거 상태가 곧 유효한 초기 오브젝트) untyped에
    // 속하지 않음
    let g = k0_mm::GRANULE as u64;
    let cnode_frames = (k0_cap::ROOT_CNODE_SLOTS * k0_cap::SLOT_SIZE).div_ceil(k0_mm::GRANULE) as u64;
    let (bi_frame, tcb_frame, cnode_frame) =
        match (frames.alloc(), frames.alloc(), frames.alloc_contig(cnode_frames)) {
            (Ok(b), Ok(t), Ok(c)) => (b, t, c),
            _ => {
                let _ = writeln!(con, "k0: boot object frame alloc failed");
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

    let root = match k0_task::spawn_root(
        root_task.image,
        root_task.base,
        root_task.entry,
        segs,
        bi_frame,
        tcb_frame,
        &mut frames,
    ) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(con, "k0: root task spawn failed: {e:?}");
            park()
        }
    };
    let user_root = root.root_pa;
    let _ = writeln!(con, "k0: root task loaded (entry {:#x})", root_task.entry);

    // 루트 주소 공간을 등록부에 올림. 이후 매핑 기록은 (asid, 세대)로 이
    // 주소 공간을 가리킴. 루트 테이블이 untyped 밖(부트 윈도우)에 있어서
    // 등록부의 전용 항목을 씀
    // SAFETY: 단일 부트 코어의 초기화 시퀀스라 등록부 접근이 배타적임
    let Some(root_asid) = (unsafe { k0_mm::addr_spaces() }).register_boot(user_root) else {
        let _ = writeln!(con, "k0: address space registry failed");
        park()
    };

    // 케이퍼빌리티 부트스트랩: 커널/DTB/윈도우를 뺀 물리 메모리가 untyped가 됨.
    // 다만 구간마다 꼬리에서 커널 몫을 먼저 떼어 둠(reserve_kernel_share)
    let mut mem_buf = [k0_cap::PhysRegion { base: 0, size: 0 }; k0_boot::MAX_MEM_REGIONS];
    for (dst, r) in mem_buf.iter_mut().zip(bootinfo.memory()) {
        dst.base = r.base;
        dst.size = r.size;
    }
    let memory = &mem_buf[..bootinfo.memory().len()];
    // 고정 예약(커널/DTB/윈도우/물리 주소 0의 그래뉼) 뒤에 DTB의 펌웨어 예약
    // 구간을 병합. 물리 주소 0은 커널 안에서 "없음"을 뜻하는 값과 같아서, RAM이
    // 0에서 시작하는 보드에서도 그 그래뉼은 오브젝트로 내주지 않음
    const FIXED_RSV: usize = 4;
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
    rsv_buf[3] = k0_cap::PhysRegion {
        base: 0,
        size: k0_mm::GRANULE as u64,
    };
    for (dst, r) in rsv_buf[FIXED_RSV..].iter_mut().zip(bootinfo.reserved()) {
        dst.base = r.base;
        dst.size = r.size;
    }
    let reserved = &rsv_buf[..FIXED_RSV + bootinfo.reserved().len()];
    let mut ut_buf = [k0_cap::PhysRegion { base: 0, size: 0 }; k0_mm::MAX_TABLE_RESERVES];
    let untyped = match reserve_kernel_share(&mut con, memory, reserved, &mut ut_buf) {
        Ok(n) => &ut_buf[..n],
        Err(e) => {
            let _ = writeln!(con, "k0: cap bootstrap failed: {e:?}");
            park()
        }
    };

    // SAFETY: cnode_frame은 방금 부트 윈도우에서 할당한 소거된 연속 프레임으로
    //         ROOT_CNODE_SLOTS개의 슬롯을 담고, 단일 부트 코어라 접근이 배타적임
    let cnode = unsafe {
        k0_cap::CNodeRef::new(
            (cnode_frame + k0_mm::KERNEL_VA_OFFSET) as *mut k0_cap::Slot,
            k0_cap::ROOT_CNODE_SLOTS,
        )
    };
    let objs = k0_cap::BootObjects {
        tcb: tcb_frame,
        addr_space_root: user_root,
        addr_space_asid: root_asid,
        cnode: cnode_frame,
    };
    // 예약 구간은 위에서 이미 뺐으므로 남은 구간이 그대로 untyped가 됨
    // SAFETY: cnode의 슬롯은 전부 비어 있고 접근이 배타적임
    let mut next = match unsafe { k0_cap::bootstrap(&cnode, &objs, untyped, &[]) } {
        Ok(n) => n,
        Err(e) => {
            let _ = writeln!(con, "k0: cap bootstrap failed: {e:?}");
            park()
        }
    };
    let untyped_count = next - k0_cap::boot_slot::FIRST_DYNAMIC;
    for i in k0_cap::boot_slot::FIRST_DYNAMIC..next {
        // SAFETY: 방금 부트스트랩이 채운 범위 안의 슬롯임
        if let Some(Ok(Some(k0_cap::Entry {
            cap: k0_cap::Cap::Untyped { base, size, .. },
            ..
        }))) = cnode.slot(i).map(|s| unsafe { k0_cap::read(s) })
        {
            let _ = writeln!(con, "k0: untyped {base:#x} + {size:#x}");
        }
    }

    // 루트 태스크 이미지의 프레임을 Frame 케이퍼빌리티로 넘김. 권한은 세그먼트
    // 종류로 고정됨: 텍스트는 R|X(쓰기 불가), 읽기 전용 데이터는 R, 데이터는
    // R|W(실행 불가). 다른 주소 공간에 실행 가능하게 매핑할 수 있는 프레임은
    // 이 텍스트 프레임뿐이고 그 내용은 부팅 시 해시 검증을 통과한 이미지임.
    // 루트 자신의 부트 매핑은 케이퍼빌리티로 표현되지 않고 영구적임
    let image_first = next;
    for s in segs {
        let rights = match s.kind {
            k0_task::SegKind::Text => k0_cap::rights::R | k0_cap::rights::X,
            k0_task::SegKind::Ro => k0_cap::rights::R,
            k0_task::SegKind::Rw => k0_cap::rights::R | k0_cap::rights::W,
        };
        let mut off = 0;
        while off < s.memsz {
            let cap = k0_cap::Cap::Frame {
                base: root.image_pa + (s.va - root_task.base) + off,
                rights,
                map: None,
            };
            // SAFETY: cnode의 남은 슬롯은 비어 있고 접근이 배타적임
            if let Err(e) = unsafe { k0_cap::push_root(&cnode, &mut next, cap) } {
                let _ = writeln!(con, "k0: image cap bootstrap failed: {e:?}");
                park()
            }
            off += g;
        }
    }

    // 루트 태스크를 자기 CNode와 주소 공간에 묶음. 다른 태스크와 똑같이 TCB의
    // 내장 슬롯에 두 케이퍼빌리티의 복사본을 넣음
    let tcb = root.tcb;
    // 시드는 루트 태스크의 진입 레지스터로 넘김(x0-x3 = 시드, x4 = 재료 정보).
    // 저장된 컨텍스트의 사본은 루트 태스크가 처음 커널에 들어올 때 그때의
    // 레지스터로 덮이고, 이 스택의 사본은 여기서 지움
    for (i, chunk) in root_seed.chunks_exact(8).enumerate() {
        tcb.ctx.x[i] = u64::from_le_bytes(chunk.try_into().unwrap_or([0; 8]));
    }
    tcb.ctx.x[4] = seed_info;
    for b in root_seed.iter_mut() {
        // SAFETY: b는 이 스택 프레임의 유효한 배열 원소임
        unsafe { core::ptr::write_volatile(b, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    let tcb_ptr = core::ptr::from_mut(tcb);
    let bound = match (
        cnode.slot(k0_cap::boot_slot::CNODE),
        cnode.slot(k0_cap::boot_slot::ADDR_SPACE),
    ) {
        // SAFETY: 네 슬롯 모두 유효하고(루트 CNode와 루트 TCB 프레임) 단일 부트
        //         코어라 접근이 배타적임
        (Some(c), Some(a)) => unsafe {
            k0_cap::copy_pair_bound(
                c,
                k0_task::Tcb::slot(tcb_ptr, k0_task::TCB_SLOT_CSPACE),
                a,
                k0_task::Tcb::slot(tcb_ptr, k0_task::TCB_SLOT_VSPACE),
            )
        },
        _ => Err(k0_cap::CapError::OutOfSlots),
    };
    if let Err(e) = bound {
        let _ = writeln!(con, "k0: root task bind failed: {e:?}");
        park()
    }
    let _ = writeln!(
        con,
        "k0: root cnode ready ({} caps, {untyped_count} untyped, {} image frames, {} slots)",
        next - 1,
        next - image_first,
        cnode.len()
    );

    let image = ImageMap {
        pa: root.image_pa,
        va: root_task.base,
        len: root_task.image.len() as u64,
    };
    if !write_bootinfo(bi_frame, &cnode, next, &image) {
        let _ = writeln!(con, "k0: bootinfo page overflow");
        park()
    }
    let _ = writeln!(con, "k0: bootinfo page ready");

    // TTBR0 교체: 이 순간부터 identity 매핑은 없음
    // SAFETY: 콘솔/GIC/DTB/페이지 테이블 접근이 전부 TTBR1 별칭으로 이행을
    //         마쳤고, user_root는 spawn_root가 완성한 사용자 테이블임
    unsafe { k0_mm::install_user_ttbr0(user_root) };

    check_user_wx(&mut con, segs, &hard);

    // DTB를 읽는 일이 전부 끝났으므로 거기 남은 시드와 그 사본을 지움
    wipe_dtb_seeds(&mut con, &mut bootinfo);

    print_device_pages(&mut con);
    let _ = writeln!(con, "k0: entering root task (el0)");
    scrub_boot_stack();
    // SAFETY: 주소 공간 설치와 컨텍스트 준비가 끝난 발산 지점에서의 이양이고,
    //         tcb_ptr은 부트 윈도우의 루트 TCB 프레임으로 이후 해제되지 않음
    unsafe { k0_sched::handoff(&mut *tcb_ptr) }
}

/// 루트 태스크 이미지가 놓인 물리 구간과 그 사용자 VA를 담는 구조체입니다.
struct ImageMap {
    pa: u64,
    va: u64,
    len: u64,
}

/// DTB에 남은 시드 값과 커널이 가진 그 사본을 지우는 함수입니다.
///
/// 파서가 기록한 구간(`rng-seed`와 `kaslr-seed`의 값 전체)을 0으로 덮고, 읽기
/// 전용 별칭으로 다시 읽어 확인한 뒤에만 지웠다고 알립니다. 시드로 PAC 키와
/// 루트 태스크의 시드를 이미 만든 뒤이므로, 남겨 두면 나중에 그 메모리를 읽는
/// 쪽이 둘을 다시 계산할 수 있습니다. 구간은 파서가 blob 안이라고 확인한
/// 것이고 DTB는 선언된 메모리 안에 있다는 검증을 통과한 뒤입니다.
///
/// 커널이 닿을 수 없는 사본(펌웨어가 가진 원본, UEFI 로더의 버퍼)은 지우지
/// 못합니다.
fn wipe_dtb_seeds(con: &mut EarlyCon, bootinfo: &mut k0_boot::BootInfo) {
    let base = bootinfo.dtb.start as u64;
    let mut wiped = 0;
    let mut clean = true;
    for &(offset, len) in bootinfo.seed_spans() {
        let start = base + offset as u64;
        if k0_mm::wipe_ro_alias(start..start + len as u64).is_err() {
            clean = false;
            continue;
        }
        // SAFETY: DTB 전체가 읽기 전용 별칭으로 매핑돼 있고 구간은 그 안임
        let left = unsafe {
            core::slice::from_raw_parts((start + k0_mm::KERNEL_VA_OFFSET) as *const u8, len)
        };
        if left.iter().any(|&b| b != 0) {
            clean = false;
        } else {
            wiped += len;
        }
    }
    bootinfo.wipe_entropy();
    if clean {
        let _ = writeln!(con, "k0: dtb seeds wiped {wiped}B");
    } else {
        let _ = writeln!(con, "k0: WARNING dtb seeds not wiped");
    }
}

/// 부트 스택에서 지금 쓰는 부분 아래를 전부 0으로 덮는 함수입니다.
///
/// 시드를 해시하던 함수들의 지역 변수와 해시 상태가 이미 끝난 프레임에
/// 남아 있을 수 있어서, EL0로 넘어가기 직전에 한꺼번에 지웁니다. 이 스택은
/// 이후 시스템 콜 스택으로 다시 쓰입니다. 지금 살아 있는 프레임(kernel_main과
/// 이 함수)은 지우지 못하므로 최선의 노력입니다.
#[inline(never)]
fn scrub_boot_stack() {
    let sp: u64;
    // SAFETY: 스택 포인터를 읽기만 함
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)) };
    let bottom = &raw const __boot_stack_bottom as u64;
    // 이 함수 자신의 프레임과 벡터가 쓸 자리를 남김
    let top = sp.saturating_sub(256) & !7;
    let mut p = bottom;
    while p < top {
        // SAFETY: [bottom, top)은 부트 스택에서 현재 프레임보다 아래의 쓰지
        //         않는 구간이고 8바이트 정렬임
        unsafe { core::ptr::write_volatile(p as *mut u64, 0) };
        p += 8;
    }
}

/// untyped로 넘길 구간을 구하고, 구간마다 꼬리에서 커널 몫을 떼어 두는
/// 함수입니다.
///
/// 커널 몫은 두 가지입니다. 하나는 그 구간의 모든 그래뉼을 커널 별칭으로
/// 매핑하는 데 들 수 있는 페이지 테이블 전부이고, 다른 하나는 그 구간의
/// 그래뉼마다 하나씩인 주소 공간 등록부 항목입니다. 둘을 부팅 때 구간 크기에
/// 맞춰 잡아 두므로, 부팅 뒤에는 어떤 태스크도 untyped 메모리를 쥐는 것만으로
/// 별칭 테이블이나 등록부를 바닥낼 수 없습니다.
///
/// 구간은 큰 것부터 처리합니다. 꼬리를 매핑하는 데 드는 테이블은 첫 구간만
/// 정적 풀에서 치르고 그 뒤로는 앞서 등록한 예약에서 나옵니다. 자기 몫을 댈
/// 수 없는 구간(너무 작거나 매핑에 실패한 구간)은 untyped로 넘기지 않고 한
/// 줄을 남깁니다. 부팅을 거부하지는 않습니다(UEFI 로더의 사전 검사가 이
/// 단계를 알지 못하므로 로더가 예측할 수 없는 거부를 만들지 않음). 온전한
/// 그래뉼이 하나도 없는 틈은 오브젝트를 만들 수 없어 커널 몫도 필요 없으므로
/// 그대로 넘깁니다.
///
/// # Arguments
/// `memory` - DTB가 보고한 물리 메모리 구간들
/// `reserved` - untyped에서 제외할 구간들
/// `out` - untyped로 넘길 구간을 받을 버퍼
///
/// # Errors
/// 구간 산술 위반과 메모리 구간 간 겹침은 `BadRegion`. 성공하면 `out`에 채운
/// 구간 수를 반환합니다
fn reserve_kernel_share(
    con: &mut EarlyCon,
    memory: &[k0_cap::PhysRegion],
    reserved: &[k0_cap::PhysRegion],
    out: &mut [k0_cap::PhysRegion],
) -> Result<usize, k0_cap::CapError> {
    let g = k0_mm::GRANULE as u64;
    k0_cap::check_regions(memory, reserved)?;

    // 버퍼에 다 담지 못한 틈은 넘기지 않음. 커널 몫을 잡지 못한 메모리를
    // untyped로 내주는 것보다 쓰지 않는 쪽이 안전함
    let mut count = 0;
    for m in memory {
        let _ = k0_cap::for_each_gap(*m, reserved, |gap| -> Result<(), ()> {
            if count < out.len() {
                out[count] = gap;
                count += 1;
            } else {
                let _ = writeln!(
                    con,
                    "k0: memory {:#x} + {:#x} not handed out (too many ranges)",
                    gap.base, gap.size
                );
            }
            Ok(())
        });
    }

    let (mut tables, mut entries, mut bytes, mut ranges) = (0u64, 0u64, 0u64, 0u64);
    let mut done = [false; k0_mm::MAX_TABLE_RESERVES];
    loop {
        // 아직 처리하지 않은 것 중 가장 큰 구간
        let mut pick: Option<usize> = None;
        for i in 0..count {
            if !done[i] && pick.is_none_or(|p| out[i].size > out[p].size) {
                pick = Some(i);
            }
        }
        let Some(i) = pick else { break };
        done[i] = true;

        let (base, size) = (out[i].base, out[i].size);
        // 주소 범위의 맨 끝에 걸친 틈은 올림이 넘치므로 온전한 그래뉼이 없는
        // 틈과 같이 다룸(끝 주소는 check_regions가 넘치지 않음을 확인함)
        let Some(lo) = base.checked_next_multiple_of(g) else {
            continue;
        };
        let hi = (base + size) / g * g;
        if hi <= lo {
            continue;
        }
        let pages = (hi - lo) / g;
        let table_count = k0_mm::alias_tables_for(&(lo..hi));
        let entry_pages = (pages * 8).div_ceil(g);
        let share = table_count + entry_pages;
        let reason = if pages <= share {
            Some("too small for its kernel tables")
        } else {
            let tail = hi - share * g;
            let entry_pa = tail + table_count * g;
            if k0_mm::reserve_tables(tail..entry_pa).is_err() {
                Some("no kernel tables to map its reserve")
            } else if k0_mm::map_kernel_window(entry_pa..hi).is_err() {
                Some("its address space entries cannot be mapped")
            } else {
                // SAFETY: 방금 별칭으로 매핑한 구간이고 untyped에서 빠져 커널만
                //         쓰므로 소거가 다른 것을 덮지 않음. 등록부 접근은 단일
                //         부트 코어의 초기화 시퀀스라 배타적이고, 구간들은 서로
                //         겹치지 않는 틈이며 루트 테이블은 부트 윈도우에 있음
                let added = unsafe {
                    core::ptr::write_bytes(
                        (entry_pa + k0_mm::KERNEL_VA_OFFSET) as *mut u8,
                        0,
                        (hi - entry_pa) as usize,
                    );
                    k0_mm::addr_spaces().add_range(lo, pages, entry_pa)
                };
                if added {
                    out[i].size = tail - base;
                    tables += table_count;
                    entries += pages;
                    bytes += share * g;
                    ranges += 1;
                    None
                } else {
                    Some("address space registry is full")
                }
            }
        };
        if let Some(why) = reason {
            let _ = writeln!(con, "k0: memory {base:#x} + {size:#x} not handed out ({why})");
            out[i].size = 0;
        }
    }
    let _ = writeln!(
        con,
        "k0: kernel reserve {tables} tables + {entries} address space entries = {bytes:#x} bytes in {ranges} ranges"
    );

    // 넘기지 않기로 한 구간을 빼고 순서를 유지한 채 앞으로 모음
    let mut kept = 0;
    for i in 0..count {
        if out[i].size != 0 {
            out[kept] = out[i];
            kept += 1;
        }
    }
    Ok(kept)
}

/// 루트 CNode의 내용을 bootinfo 페이지에 기록하는 함수입니다.
///
/// 페이지는 EL0에 RO로 매핑돼 있고 커널은 TTBR1 별칭으로 씁니다. 0번부터
/// `count - 1`번 슬롯까지 하나씩 서술합니다. untyped는 물리 구간을, 이미지
/// Frame은 루트 주소 공간에서의 VA와 권한을 알려 주고, 나머지 종류는 커널
/// 내부 주소를 숨깁니다(0).
///
/// # Arguments
/// `frame_pa` - bootinfo 페이지 프레임의 PA(부트 윈도우에서 할당)
/// `cnode` - 부트스트랩을 마친 루트 CNode
/// `count` - 서술할 슬롯 수(첫 빈 슬롯 번호)
/// `image` - 이미지 프레임의 PA를 VA로 바꾸기 위한 배치
///
/// # Errors
/// 디스크립터가 페이지 하나에 들어가지 않으면 false를 반환하며, 호출자는
/// 부팅을 중단해야 합니다
fn write_bootinfo(frame_pa: u64, cnode: &k0_cap::CNodeRef, count: u64, image: &ImageMap) -> bool {
    use k0_abi::bootinfo::{cap_kind, CapDesc, Header, VERSION};
    use k0_cap::Cap;

    let capacity = ((k0_mm::GRANULE - size_of::<Header>()) / size_of::<CapDesc>()) as u64;
    if count > capacity {
        return false;
    }
    let plain = |kind| CapDesc {
        kind,
        base: 0,
        size: 0,
        rights: 0,
    };

    let hdr = (frame_pa + k0_mm::KERNEL_VA_OFFSET) as *mut Header;
    // SAFETY: 프레임은 부트 윈도우에서 할당돼 별칭 매핑이 있고, EL0에는 RO라
    //         이 기록 경로가 유일한 쓰기임. count가 페이지 용량 안임을 위에서
    //         확인함
    unsafe {
        hdr.write(Header {
            version: VERSION,
            frame_size: k0_mm::GRANULE as u64,
            cap_count: count,
            cnode_slots: cnode.len() as u64,
        });
        let descs = hdr.add(1) as *mut CapDesc;
        for i in 0..count {
            let entry = match cnode.slot(i).map(|s| k0_cap::read(s)) {
                Some(Ok(e)) => e,
                Some(Err(_)) => return false,
                None => None, // 0번 슬롯
            };
            let d = match entry.map(|e| e.cap) {
                None => plain(cap_kind::EMPTY),
                Some(Cap::Tcb { .. }) => plain(cap_kind::TCB),
                Some(Cap::AddrSpace { .. }) => plain(cap_kind::ADDR_SPACE),
                Some(Cap::Console) => plain(cap_kind::CONSOLE),
                Some(Cap::CNode { .. }) => plain(cap_kind::CNODE),
                Some(Cap::PageTable { .. }) => plain(cap_kind::PAGE_TABLE),
                Some(Cap::Endpoint { .. }) => plain(cap_kind::ENDPOINT),
                Some(Cap::Untyped { base, size, .. }) => CapDesc {
                    kind: cap_kind::UNTYPED,
                    base,
                    size,
                    rights: 0,
                },
                Some(Cap::Frame { base, rights, .. }) => {
                    // 이미지 프레임만 부트 시점에 존재함. PA 대신 루트 주소
                    // 공간에서의 VA를 알려 줌
                    if base < image.pa || base - image.pa >= image.len {
                        return false;
                    }
                    CapDesc {
                        kind: cap_kind::FRAME,
                        base: image.va + (base - image.pa),
                        size: k0_mm::GRANULE as u64,
                        rights: u64::from(rights),
                    }
                }
            };
            descs.add(i as usize).write(d);
        }
    }
    true
}

/// 초기 페이지 테이블을 구성해 MMU를 켜는 함수입니다. (점프 전 구간)
///
/// 이미지가 적재된 물리 주소를 반환합니다. 점프 전이라 링커 심볼의 주소가 곧
/// 실제로 적재된 물리 주소입니다(PC 상대로 계산됨).
///
/// # Errors
/// 활성화 실패는 주소 공간을 신뢰할 수 없다는 의미이기 때문에 fail-secure
/// 파킹으로 이어집니다.
fn enable_mmu(con: &mut EarlyCon) -> u64 {
    let text_start = &raw const __kernel_start as u64;
    let rodata_start = &raw const __rodata_start as u64;
    let data_start = &raw const __data_start as u64;
    let bss_start = &raw const __bss_start as u64;
    let stack_bottom = &raw const __boot_stack_bottom as u64;
    let stack_top = &raw const __boot_stack_top as u64;
    let end = &raw const __kernel_end as u64;
    let granule = k0_mm::GRANULE as u64;

    let layout = k0_mm::KernelLayout {
        text: text_start..rodata_start,
        rodata: rodata_start..data_start,
        // 두 구간 사이의 가드 페이지는 의도적으로 매핑안함
        rw: [data_start..stack_bottom - granule, stack_bottom..stack_top],
        cold: bss_start..end,
    };

    match k0_mm::enable_paging(&layout) {
        Ok(_mmu) => {
            con.put_str("k0: mmu on (identity + fixed image address)\n");
        }
        Err(e) => {
            con.put_str("k0: mmu enable failed, code = ");
            con.put_hex(e as u64);
            con.put_str("\n");
            park()
        }
    }
    text_start
}

/// DTB를 TTBR1 별칭에 읽기 전용으로 매핑하는 함수입니다.
///
/// 크기를 알려면 헤더를 읽어야 하므로 첫 그래뉼을 먼저 열고, 헤더가 말하는
/// 크기(상한 2MiB)만큼 다시 엽니다. 포인터가 RAM이 아닌 곳을 가리키면 여기서의
/// 읽기가 폴트로 정지합니다. 그 주소가 선언된 메모리 안인지는 메모리 맵을
/// 읽은 뒤에 확인합니다.
fn map_dtb(con: &mut EarlyCon, dtb_phys: usize) {
    let fail = |con: &mut EarlyCon, what: &str| -> ! {
        let _ = writeln!(con, "k0: dtb rejected: {what}");
        park()
    };
    if dtb_phys == 0 || dtb_phys % 8 != 0 {
        fail(con, "bad pointer");
    }
    let start = dtb_phys as u64;
    if k0_mm::map_dtb(start..start + 8).is_err() {
        fail(con, "header not mappable");
    }
    let span = match k0_boot::dtb_span(dtb_phys + k0_mm::KERNEL_VA_OFFSET as usize) {
        Ok(span) => span,
        Err(e) => {
            let _ = writeln!(con, "k0: dtb rejected: {e:?}");
            park()
        }
    };
    if k0_mm::map_dtb(start..start + (span.end - span.start) as u64).is_err() {
        fail(con, "not mappable");
    }
}

/// 고정 모드의 기대값입니다. 빌드할 때 환경 변수 `K0_PIN`에 부트 로그의
/// `k0: platform` 줄 내용을 넣으면, 그와 다른 플랫폼에서는 부팅을 거부합니다.
const PIN: Option<&str> = option_env!("K0_PIN");

/// 한 줄을 스택에서 조립하는 버퍼입니다. 넘치면 넘쳤다는 사실만 기억합니다.
struct Line {
    buf: [u8; 240],
    len: usize,
    overflow: bool,
}

impl Line {
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl core::fmt::Write for Line {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            if self.len == self.buf.len() {
                self.overflow = true;
                break;
            }
            self.buf[self.len] = b;
            self.len += 1;
        }
        Ok(())
    }
}

/// 발견한 콘솔을 매핑하고 연결하는 함수입니다.
///
/// 이 빌드에 드라이버가 없는 종류는 장치 페이지를 매핑하기 전에 걸러 냅니다.
/// 식별 레지스터가 있는 PL011은 계속 쓸 매핑을 만들기 전에 프로브 칸으로 그
/// 값을 확인하므로, 검사에 떨어진 장치는 매핑된 채 남지 않습니다. 어느 쪽이든
/// 연결하지 못하면 콘솔 없이 계속합니다(로그에만 남음).
#[cfg(any(feature = "console-pl011", feature = "console-16550"))]
fn attach_console(con: &mut EarlyCon, c: &k0_boot::Console, counter_hz: u64) {
    use k0_arch::earlycon::Uart;
    let uart = match c.kind {
        #[cfg(feature = "console-pl011")]
        k0_boot::ConsoleKind::Pl011 | k0_boot::ConsoleKind::SbsaUart => Uart::Pl011,
        #[cfg(feature = "console-16550")]
        k0_boot::ConsoleKind::Ns16550 { reg_shift, io_width } => Uart::Ns16550 {
            shift: reg_shift,
            width: io_width,
        },
        // 두 드라이버가 다 있는 빌드에서는 닿지 않는 갈래
        #[allow(unreachable_patterns)]
        _ => {
            let _ = writeln!(con, "k0: no console (driver not built in)");
            return;
        }
    };
    #[cfg(feature = "console-pl011")]
    if c.kind == k0_boot::ConsoleKind::Pl011 {
        // 식별 레지스터는 4KiB 프레임의 끝에 있음. 선언된 구간이 프레임
        // 전체일 때만 읽음
        let framed = c.base % 0x1000 == 0 && c.reg.size >= 0x1000;
        if framed {
            // 프로브 칸으로 들여다보고 곧바로 지움. 검사에 떨어진 장치의 페이지가
            // 커널 주소 공간에 남지 않게 함
            let Ok(probe) = k0_mm::probe_map(c.base) else {
                let _ = writeln!(con, "k0: console not mapped");
                return;
            };
            // SAFETY: 방금 Device로 매핑한 PL011 프레임의 시작 주소
            let ok = unsafe { k0_arch::earlycon::pl011_identity_ok(probe as usize) };
            k0_mm::probe_clear();
            if !ok {
                let _ = writeln!(con, "k0: console is not a pl011, ignored");
                return;
            }
        }
    }
    let Ok(va) = k0_mm::map_device(c.base) else {
        let _ = writeln!(con, "k0: console not mapped");
        return;
    };
    // SAFETY: 드라이버가 건드리는 범위가 한 그래뉼 안임을 검증했고(validate),
    //         그 그래뉼을 방금 Device로 매핑함
    unsafe { k0_arch::earlycon::attach(uart, va as usize, counter_hz) };
}

/// 콘솔 드라이버가 없는 빌드의 연결 함수입니다.
///
/// 콘솔을 찾았더라도 장치 페이지를 매핑하지 않고 UART에 접근하지 않습니다.
/// 출력은 메모리의 부트 로그에만 남습니다.
#[cfg(not(any(feature = "console-pl011", feature = "console-16550")))]
fn attach_console(con: &mut EarlyCon, _c: &k0_boot::Console, _counter_hz: u64) {
    let _ = writeln!(con, "k0: no console (driver not built in)");
}

/// 커널이 매핑해 둔 디바이스 페이지를 한 줄로 출력하는 함수입니다.
///
/// 사용자 공간으로 넘어가기 직전에 부릅니다. 이 뒤로 커널은 디바이스를 더
/// 매핑하지 않으므로 목록이 곧 이 커널이 닿을 수 있는 장치의 전부입니다.
/// 콘솔 드라이버가 없는 이미지에서 콘솔 페이지가 여기에 없다는 것을 부팅
/// 매트릭스가 확인합니다.
fn print_device_pages(con: &mut EarlyCon) {
    let _ = write!(con, "k0: device pages [");
    let mut first = true;
    k0_mm::device_pages(|page| {
        let _ = write!(con, "{}{page:#x}", if first { "" } else { " " });
        first = false;
    });
    let _ = writeln!(con, "] of {:#x} bytes", k0_mm::GRANULE);
}

/// DTB에서 플랫폼을 발견해 검증하고 콘솔을 연결하는 함수입니다.
///
/// 순서는 발견, 메모리 맵과의 대조, 고정 모드 대조, 콘솔 연결입니다. 디바이스
/// 페이지는 대조를 통과한 뒤에만 매핑합니다. 플랫폼과 카운터 주파수를
/// 반환합니다.
///
/// # Errors
/// 어느 단계든 실패하면 부팅을 거부하고 파킹합니다. 인터럽트 컨트롤러나
/// 타이머 때문에 거부할 때도, 지원하는 콘솔이 있고 대조를 통과하면 사유를
/// 볼 수 있게 그 콘솔을 먼저 연결합니다.
fn bring_up_platform(
    con: &mut EarlyCon,
    dtb_phys: usize,
    bootinfo: &k0_boot::BootInfo,
    image: &Range<u64>,
) -> (k0_boot::Platform, u64) {
    // 거부로 끝나는 부팅에서도 어떤 이미지인지 로그에 남도록 맨 먼저 출력함
    let _ = writeln!(con, "{}", k0_arch::earlycon::DRIVERS_LINE);
    let off = k0_mm::KERNEL_VA_OFFSET as usize;
    let layout = k0_boot::Layout {
        memory: bootinfo.memory(),
        reserved: bootinfo.reserved(),
        image: image.clone(),
        dtb: bootinfo.dtb.start as u64..bootinfo.dtb.end as u64,
        granule: k0_mm::GRANULE as u64,
        ram_limit: k0_mm::PA_CEILING,
        device_limit: 1u64 << k0_mm::pa_bits(),
    };

    let found = match k0_boot::discover(dtb_phys, off) {
        Ok(found) => found,
        Err(e) => {
            if let Some(c) = k0_boot::discover_console(dtb_phys, off) {
                if c.check(&layout).is_ok() && PIN.is_none() {
                    attach_console(con, &c, 0);
                }
            }
            let _ = writeln!(con, "k0: platform rejected: {e:?}");
            park()
        }
    };
    let platform = found.platform;
    if let Err(e) = platform.validate(&layout) {
        let _ = writeln!(con, "k0: platform rejected: {e:?} ({platform})");
        park()
    }

    // 고정 모드가 대조하는 한 줄: 디바이스, 진입 EL, RAM이 놓인 범위
    let lo = bootinfo.memory().iter().map(|m| m.base).min().unwrap_or(0);
    let hi = bootinfo.memory().iter().map(|m| m.base + m.size).max().unwrap_or(0);
    let mut line = Line {
        buf: [0; 240],
        len: 0,
        overflow: false,
    };
    let _ = write!(
        line,
        "{platform} el={} ram={lo:#x}-{hi:#x}",
        K0_ENTRY_EL.load(Ordering::Relaxed)
    );
    if let Some(pin) = PIN {
        if line.overflow || pin != line.as_str() {
            // 기대와 다른 하드웨어의 콘솔에는 쓰지 않음. 사유는 로그에만 남음
            let _ = writeln!(con, "k0: platform rejected: differs from the pinned build");
            let _ = writeln!(con, "k0: found  {}", line.as_str());
            let _ = writeln!(con, "k0: pinned {pin}");
            park()
        }
    }

    let counter_hz = k0_arch::irq::counter_hz(platform.timer.clock_hz);
    if let Some(c) = &platform.console {
        attach_console(con, c, counter_hz.unwrap_or(0));
    }
    let _ = writeln!(con, "k0: platform {}", line.as_str());
    if let Some(why) = found.no_console {
        let _ = writeln!(con, "k0: no console ({why:?})");
    }
    match counter_hz {
        Ok(hz) => (platform, hz),
        Err(e) => {
            let _ = writeln!(con, "k0: timer rejected: {e:?}");
            park()
        }
    }
}

/// 인터럽트 컨트롤러의 레지스터 페이지를 매핑하고 초기화하는 함수입니다.
///
/// 필요한 페이지만 매핑합니다. GICv3의 리디스트리뷰터 구간은 CPU 수만큼
/// 커지므로 통째로 매핑하지 않고, 프로브 페이지로 한 프레임씩 들여다봐 부트
/// 코어의 프레임을 찾은 뒤 그 두 페이지만 매핑합니다.
///
/// # Errors
/// 매핑 실패는 `ProbeFailed`, 그 밖은 드라이버의 `IrqError`
fn setup_irq(
    traps: &k0_arch::vectors::Vectors,
    platform: &k0_boot::Platform,
    counter_hz: u64,
) -> Result<k0_arch::irq::Irq, k0_arch::irq::IrqError> {
    use k0_arch::irq::{gicv3, Gic, IrqError};
    let map = |pa: u64| k0_mm::map_device(pa).map(|va| va as usize).map_err(|_| IrqError::ProbeFailed);

    let gic = match &platform.intc {
        k0_boot::Intc::GicV2 { dist, cpu } => Gic::V2 {
            dist: map(dist.base)?,
            cpu: map(cpu.base)?,
        },
        k0_boot::Intc::GicV3 { dist, stride, .. } => {
            let mut regions = [(0u64, 0u64); k0_boot::MAX_REDIST_REGIONS];
            let list = platform.intc.redist();
            for (slot, r) in regions.iter_mut().zip(list) {
                *slot = (r.base, r.size);
            }
            let rd = gicv3::find_redistributor(&regions[..list.len()], *stride, &mut |pa| {
                k0_mm::probe_map(pa).ok().map(|va| va as usize)
            });
            // 마지막으로 들여다본 프레임이 매핑된 채 남지 않게 함
            k0_mm::probe_clear();
            let rd = rd?;
            Gic::V3(gicv3::Frames {
                dist: map(dist.base)?,
                dist_pidr2: map(dist.base + gicv3::PIDR2)?,
                rd: map(rd)?,
                sgi: map(rd + gicv3::SGI_OFFSET)?,
            })
        }
    };
    // SAFETY: 주소들은 DTB에서 발견해 메모리 맵과의 대조(validate)를 마친
    //         디바이스를 방금 Device로 매핑한 것임
    unsafe { k0_arch::irq::init(traps, &gic, platform.timer.intid, counter_hz) }
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
        ("id+r", k0_mm::can_read(k0_mm::image_pa(ktext_va)), false),
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
    let checks: [(&str, bool, bool); 7] = [
        ("text+r", k0_mm::can_read(text), true),
        ("text+w", k0_mm::can_write(text), false),
        ("rodata+w", k0_mm::can_write(rodata), false),
        ("guard+r", k0_mm::can_read(stack_bottom - granule), false),
        ("stack+w", k0_mm::can_write(stack_top - granule), true),
        ("id+r", k0_mm::can_read(k0_mm::image_pa(text)), true),
        // 이미지에는 선형 별칭이 없어야 함(재분류의 소거가 닿을 수 없게)
        ("alias+r", k0_mm::can_read(k0_mm::image_pa(text) + k0_mm::KERNEL_VA_OFFSET), false),
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
