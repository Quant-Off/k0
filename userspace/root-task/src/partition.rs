//! 시간 분할(도메인과 표)을 부팅 때마다 검증하는 자가 검증 모듈입니다.
//!
//! # Features
//! 세 부분으로 나뉩니다.
//!
//! - 거절: 제어 케이퍼빌리티가 아닌 슬롯, 빈 슬롯, 틀린 표, 범위 밖 도메인,
//!   Stopped가 아닌 TCB(구성 전, 실행 중, 준비, 블록, 종료), 제어 케이퍼빌리티의
//!   복사가 모두 정해진 에러로 거절되고, 거절 뒤에도 기본 표로 시스템이 계속
//!   도는지 봅니다. 기다리는 일은 마지막의 틱 하나짜리 대기뿐입니다.
//! - 창: 아홉 틱짜리 표 하나를 한 바퀴 돌려 보고 실행 순서만으로 판정합니다.
//!   EL0에는 시계가 없으므로 시간을 재지 않습니다. 루트 태스크가 기한 대기로 깨어나는
//!   순서가 곧 시계이고, 모든 태스크가 한 로그에 남긴 글자의 순서를 비교합니다.
//! - 봉인: 제어 케이퍼빌리티를 지우면 표를 더 바꿀 수 없는지 봅니다.
//!
//! 창 검증의 표는 틱 E부터 `E:1 E+1:0 E+2:2 E+3:0 E+4:2 E+5:2 E+6:1 E+7:3 E+8:0`
//! 입니다(숫자는 도메인이고 도메인 2의 창은 두 틱). 도메인 0은 루트 태스크와 기한 2틱 대기자 H, 1은
//! 기한 대기자 X와 X가 만든 자식 C, 그리고 X보다 나중에 기한을 거는 V, 2는
//! 양보하는 Y1과 도는 Y2, 3은 표에 있지만 태스크가 없는 도메인, 4는 어떤 표에도
//! 없는 도메인(Z)입니다. 표는 호출한 틱 n의
//! 두 틱 뒤인 E = n + 2에서 0번 항목부터 효력이 생깁니다. 기한 n틱짜리 대기는 호출
//! 때의 틱 수에 n + 1을 더한 틱에 끝나고, 한 도메인의 대기자는 그 도메인이 도는
//! (또는 들어가는) 틱에서만 깨어납니다.
//!
//! Y1과 Y2는 다른 태스크가 그 사이에 실행된 뒤에만 로그를 남깁니다(마지막으로 돈
//! 태스크를 적은 변수로 판단). 그래서 로그의 글자는 "누가 언제 다시 CPU를 받았는가"
//! 를 나타냅니다.
//!
//! | 틱 | 도메인 | 일어나는 일 | 로그 |
//! | --- | --- | --- | --- |
//! | E | 1 | X가 `X`를 남기고 자식을 구성해 시작시킨 뒤 양보, V도 양보, 자식이 `k`를 남기고 영영 블록, X가 `w`를 남기고 기한 1틱 대기(기한 E+2), V가 기한 2틱 대기(기한 E+3), 남는 창은 빈 채로 | X k w |
//! | E+1 | 0 | 루트의 기한(E)과 H의 기한(E+1)이 지나 둘이 깨어남. 루트가 `R`을 남기고 다시 대기(기한 E+3), H가 `h`를 남기고 영영 블록 | R h |
//! | E+2 | 2 | Y1이 `a`를 남기고 양보, Y2가 `b`를 남기고 돎 | a b |
//! | E+3 | 0 | 도는 Y2를 끊고(틱 사이에 들어와 첫 틱 면제) 큐 앞에 세움, 루트가 `r`을 남기고 대기(기한 E+5) | r |
//! | E+4 | 2 | 큐 앞의 Y2가 다시 들어와 돎(Y1이 앞서면 이때 `y`가 남음) | |
//! | E+5 | 2 | 같은 도메인의 틱: 틱의 판정으로 들어온 Y2가 끊기고 Y1이 `y`를 남김(들어올 때의 경위가 틱이 아니면 Y2가 이어서 돌아 `y`가 없음) | y |
//! | E+6 | 1 | Y1을 끊고 들어가 두 기한(E+2, E+3)이 이제야 처리됨. 나중에 건 V가 먼저 깨어나 `v`를, 이어서 X가 `x`를 남기고 둘이 번갈아 양보하며 돎 | v x |
//! | E+7 | 3 | 돌던 태스크를 끊고 태스크 없는 도메인에 들어가 빈 채로 기다림 | |
//! | E+8 | 0 | 루트의 기한(E+5)이 지나 깨어나 로그 `XkwRhabryvx`를 확인하고 정리, 기본 표 복원 | |
//!
//! 도메인 1의 두 기한은 모두 다른 도메인의 창에서 지납니다. 기한이 그 창에서
//! 처리됐다면 X가 먼저 큐에 서서 `x`가 `v`보다 앞섭니다. E+6에서 X와 V는 블록되지
//! 않고 서로에게 양보만 하므로 양보는 언제나 상대를 거쳐 돌아옵니다. 창이 닫힌
//! 뒤에(E+7의 빈 창에서) 끊긴 태스크가 계속 실행된다면 양보가 상대 없이 돌아오고,
//! 그 횟수를 세어 0인지 봅니다.
//!
//! 복원 뒤에는 기한 2틱 대기를 한 번 더 하며, 그동안 도메인 2의 Y1과 어떤 표에도
//! 없는 도메인의 Z가 한 번도 실행되지 않았는지 카운터로 확인합니다. 복원이
//! 적용되지 않았다면 표가 이어져 도메인 2의 창이 열리므로 Y1이 돕니다. 이
//! 대기가 기본 표에서 CPU가 노는 일 없이 끝나도록(처음 노는 때의 안내 줄이 이
//! 검증 안에서 찍히지 않도록) 복원할 때 도메인 0에 도는 태스크를 하나 둡니다.
//!
//! 틱 하나가 호스트의 멈춤으로 통째로 건너뛰어지면 로그가 달라집니다. 그때는
//! 정리하고 한 번 더 돌리며 `root: partition window retry` 줄을 남깁니다. 두 번
//! 다 틀리면 실패입니다.
//!
//! # Examples
//! ```rust,ignore
//! partition::test_refusals(b);
//! partition::test_windows(b);
//! partition::seal();
//! ```

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::{
    alloc_slot, cap_delete, check, copy_here, fail, map, part_set_domain, part_set_table,
    put_str, recv, release_slots, retype, retype_at, sys, tcb_configure, tcb_resume, Boot,
    NEXT_SLOT, TEST_VA,
};
use k0_abi::bootinfo::slot;
use k0_abi::{err, ipc, obj, perm, rights, syscall};

/// 태스크들이 남기는 글자의 로그. 인덱스는 [LEN]의 fetch_add로 받음
static LOG: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
static LEN: AtomicU64 = AtomicU64::new(0);
/// X가 구성할 자식의 TCB 슬롯과 스택 최상단
static CHILD_TCB: AtomicU64 = AtomicU64::new(0);
static CHILD_SP: AtomicU64 = AtomicU64::new(0);
/// X가 기한 대기를 거는 엔드포인트
static EP_X: AtomicU64 = AtomicU64::new(0);
/// H가 기한 대기를 거는 엔드포인트
static EP_H: AtomicU64 = AtomicU64::new(0);
/// 영영 블록되는 태스크들(X의 자식, H)이 기다리는 엔드포인트
static EP_V: AtomicU64 = AtomicU64::new(0);
/// Y1, Y2, Z의 진행(복원 뒤에 실행되지 않았는지 보는 데 씀)
static Y1_SPINS: AtomicU64 = AtomicU64::new(0);
static Y2_SPINS: AtomicU64 = AtomicU64::new(0);
/// 마지막으로 로그를 남기며 돈 Y 태스크(1 또는 2)
static LAST: AtomicU64 = AtomicU64::new(0);
static Z_SPINS: AtomicU64 = AtomicU64::new(0);
/// 도메인 1에서 마지막으로 양보한 태스크(1 = X, 2 = V)
static TURN: AtomicU64 = AtomicU64::new(0);
/// 양보가 상대를 거치지 않고 돌아온 횟수
static ALONE: AtomicU64 = AtomicU64::new(0);

/// 창 검증이 끝났을 때 로그에 있어야 하는 글자
const EXPECT: &[u8] = b"XkwRhabryvx";

/// 창 검증의 표입니다. 항목은 (도메인, 틱 수)
const T8: [(u8, u8); 8] = [(1, 1), (0, 1), (2, 1), (0, 1), (2, 2), (1, 1), (3, 1), (0, 1)];

/// 태스크 스택이 놓이는 첫 그래뉼 번호(TEST_VA 기준). 사이사이는 가드로 비워 둠
const STACK_BASE: u64 = 62;

/// 창 검증의 태스크 여덟 개의 번호입니다. 거절 검증은 앞의 여섯 자리를 씁니다.
const X: usize = 0;
const C: usize = 1;
const Y1: usize = 2;
const Y2: usize = 3;
const Z: usize = 4;
const S0: usize = 5;
const H: usize = 6;
const V: usize = 7;

/// 로그에 글자 하나를 남기는 함수입니다.
fn log(tag: u8) {
    let i = LEN.fetch_add(1, Relaxed) as usize;
    if i < LOG.len() {
        LOG[i].store(u64::from(tag), Relaxed);
    }
}

/// 기한 없이 블록되는 함수입니다. 루트가 이 태스크를 지울 때까지 돌아오지 않습니다.
fn park(ep: u64) -> ! {
    let _ = recv(ep as i64, 0);
    loop {
        core::hint::spin_loop();
    }
}

/// 도메인 1의 기한 대기자 X의 진입점입니다. 자식 태스크를 하나 구성해 시작시키고(자식은
/// 만든 태스크의 도메인을 물려받아야 함), 자식이 돌 때까지 양보한 뒤 기한 대기에
/// 들어갑니다. 기한이 지나 깨어나면 `x`를 남기고 V와 번갈아 양보합니다.
extern "C" fn x_entry(_arg: u64) -> ! {
    log(b'X');
    let (t, sp) = (CHILD_TCB.load(Relaxed) as i64, CHILD_SP.load(Relaxed));
    let ok = super::thread_configure(t, c_entry as usize as u64, sp) == 0 && tcb_resume(t) == 0;
    super::yield_now();
    log(if ok { b'w' } else { b'e' });
    let st = recv(EP_X.load(Relaxed) as i64, ipc::timeout(1)).status;
    log(if st == err::TIMEOUT { b'x' } else { b'e' });
    pair(1)
}

/// 도메인 1의 둘째 기한 대기자 V의 진입점입니다. X가 기한을 건 뒤에 걸리도록 한 번
/// 양보하고 기한 2틱 대기에 들어갑니다. 기한이 지나 깨어나면 `v`를 남기고 X와
/// 번갈아 양보합니다.
extern "C" fn v_entry(_arg: u64) -> ! {
    super::yield_now();
    let st = recv(EP_X.load(Relaxed) as i64, ipc::timeout(2)).status;
    log(if st == err::TIMEOUT { b'v' } else { b'e' });
    pair(2)
}

/// 같은 도메인의 두 태스크가 번갈아 양보하며 도는 함수입니다. 양보가 상대를 거치지
/// 않고 돌아오면 [ALONE]을 올립니다.
///
/// 둘 다 블록되지 않으므로 창 안에서는 양보가 언제나 상대에게 갑니다. 상대 없이
/// 돌아오는 것은 창이 닫혀 큐에 서 있어야 할 태스크가 계속 실행될 때뿐입니다.
///
/// # Arguments
/// `me` - 이 태스크의 번호(1 또는 2)
fn pair(me: u64) -> ! {
    loop {
        TURN.store(me, Relaxed);
        super::yield_now();
        if TURN.load(Relaxed) == me {
            ALONE.fetch_add(1, Relaxed);
        }
    }
}

/// 거절 검증에서 기한을 걸고 블록되는 태스크의 진입점입니다.
extern "C" fn timed_entry(_arg: u64) -> ! {
    let _ = recv(EP_V.load(Relaxed) as i64, ipc::timeout(100));
    loop {
        core::hint::spin_loop();
    }
}

/// 거절 검증에서 기한 없이 블록되는 태스크의 진입점입니다.
extern "C" fn parked_entry(_arg: u64) -> ! {
    park(EP_V.load(Relaxed))
}

/// 거절 검증에서 곧바로 끝나는 태스크의 진입점입니다.
extern "C" fn exit_entry(_arg: u64) -> ! {
    super::sys1(syscall::EXIT, 21);
    loop {
        core::hint::spin_loop();
    }
}

/// X가 만든 자식의 진입점입니다. `k`를 남기고 영영 블록됩니다. 도메인 1에 있다면
/// X가 양보한 자리에서 돌고, 도메인 0에 잘못 놓였다면 루트의 창에서야 돕니다.
extern "C" fn c_entry(_arg: u64) -> ! {
    log(b'k');
    park(EP_V.load(Relaxed))
}

/// 도메인 0의 기한 대기자 H의 진입점입니다. 표를 거는 직전에 기한 2틱 대기에
/// 들어가고, 깨어나면 `h`를 남기고 영영 블록됩니다.
extern "C" fn h_entry(_arg: u64) -> ! {
    let st = recv(EP_H.load(Relaxed) as i64, ipc::timeout(2)).status;
    log(if st == err::TIMEOUT { b'h' } else { b'e' });
    park(EP_V.load(Relaxed))
}

/// 도메인 2에서 한 번 양보하는 Y1의 진입점입니다. 다른 태스크가 돈 뒤에 다시 CPU를
/// 받으면 `y`를 남깁니다.
extern "C" fn y1_entry(_arg: u64) -> ! {
    log(b'a');
    LAST.store(1, Relaxed);
    super::yield_now();
    loop {
        if LAST.load(Relaxed) != 1 {
            LAST.store(1, Relaxed);
            log(b'y');
        }
        Y1_SPINS.fetch_add(1, Relaxed);
    }
}

/// 도메인 2에서 끝없이 도는 Y2의 진입점입니다. 처음에는 `b`, 다른 태스크가 돈 뒤에
/// 다시 CPU를 받으면 `c`를 남깁니다.
extern "C" fn y2_entry(_arg: u64) -> ! {
    let mut tag = b'b';
    loop {
        if LAST.load(Relaxed) != 2 {
            LAST.store(2, Relaxed);
            log(tag);
            tag = b'c';
        }
        Y2_SPINS.fetch_add(1, Relaxed);
    }
}

/// 어떤 표에도 없는 도메인의 Z의 진입점입니다. 실행되면 `z`를 남기고 돕니다.
extern "C" fn z_entry(_arg: u64) -> ! {
    log(b'z');
    loop {
        Z_SPINS.fetch_add(1, Relaxed);
    }
}

/// 도메인 0에서 끝없이 도는 태스크의 진입점입니다. 아무것도 남기지 않습니다.
extern "C" fn s0_entry(_arg: u64) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// 여러 번 쓰는 슬롯과 스택 묶음 구조체입니다. 두 번 도는 검증이 같은 슬롯을 다시 씁니다.
struct Stage {
    ut: u64,
    /// 루트가 기한 대기를 거는 엔드포인트
    wait: i64,
    tcb: [u64; 8],
    sp: [u64; 8],
}

/// 슬롯과 스택을 준비하는 함수입니다. 스택은 TCB마다 프레임을 하나씩 매핑합니다.
///
/// # Arguments
/// `b` - 부팅 때 찾은 untyped와 그래뉼 크기
/// `count` - 만들 태스크의 수(8 이하)
fn stage(b: &Boot, count: usize) -> Stage {
    let (ut, fs) = (b.ut_main, b.frame_size);
    let wait = retype(ut, obj::ENDPOINT);
    check("partition: wait endpoint", wait > 0);
    let mut st = Stage {
        ut,
        wait,
        tcb: [0; 8],
        sp: [0; 8],
    };
    for i in 0..count {
        let frame = retype(ut, obj::FRAME);
        let va = TEST_VA + (STACK_BASE + 2 * i as u64) * fs;
        check("partition: stack", frame > 0 && map(frame, va, perm::RW) == 0);
        st.sp[i] = va + fs;
        st.tcb[i] = alloc_slot();
    }
    st
}

/// TCB를 만들고 구성해서 Stopped로 두는 함수입니다.
///
/// # Arguments
/// `st` - 슬롯과 스택
/// `i` - 태스크 번호
/// `entry` - 진입점
fn make(st: &Stage, i: usize, entry: extern "C" fn(u64) -> !) {
    check("partition: tcb", retype_at(st.ut, obj::TCB, st.tcb[i]) == 0);
    check(
        "partition: configure",
        super::thread_configure(st.tcb[i] as i64, entry as usize as u64, st.sp[i]) == 0,
    );
}

/// 기한 `n`틱짜리 대기를 걸어 기한으로 끝났는지 알려 주는 함수입니다.
fn wait(st: &Stage, n: u32) -> bool {
    recv(st.wait, ipc::timeout(n)).status == err::TIMEOUT
}

/// 거절 검증입니다. 시스템 콜은 모두 기다림 없이 거절돼야 하고, 마지막에 기본 표로
/// 계속 도는지 틱 하나짜리 대기로 봅니다.
///
/// 이 안의 대기가 CPU를 놀리면 "처음 노는 때"의 안내 줄이 이 검증 안에서 찍혀
/// 뒤의 검증이 기대하는 자리를 잃으므로, 기다리는 동안 도메인 0에서 도는 태스크를
/// 하나 둡니다.
///
/// # Arguments
/// `b` - 부팅 때 찾은 untyped와 그래뉼 크기
pub fn test_refusals(b: &Boot) {
    let first_slot = NEXT_SLOT.load(Relaxed);
    let st = stage(b, 6);
    let ctl = slot::PART_CTL as i64;
    let empty = alloc_slot() as i64;
    let (spin, stopped, inactive) = (st.tcb[0] as i64, st.tcb[1] as i64, st.tcb[2] as i64);
    make(&st, 0, s0_entry);
    make(&st, 1, s0_entry);
    // 구성하지 않은 TCB는 Inactive
    check("partition: inactive tcb", retype_at(st.ut, obj::TCB, st.tcb[2]) == 0);

    let ok = [(0u8, 1u8)];
    // 제어 케이퍼빌리티가 아닌 슬롯과 빈 슬롯
    for (what, s) in [
        ("partition: table via cnode", slot::CNODE as i64),
        ("partition: table via console", slot::CONSOLE as i64),
        ("partition: table via own tcb", slot::TCB as i64),
        ("partition: table via endpoint", st.wait),
        ("partition: table via untyped slot", st.ut as i64),
    ] {
        check(what, part_set_table(s, &ok) == err::BAD_CAP);
    }
    check("partition: table via empty slot", part_set_table(empty, &ok) == err::BAD_SLOT);
    check("partition: table via slot 0", part_set_table(0, &ok) == err::BAD_SLOT);
    check("partition: table via far slot", part_set_table(1 << 40, &ok) == err::BAD_SLOT);

    // 틀린 표. 호출한 태스크의 도메인(0)은 들어 있게 해서 다른 까닭이 가려지지 않게 함
    let bad = |what: &'static str, r: i64| check(what, r == err::BAD_TABLE);
    bad("partition: count 0", part_set_table(ctl, &[]));
    bad("partition: count 17", part_set_table(ctl, &[(0, 1); 17]));
    bad("partition: zero ticks", part_set_table(ctl, &[(0, 0)]));
    bad("partition: zero ticks late", part_set_table(ctl, &[(0, 1), (1, 0)]));
    bad("partition: domain 8", part_set_table(ctl, &[(0, 1), (8, 1)]));
    bad("partition: domain 255", part_set_table(ctl, &[(0, 1), (255, 1)]));
    // 마지막 항목 뒤의 비트 하나라도 서 있으면 거절(레지스터 하나씩, 맨 끝 비트까지)
    let entry0 = 1u64 << 8;
    for (what, count, regs) in [
        ("partition: bit after entry 0", 1u64, [entry0 | 1 << 16, 0, 0, 0]),
        ("partition: bit after entry 3", 4, [entry0 | entry0 << 16 | entry0 << 32 | entry0 << 48, 1 << 32, 0, 0]),
        ("partition: bit in register 3", 1, [entry0, 0, 0, 1 << 63]),
        ("partition: bit 0 of register 2", 5, [entry0 | entry0 << 16 | entry0 << 32 | entry0 << 48, entry0, 1, 0]),
    ] {
        let r = sys(
            syscall::PART_SET_TABLE,
            [ctl as u64, count, regs[0], regs[1], regs[2], regs[3]],
        ) as i64;
        bad(what, r);
    }
    // 호출한 태스크의 도메인이 없는 표
    bad("partition: no own domain", part_set_table(ctl, &[(1, 1)]));
    bad("partition: no own domain 2", part_set_table(ctl, &[(1, 3), (2, 1), (7, 255)]));

    // PART_SET_DOMAIN: 범위 밖 도메인은 Stopped인 TCB에도 거절됨
    for d in [8u64, 9, 255, 256, 1 << 32, u64::MAX] {
        check("partition: domain out of range", part_set_domain(ctl, stopped, d) == err::BAD_DOMAIN);
    }
    // Stopped가 아닌 TCB: 블록된 TCB(기한 없이, 기한을 걸고)와 끝난 TCB. 블록된
    // 채로 도메인이 바뀌면 기한 목록과 TCB의 도메인이 어긋남. 셋은 루트가 양보한
    // 사이에 차례로 돌아 블록되거나 끝남(도는 태스크를 시작시키기 전이라 곧 돌아옴)
    EP_V.store(st.wait as u64, Relaxed);
    let states: [(usize, extern "C" fn(u64) -> !, &'static str); 3] = [
        (3, parked_entry, "partition: set domain blocked"),
        (4, timed_entry, "partition: set domain blocked with a deadline"),
        (5, exit_entry, "partition: set domain dead"),
    ];
    for (i, entry, _) in states {
        make(&st, i, entry);
        check("partition: resume for a state", tcb_resume(st.tcb[i] as i64) == 0);
    }
    super::yield_now();
    for (i, _, what) in states {
        check(what, part_set_domain(ctl, st.tcb[i] as i64, 1) == err::BAD_STATE);
        check("partition: delete after a state", cap_delete(st.tcb[i] as i64) == 0);
    }
    // Inactive, 루트 자신(실행 중), 준비 큐에 선 TCB
    check("partition: set domain inactive", part_set_domain(ctl, inactive, 1) == err::BAD_STATE);
    check(
        "partition: set domain of the running tcb",
        part_set_domain(ctl, slot::TCB as i64, 1) == err::BAD_STATE,
    );
    check("partition: resume spinner", tcb_resume(spin) == 0);
    check("partition: set domain ready", part_set_domain(ctl, spin, 1) == err::BAD_STATE);
    // 틀린 케이퍼빌리티
    check("partition: domain via cnode", part_set_domain(slot::CNODE as i64, stopped, 1) == err::BAD_CAP);
    check("partition: domain via tcb", part_set_domain(slot::TCB as i64, stopped, 1) == err::BAD_CAP);
    check("partition: domain via empty", part_set_domain(empty, stopped, 1) == err::BAD_SLOT);
    check("partition: domain of a cnode", part_set_domain(ctl, slot::CNODE as i64, 1) == err::BAD_CAP);
    check("partition: domain of an endpoint", part_set_domain(ctl, st.wait, 1) == err::BAD_CAP);
    check("partition: domain of an empty slot", part_set_domain(ctl, empty, 1) == err::BAD_SLOT);

    // 제어 케이퍼빌리티는 복사되지 않음
    check("partition: copy of the control", copy_here(ctl, rights::ALL, 0) == err::BAD_CAP);
    check("partition: copy of the control r", copy_here(ctl, rights::R, 0) == err::BAD_CAP);

    // 거절은 아무것도 바꾸지 않았음: 기본 표로 틱 하나짜리 대기가 끝나고, 거절된
    // 표가 효력을 냈다면 도메인 0이 멈춰 이 대기는 끝나지 않음
    check("partition: still running", wait(&st, 1));
    check("partition: delete spinner", cap_delete(spin) == 0);
    release_slots(first_slot);
    put_str("root: partition refusal pass\n");
}

/// 창 검증을 한 번 돌리는 함수입니다. 로그의 순서가 맞았으면 true
///
/// 틀렸더라도 정리는 맞았을 때와 똑같이 하고(기본 표 복원, 태스크 삭제), 복원이
/// 적용됐는지는 로그가 맞았을 때만 봅니다. 호스트가 틱을 건너뛰어 틀린 실행은
/// 표의 어느 자리에서 복원을 걸었는지 알 수 없어, 그 자리의 창이 Y2를 돌릴 수
/// 있기 때문입니다.
///
/// # Arguments
/// `st` - 슬롯과 스택
fn one(st: &Stage) -> bool {
    let ctl = slot::PART_CTL as i64;
    LEN.store(0, Relaxed);
    Y1_SPINS.store(0, Relaxed);
    Y2_SPINS.store(0, Relaxed);
    LAST.store(0, Relaxed);
    Z_SPINS.store(0, Relaxed);
    TURN.store(0, Relaxed);
    ALONE.store(0, Relaxed);
    let entries: [(usize, extern "C" fn(u64) -> !); 7] = [
        (X, x_entry),
        (V, v_entry),
        (Y1, y1_entry),
        (Y2, y2_entry),
        (Z, z_entry),
        (S0, s0_entry),
        (H, h_entry),
    ];
    for (i, entry) in entries {
        make(st, i, entry);
    }
    let t = |i: usize| st.tcb[i] as i64;
    // X의 자식은 재분류만 해 둠(X가 구성하고 시작시킴)
    check("partition: child tcb", retype_at(st.ut, obj::TCB, st.tcb[C]) == 0);
    CHILD_TCB.store(st.tcb[C], Relaxed);
    CHILD_SP.store(st.sp[C], Relaxed);
    // 도메인은 모두 정한 뒤에 시작시킴. 시작 순서가 각 도메인 큐의 순서임
    for (i, d) in [(X, 1), (V, 1), (Y1, 2), (Y2, 2), (Z, 4)] {
        check("partition: set domain", part_set_domain(ctl, t(i), d) == 0);
    }
    for i in [X, V, Y1, Y2, Z] {
        check("partition: resume", tcb_resume(t(i)) == 0);
    }
    // 도메인 0의 기한 대기자 H: 기한 2틱 대기에 들어간 뒤 루트가 돌아옴. 표를 거는
    // 시스템 콜 바로 앞에서 해야 H의 기한이 루트의 기한과 같은 틱 수에서 잡힘
    check("partition: resume waiter", tcb_resume(t(H)) == 0);
    super::yield_now();
    // 표를 건 시스템 콜과 대기 사이에는 시스템 콜이 없어야 함(둘 사이에 틱이 끼면
    // 기한이 한 틱 늦게 잡힘)
    check("partition: set table", part_set_table(ctl, &T8) == 0);
    check("partition: wait E", wait(st, 1));
    log(b'R');
    check("partition: wait E+1", wait(st, 1));
    log(b'r');
    check("partition: wait E+3", wait(st, 1));

    let n = LEN.load(Relaxed) as usize;
    let mut seen = [0u8; 16];
    for (i, c) in seen.iter_mut().enumerate().take(n.min(16)) {
        *c = LOG[i].load(Relaxed) as u8;
    }
    // 도메인 1의 창이 닫힌 뒤에 그 태스크가 실행됐으면 양보가 상대 없이 돌아왔음
    let alone = ALONE.load(Relaxed) != 0;
    let matched = n == EXPECT.len() && seen[..n] == *EXPECT && !alone;
    if !matched {
        put_str("root: partition window log ");
        // 로그는 ASCII 글자만 담음
        put_str(core::str::from_utf8(&seen[..n.min(16)]).unwrap_or("?"));
        put_str(if alone { " (a task ran outside its window)\n" } else { "\n" });
    }

    // 블록된 태스크(다른 도메인의 X의 자식 C, 도메인 0의 H), 큐에 서 있는 태스크(X와
    // V는 둘뿐인 큐의 앞과 뒤, Y2는 뒤에 Y1이 있는 큐의 앞)를 지움
    for i in [X, C, V, H, Y2] {
        check("partition: delete", cap_delete(t(i)) == 0);
    }
    // 복원. 도는 태스크가 있어야 기본 표에서 CPU가 놀지 않음
    check("partition: resume spinner", tcb_resume(t(S0)) == 0);
    check("partition: restore", part_set_table(ctl, &[(0, 1)]) == 0);
    let (y1, z, len) = (Y1_SPINS.load(Relaxed), Z_SPINS.load(Relaxed), LEN.load(Relaxed));
    check("partition: wait restore", wait(st, 2));
    if matched {
        check(
            "partition: restore not in force",
            Y1_SPINS.load(Relaxed) == y1 && Z_SPINS.load(Relaxed) == z && LEN.load(Relaxed) == len,
        );
    }
    // Y1은 큐의 끝에서, Z는 다른 도메인의 큐에서, S0는 도메인 0에서 지움
    for i in [Y1, Z, S0] {
        check("partition: delete", cap_delete(t(i)) == 0);
    }
    matched
}

/// 시간 분할 표 하나를 한 바퀴 돌리고 순서를 판정하는 함수입니다. 틀리면 한 번
/// 더 돌립니다.
///
/// # Arguments
/// `b` - 부팅 때 찾은 untyped와 그래뉼 크기
pub fn test_windows(b: &Boot) {
    let first_slot = NEXT_SLOT.load(Relaxed);
    let st = stage(b, 8);
    let ep_x = retype(st.ut, obj::ENDPOINT);
    let ep_v = retype(st.ut, obj::ENDPOINT);
    let ep_h = retype(st.ut, obj::ENDPOINT);
    check("partition: endpoints", ep_x > 0 && ep_v > 0 && ep_h > 0);
    EP_H.store(ep_h as u64, Relaxed);
    EP_X.store(ep_x as u64, Relaxed);
    EP_V.store(ep_v as u64, Relaxed);
    if !one(&st) {
        put_str("root: partition window retry\n");
        if !one(&st) {
            fail("partition: window order wrong twice");
        }
    }
    release_slots(first_slot);
    put_str("root: partition window pass\n");
}

/// 제어 케이퍼빌리티를 지우고 표를 더 바꿀 수 없는지 확인하는 함수입니다.
///
/// 다른 자가 검증은 이 케이퍼빌리티를 쓰지 않으므로 지운 뒤에 돌려도 됩니다.
pub fn seal() {
    let ctl = slot::PART_CTL as i64;
    check("partition: seal delete", super::cap_delete(ctl) == 0);
    check("partition: sealed table", part_set_table(ctl, &[(0, 1)]) == err::BAD_SLOT);
    check("partition: sealed domain", part_set_domain(ctl, slot::TCB as i64, 0) == err::BAD_SLOT);
    put_str("root: partition sealed\n");
}
