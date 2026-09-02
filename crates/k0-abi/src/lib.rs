//! 커널과 사용자 공간이 공유하는 ABI 정의 크레이트입니다.
//!
//! # Features
//! 시스템 콜 번호와 주소 공간 배치 상수를 담습니다. 의존성이 없는 최하단
//! 크레이트라서 커널 크레이트들과 root-task가 모두 여기에 의존합니다.

#![no_std]

/// TTBR1 higher-half 선형 별칭의 VA 오프셋 (T1SZ=16, 48비트 VA 전제)
///
/// 커널 내부 상수지만 k0-mm(매핑)과 k0-arch(MMIO VA 계산)가 함께 쓰기 때문에
/// 최하단인 이 크레이트에 둡니다. 링커 스크립트의 `__virt_offset`과 반드시
/// 일치해야 합니다.
pub const KERNEL_VA_OFFSET: u64 = 0xFFFF_0000_0000_0000;

/// 시스템 콜 번호(x8) 모듈입니다. 인자는 x0-x5, 반환값은 x0을 사용하고
/// IPC 수신 계열은 x1-x5로도 돌려받습니다.
pub mod syscall {
    /// 바이트 하나를 커널 디버그 콘솔로 출력
    ///
    /// x0 = Console 슬롯, x1 = 바이트(하위 8비트만 사용). 출력 가능한
    /// ASCII(0x20-0x7E)와 개행만 그대로 나가고 그 외 바이트(터미널 제어
    /// 문자 등)는 `?`로 바뀝니다. 성공 시 x0 = 0, 실패 시 음수 에러
    pub const DEBUG_PUTC: u64 = 0;
    /// 스케줄러 양보(현재는 단일 태스크라 즉시 복귀)
    pub const YIELD: u64 = 1;
    /// 태스크 종료, x0 = 종료 코드
    pub const EXIT: u64 = 2;
    /// untyped를 커널 오브젝트로 재분류(retype)
    ///
    /// x0 = untyped 슬롯, x1 = 오브젝트 타입([super::obj]). 성공 시 x0 =
    /// 새 케이퍼빌리티의 슬롯, 실패 시 음수 에러([super::err])
    pub const RETYPE: u64 = 3;
    /// 케이퍼빌리티를 AddrSpace 케이퍼빌리티가 가리키는 주소 공간에 매핑
    ///
    /// x0 = 슬롯, x1 = 사용자 VA, x2 = 권한([super::perm]), x3 = AddrSpace
    /// 슬롯. Frame은 리프 매핑을 만들고, PageTable은 x1 경로의 첫 빈
    /// 레벨에 설치되며 x2를 무시합니다. 대상 주소 공간은 언제나 제시한
    /// 케이퍼빌리티로만 정해집니다(현재 실행 중인 주소 공간이라는 암묵
    /// 권한 없음). 성공 시 x0 = 0, 실패 시 음수 에러([super::err])
    pub const MAP: u64 = 4;
    /// 재분류된 TCB의 진입 컨텍스트 구성
    ///
    /// x0 = TCB 슬롯, x1 = 진입점 VA(4바이트 정렬), x2 = 스택 최상단
    /// VA(16바이트 정렬), x3 = AddrSpace 슬롯. Inactive 상태에서 한 번만
    /// 허용됩니다. SPSR과 스레드 포인터(TPIDR_EL0 / TPIDRRO_EL0 = 0)는
    /// 커널이 강제합니다. 성공 시 x0 = 0, 실패 시 음수 에러
    pub const TCB_CONFIGURE: u64 = 5;
    /// 구성을 마친 TCB를 준비 큐에 넣어 실행 대상으로 만듦
    ///
    /// x0 = TCB 슬롯. Stopped 상태에서만 허용됩니다. 성공 시 x0 = 0,
    /// 실패 시 음수 에러
    pub const TCB_RESUME: u64 = 6;
    /// 엔드포인트로 메시지 전송(동기 랑데부, 커널 버퍼 없음)
    ///
    /// x0 = 엔드포인트 슬롯, x1 = 플래그([super::ipc]), x2-x5 = MR0-MR3.
    /// 대기 중인 수신자가 없으면 블록됩니다. 성공 시 x0 = 0
    pub const SEND: u64 = 7;
    /// 엔드포인트에서 메시지 수신
    ///
    /// x0 = 엔드포인트 슬롯, x1 = 플래그. 대기 중인 송신자가 없으면
    /// 블록됩니다. 성공 시 x0 = 0, x1 = 배지(예약, 현재 0), x2-x5 = MR0-MR3
    pub const RECV: u64 = 8;
    /// 전송과 응답 대기를 한 번의 트랩으로 묶은 원자적 호출
    ///
    /// 인자는 SEND와 같고, 성공 시 응답이 x2-x5로 돌아옵니다. 수신자가
    /// 응답 없이 종료하거나 응답 자격을 잃으면 x0 = NO_REPLY
    pub const CALL: u64 = 9;
    /// 보류된 CALL 호출자에게 응답한 뒤 이어서 다음 메시지 수신(서버 루프)
    ///
    /// 인자와 반환은 RECV와 같고 x2-x5가 먼저 응답으로 전달됩니다. 응답
    /// 자격은 1회성이라 전달 즉시 소멸하고, 보류된 응답이 없으면 응답
    /// 단계는 건너뜁니다
    pub const REPLY_RECV: u64 = 10;
}

/// IPC 플래그(x1) 모듈입니다.
pub mod ipc {
    /// 상대가 준비돼 있지 않으면 블록 대신 WOULD_BLOCK으로 즉시 복귀
    ///
    /// 타임아웃 없는 설계에서 유일한 비대기 수단입니다
    pub const NONBLOCK: u64 = 1;
}

/// 재분류(retype)로 만들 수 있는 오브젝트 타입 모듈입니다.
pub mod obj {
    /// 사용자 매핑 가능한 프레임 한 개(크기는 bootinfo의 frame_size)
    pub const FRAME: u64 = 1;
    /// 사용자 주소 공간의 중간 페이지 테이블 한 장
    pub const PAGE_TABLE: u64 = 2;
    /// 태스크 제어 블록(TCB)
    pub const TCB: u64 = 3;
    /// 동기 랑데부 IPC의 엔드포인트
    pub const ENDPOINT: u64 = 4;
}

/// Frame 매핑 권한 모듈입니다. 실행 가능한 조합은 제공하지 않습니다(W^X).
pub mod perm {
    /// EL0 읽기 전용
    pub const RO: u64 = 0;
    /// EL0 읽기/쓰기
    pub const RW: u64 = 1;
}

/// 시스템 콜 에러 코드(음수 i64) 모듈입니다.
pub mod err {
    /// 슬롯 번호가 범위 밖이거나 비어 있음
    pub const BAD_SLOT: i64 = -1;
    /// 해당 슬롯이 untyped가 아님
    pub const NOT_UNTYPED: i64 = -2;
    /// untyped의 남은 공간이 부족함
    pub const EXHAUSTED: i64 = -3;
    /// CNode 슬롯이 가득 참
    pub const OUT_OF_SLOTS: i64 = -4;
    /// 알 수 없는 오브젝트 타입
    pub const BAD_TYPE: i64 = -5;
    /// VA가 정렬/범위/null 가드 규칙을 위반함
    pub const BAD_VA: i64 = -6;
    /// 알 수 없는 권한 값
    pub const BAD_PERM: i64 = -7;
    /// 이 케이퍼빌리티는 이미 매핑/설치됨
    pub const ALREADY_MAPPED: i64 = -8;
    /// 경로의 중간 테이블이 없음(PageTable을 먼저 매핑할 것)
    pub const MISSING_TABLE: i64 = -9;
    /// 대상 위치에 이미 매핑이 있음
    pub const OVERLAP: i64 = -10;
    /// 커널 내부 자원(테이블 풀 등) 고갈
    pub const KERNEL_RESOURCE: i64 = -11;
    /// 해당 슬롯의 케이퍼빌리티는 매핑 대상이 아님
    pub const BAD_CAP: i64 = -12;
    /// 대상 오브젝트가 이 연산을 허용하지 않는 상태임
    pub const BAD_STATE: i64 = -13;
    /// 상대가 준비돼 있지 않음(NONBLOCK 요청의 즉시 복귀)
    pub const WOULD_BLOCK: i64 = -14;
    /// CALL의 수신자가 응답 없이 종료했거나 응답 자격을 잃음
    pub const NO_REPLY: i64 = -15;
}

/// 커널이 루트 태스크에 RO로 매핑해 주는 bootinfo 페이지 모듈입니다.
///
/// 페이지 선두에 [Header], 바로 뒤에 `cap_count`개의 [CapDesc] 배열이
/// 이어집니다. 내용은 이양 직전의 스냅샷이라 이후 재분류로 생긴
/// 케이퍼빌리티는 반영되지 않습니다(RETYPE 반환값으로 추적).
pub mod bootinfo {
    /// bootinfo 페이지의 사용자 VA(양 플랫폼 그래뉼 정렬)
    pub const VA: u64 = 0x0F00_0000;
    /// 레이아웃 버전(불일치 시 루트 태스크는 진행하면 안 됨)
    pub const VERSION: u64 = 1;

    /// bootinfo 페이지 선두의 헤더 구조체입니다.
    ///
    /// 커널과 루트 태스크가 레이아웃을 공유하므로 repr(C)로 고정합니다.
    #[repr(C)]
    pub struct Header {
        pub version: u64,
        pub frame_size: u64,
        pub cap_count: u64,
    }

    /// 케이퍼빌리티 슬롯 하나를 서술하는 구조체입니다.
    ///
    /// 배열 인덱스가 곧 CNode 슬롯 번호입니다. base와 size는 untyped에만
    /// 채워지고 나머지 종류는 0입니다(커널 내부 주소 노출 최소화).
    #[repr(C)]
    pub struct CapDesc {
        pub kind: u64,
        pub base: u64,
        pub size: u64,
    }

    /// [CapDesc]의 kind 값 모듈입니다.
    pub mod cap_kind {
        pub const EMPTY: u64 = 0;
        pub const TCB: u64 = 1;
        pub const ADDR_SPACE: u64 = 2;
        pub const UNTYPED: u64 = 3;
        pub const FRAME: u64 = 4;
        pub const PAGE_TABLE: u64 = 5;
        pub const ENDPOINT: u64 = 6;
        pub const CONSOLE: u64 = 7;
    }
}
