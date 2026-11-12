//! 커널과 사용자 공간이 공유하는 ABI 정의 크레이트입니다.
//!
//! # Features
//! 시스템 콜 번호와 주소 공간 배치 상수를 담습니다. 의존성이 없는 최하단
//! 크레이트라서 커널 크레이트들과 root-task가 모두 여기에 의존합니다.

#![cfg_attr(not(test), no_std)]

/// TTBR1 higher-half 선형 별칭의 VA 오프셋 (T1SZ=16, 48비트 VA 전제)
///
/// 커널 내부 상수지만 k0-mm(매핑)과 k0-arch(MMIO VA 계산)가 함께 쓰기 때문에
/// 최하단인 이 크레이트에 둡니다. 링커 스크립트의 `__virt_offset`과 반드시
/// 일치해야 합니다.
pub const KERNEL_VA_OFFSET: u64 = 0xFFFF_0000_0000_0000;

/// 커널 이미지가 매핑되는 고정 가상 주소입니다.
///
/// 이미지는 부트로더가 올려 둔 물리 주소와 무관하게 항상 이 주소에서 실행
/// 됩니다. 시스템 콜이나 bootinfo의 형식에는 관여하지 않는 배치 상수이며,
/// 링커 스크립트는 빌드 스크립트를 통해 이 값을 그대로 받습니다.
pub const KERNEL_IMAGE_VA: u64 = 0xFFFF_8000_0000_0000;

/// 선형 별칭(`PA + KERNEL_VA_OFFSET`)으로 다룰 수 있는 물리 주소의 상한입니다.
/// 별칭이 이미지의 가상 주소 영역과 겹치지 않는 한계이며, 이보다 높은 RAM을
/// 가진 시스템은 지원하지 않습니다.
pub const PA_CEILING: u64 = KERNEL_IMAGE_VA - KERNEL_VA_OFFSET;

/// 시스템 콜 번호(x8) 모듈입니다. 인자는 x0-x5, 반환값은 x0을 사용하고
/// IPC 수신 계열은 x1-x5로도 돌려받습니다.
///
/// 슬롯 번호 인자는 언제나 호출한 태스크 자신의 CNode 안에서 해석됩니다.
/// 다른 태스크의 케이퍼빌리티를 번호로 가리킬 방법은 없습니다. 0번 슬롯은
/// null이라 어떤 인자로도 쓸 수 없습니다.
pub mod syscall {
    /// 바이트 하나를 커널 디버그 콘솔로 출력
    ///
    /// x0 = Console 슬롯(쓰기 권한 W 필요), x1 = 바이트(하위 8비트만 사용).
    /// 출력 가능한 ASCII(0x20-0x7E)와 개행만 그대로 나가고 그 외 바이트(터미널
    /// 제어 문자 등)는 `?`로 바뀝니다. `console-input`으로 빌드한 이미지에서
    /// 읽기 권한 R도 있는 케이퍼빌리티로 부르면 백스페이스(0x08)가 그대로
    /// 나갑니다(입력한 글자를 지우는 데 필요). 성공 시 x0 = 0, 실패 시 음수 에러
    pub const DEBUG_PUTC: u64 = 0;
    /// 스케줄러 양보(준비된 다른 태스크가 없으면 즉시 복귀)
    pub const YIELD: u64 = 1;
    /// 태스크 종료, x0 = 종료 코드
    pub const EXIT: u64 = 2;
    /// untyped를 커널 오브젝트로 재분류(retype)
    ///
    /// x0 = untyped 슬롯, x1 = 오브젝트 타입([super::obj]), x2 = 새
    /// 케이퍼빌리티를 받을 빈 슬롯. 목적지를 호출자가 지정하므로 커널은
    /// 빈 슬롯을 찾지 않습니다(상수 시간). untyped에서 파생된 케이퍼빌리티가
    /// 하나도 남아 있지 않으면 그 메모리를 처음부터 재사용합니다. 성공 시
    /// x0 = 0, 실패 시 음수 에러([super::err])
    pub const RETYPE: u64 = 3;
    /// 케이퍼빌리티를 AddrSpace 케이퍼빌리티가 가리키는 주소 공간에 매핑
    ///
    /// x0 = 슬롯, x1 = 사용자 VA, x2 = 권한([super::perm]), x3 = AddrSpace
    /// 슬롯. Frame은 리프 매핑을 만들고 요청한 권한을 케이퍼빌리티의 권한
    /// 비트가 덮어야 합니다. PageTable은 x1 경로의 첫 빈 레벨에 설치되며
    /// x2를 무시합니다. 대상 주소 공간은 언제나 제시한 케이퍼빌리티로만
    /// 정해집니다(현재 실행 중인 주소 공간이라는 암묵 권한 없음). 성공 시
    /// x0 = 0, 실패 시 음수 에러([super::err])
    pub const MAP: u64 = 4;
    /// 재분류된 TCB의 진입 컨텍스트와 소속 구성
    ///
    /// x0 = TCB 슬롯, x1 = 진입점 VA(4바이트 정렬), x2 = 스택 최상단
    /// VA(16바이트 정렬), x3 = AddrSpace 슬롯, x4 = CNode 슬롯, x5 = 태스크가
    /// x0으로 받을 인자. Inactive 상태에서 한 번만 허용됩니다. 새 태스크는
    /// x3의 주소 공간에서 실행되고 x4의 CNode만을 자기 케이퍼빌리티 공간으로
    /// 갖습니다. SPSR과 스레드 포인터(TPIDR_EL0 / TPIDRRO_EL0 = 0)는 커널이
    /// 강제합니다. 성공 시 x0 = 0, 실패 시 음수 에러
    pub const TCB_CONFIGURE: u64 = 5;
    /// 구성을 마친 TCB를 준비 큐에 넣어 실행 대상으로 만듦
    ///
    /// x0 = TCB 슬롯. Stopped 상태에서만 허용됩니다. 성공 시 x0 = 0,
    /// 실패 시 음수 에러
    pub const TCB_RESUME: u64 = 6;
    /// 엔드포인트로 메시지 전송(동기 랑데부, 커널 버퍼 없음)
    ///
    /// x0 = 엔드포인트 슬롯(송신 권한 W 필요), x1 = 플래그([super::ipc]),
    /// x2-x5 = MR0-MR3. 대기 중인 수신자가 없으면 블록됩니다. 성공 시 x0 = 0
    pub const SEND: u64 = 7;
    /// 엔드포인트에서 메시지 수신
    ///
    /// x0 = 엔드포인트 슬롯(수신 권한 R 필요), x1 = 플래그. 대기 중인
    /// 송신자가 없으면 블록됩니다. 성공 시 x0 = 0, x1 = 송신자가 쓴
    /// 케이퍼빌리티의 배지, x2-x5 = MR0-MR3
    pub const RECV: u64 = 8;
    /// 전송과 응답 대기를 한 번의 트랩으로 묶은 원자적 호출
    ///
    /// 인자는 SEND와 같고, 성공 시 x1 = 0이고 응답이 x2-x5로 돌아옵니다.
    /// 수신자가 응답 없이 종료하거나 응답 자격을 잃으면 x0 = NO_REPLY
    pub const CALL: u64 = 9;
    /// 보류된 CALL 호출자에게 응답한 뒤 이어서 다음 메시지 수신(서버 루프)
    ///
    /// 인자와 반환은 RECV와 같고 x2-x5가 먼저 응답으로 전달됩니다. 응답
    /// 자격은 1회성이라 전달 즉시 소멸하고, 보류된 응답이 없으면 응답
    /// 단계는 건너뜁니다
    pub const REPLY_RECV: u64 = 10;
    /// Frame 매핑을 해제하거나 PageTable을 주소 공간에서 떼어 냄
    ///
    /// x0 = 슬롯. 떼어 낸 PageTable은 소거되므로 그 아래에 걸려 있던 매핑은
    /// 함께 사라집니다. 매핑돼 있지 않으면 BAD_STATE. 성공 시 x0 = 0
    pub const UNMAP: u64 = 11;
    /// 케이퍼빌리티를 다른 슬롯으로 복사(파생)
    ///
    /// x0 = 원본 슬롯, x1 = 목적지 CNode를 가리키는 CNode 슬롯, x2 = 그
    /// CNode 안의 빈 목적지 슬롯, x3 = 남길 권한 비트([super::rights]),
    /// x4 = 배지(0 = 붙이지 않음). 권한은 줄일 수만 있고 Frame / Endpoint /
    /// Console에만 적용됩니다. 배지는 배지가 없는 Endpoint에만 붙일 수 있습니다. 복사본은
    /// 원본의 자식이라 원본을 회수하거나 삭제하면 함께 사라집니다. Untyped와
    /// PageTable은 복사할 수 없습니다. 성공 시 x0 = 0
    pub const CAP_COPY: u64 = 12;
    /// 케이퍼빌리티를 다른 슬롯으로 이동
    ///
    /// x0 = 원본 슬롯, x1 = 목적지 CNode를 가리키는 CNode 슬롯, x2 = 그
    /// CNode 안의 빈 목적지 슬롯. 파생 관계와 매핑 상태가 그대로
    /// 따라갑니다. 성공 시 x0 = 0
    pub const CAP_MOVE: u64 = 13;
    /// 케이퍼빌리티를 그 자손과 함께 삭제
    ///
    /// x0 = 슬롯. 오브젝트의 원본 케이퍼빌리티(재분류가 만든 것)를 삭제하면
    /// 그 오브젝트가 종료됩니다. 성공 시 x0 = 0
    pub const CAP_DELETE: u64 = 14;
    /// 케이퍼빌리티에서 파생된 것을 전부 회수(자신은 남김)
    ///
    /// x0 = 슬롯. untyped를 회수하면 거기서 잘라낸 오브젝트가 전부 종료되고
    /// 메모리를 다시 재분류할 수 있습니다. 성공 시 x0 = 0
    pub const CAP_REVOKE: u64 = 15;
    /// 콘솔 입력에서 바이트 하나를 읽음(커널을 `console-input`으로 빌드한
    /// 이미지에만 있음)
    ///
    /// x0 = Console 슬롯(읽기 권한 R 필요), x1 = 플래그([super::console]).
    /// 받아 둔 바이트가 없으면 올 때까지 블록됩니다. 한 번에 한 태스크만
    /// 기다릴 수 있습니다. 성공 시 x0 = 0, x1 = 바이트, x2 = 이 바이트 바로
    /// 앞에서 입력이 유실됐으면 [super::console::LOST], 아니면 0. 커널은
    /// 바이트를 해석하지 않습니다. 기다리는 동안 그 케이퍼빌리티를 잃으면
    /// REVOKED로 깨어납니다
    pub const CONSOLE_READ: u64 = 16;
}

/// CONSOLE_READ의 플래그(x1)와 결과(x2) 모듈입니다.
pub mod console {
    /// 받아 둔 바이트가 없으면 블록 대신 WOULD_BLOCK으로 즉시 복귀
    pub const NONBLOCK: u64 = 1;
    /// 읽기 전에, 지금까지 받아 둔 입력과 유실 표시를 전부 버림
    ///
    /// 프롬프트를 내기 전에 들어온 입력이 명령으로 읽히지 않게 하는 데
    /// 씁니다. 버리는 양은 커널의 버퍼와 장치의 FIFO 크기로 묶여 있습니다
    pub const FLUSH: u64 = 2;
    /// 플래그 레지스터에서 뜻이 정해지지 않은 비트. 하나라도 서 있으면
    /// BAD_FLAGS로 거부됩니다. 상위 32비트는 IPC와 같은 형식의 타임아웃입니다
    /// ([super::ipc::TIMEOUT_SHIFT])
    pub const RESERVED: u64 = 0xFFFF_FFFC;
    /// 결과(x2): 이 바이트 바로 앞에서 입력이 유실됨(장치의 FIFO가 넘쳤거나
    /// 오류가 난 문자를 버렸음)
    pub const LOST: u64 = 1;
}

/// IPC 플래그(x1) 모듈입니다.
pub mod ipc {
    /// 상대가 준비돼 있지 않으면 블록 대신 WOULD_BLOCK으로 즉시 복귀
    ///
    /// 타임아웃과 함께 주면 이 플래그가 우선합니다
    pub const NONBLOCK: u64 = 1;
    /// 플래그 레지스터에서 타임아웃이 시작하는 비트 위치
    ///
    /// 상위 32비트가 타임아웃이고 단위는 스케줄러 틱입니다. 0은 기한 없는
    /// 대기입니다. n을 주면 호출 뒤 n+1번째 틱에서 대기가 끝나므로, 틱이
    /// 제때 처리되는 한 대기는 틱 주기의 n배에서 n+1배 사이입니다. 커널이
    /// 인터럽트를 막은 채 오래 실행되면 틱이 늦어지고 대기도 그만큼
    /// 길어집니다. 기한은 틱 수로 세는 하한이지 벽시계 시간이 아닙니다.
    /// 기한이 지나면 TIMEOUT 또는 REPLY_TIMEOUT 에러로 복귀합니다. EL0가
    /// 얻는 유일한 시간 정보라서 틱보다 정밀하지 않습니다
    pub const TIMEOUT_SHIFT: u32 = 32;
    /// 플래그 레지스터에서 뜻이 정해지지 않은 비트. 하나라도 서 있으면
    /// BAD_FLAGS로 거부됩니다
    pub const RESERVED: u64 = 0xFFFF_FFFE;

    /// 타임아웃 틱 수를 플래그 레지스터 값으로 바꾸는 함수입니다.
    ///
    /// # Arguments
    /// `ticks` - 기다릴 틱 수(0 = 기한 없음)
    pub const fn timeout(ticks: u32) -> u64 {
        (ticks as u64) << TIMEOUT_SHIFT
    }
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
    /// 케이퍼빌리티 슬롯 배열(슬롯 수 = frame_size / 64, 0번은 null)
    pub const CNODE: u64 = 5;
    /// 비어 있는 사용자 주소 공간
    pub const ADDR_SPACE: u64 = 6;
}

/// Frame 매핑 권한 모듈입니다. 쓰기와 실행을 함께 주는 조합은 없습니다(W^X).
pub mod perm {
    /// EL0 읽기 전용
    pub const RO: u64 = 0;
    /// EL0 읽기/쓰기
    pub const RW: u64 = 1;
    /// EL0 읽기/실행. 실행 권한(X)이 있는 Frame 케이퍼빌리티로만 가능하고,
    /// 그런 케이퍼빌리티는 부트 시점의 텍스트 프레임에만 존재합니다
    pub const RX: u64 = 2;
}

/// 케이퍼빌리티 권한 비트 모듈입니다. (CAP_COPY의 x3)
pub mod rights {
    /// Frame: 읽기 매핑, Endpoint: 수신, Console: 입력 읽기
    pub const R: u64 = 1;
    /// Frame: 쓰기 매핑, Endpoint: 송신, Console: 출력
    pub const W: u64 = 2;
    /// Frame: 실행 매핑
    pub const X: u64 = 4;
    /// 원본의 권한을 그대로 유지
    pub const ALL: u64 = R | W | X;
}

/// 시스템 콜 에러 코드(음수 i64) 모듈입니다.
pub mod err {
    /// 슬롯 번호가 0이거나 범위 밖이거나 비어 있음
    pub const BAD_SLOT: i64 = -1;
    /// 해당 슬롯이 untyped가 아님
    pub const NOT_UNTYPED: i64 = -2;
    /// untyped의 남은 공간이 부족함
    pub const EXHAUSTED: i64 = -3;
    /// 목적지 슬롯이 비어 있지 않음
    pub const OCCUPIED: i64 = -4;
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
    /// 커널이 오브젝트의 메모리를 준비하지 못함. 별칭 테이블과 주소 공간
    /// 등록부는 부팅 때 메모리 크기에 맞춰 잡으므로 untyped를 쥔 태스크가
    /// 이 에러를 일으킬 수는 없고, 커널 쪽 한계에 닿았을 때만 나옴
    pub const KERNEL_RESOURCE: i64 = -11;
    /// 해당 슬롯의 케이퍼빌리티는 이 연산의 대상이 아님
    pub const BAD_CAP: i64 = -12;
    /// 대상 오브젝트가 이 연산을 허용하지 않는 상태임
    pub const BAD_STATE: i64 = -13;
    /// 상대가 준비돼 있지 않음(NONBLOCK 요청의 즉시 복귀)
    pub const WOULD_BLOCK: i64 = -14;
    /// CALL의 수신자가 응답 없이 종료했거나 응답 자격을 잃음
    pub const NO_REPLY: i64 = -15;
    /// 케이퍼빌리티에 이 연산에 필요한 권한 비트가 없음
    pub const NO_RIGHTS: i64 = -16;
    /// 대기하던 엔드포인트가 종료됨. 콘솔 입력을 기다리던 중이라면 그
    /// Console 케이퍼빌리티를 잃었음
    pub const REVOKED: i64 = -17;
    /// 배지를 붙일 수 없는 케이퍼빌리티이거나 이미 다른 배지가 있음
    pub const BAD_BADGE: i64 = -18;
    /// 파생 깊이 상한 초과
    pub const TOO_DEEP: i64 = -19;
    /// IPC 대기가 기한 안에 상대를 만나지 못하고 끝남. 메시지는 전달되지
    /// 않았음
    pub const TIMEOUT: i64 = -20;
    /// CALL의 메시지는 수신자에게 전달됐지만 기한 안에 응답이 오지 않음.
    /// 수신자가 요청을 처리했는지는 알 수 없고 뒤늦은 응답은 버려짐
    pub const REPLY_TIMEOUT: i64 = -21;
    /// IPC 플래그 레지스터에 뜻이 정해지지 않은 비트가 서 있음
    pub const BAD_FLAGS: i64 = -22;
    /// 다른 태스크가 이미 콘솔 입력을 기다리는 중임
    pub const BUSY: i64 = -23;
    /// 이 이미지나 이 기계에 콘솔 입력 경로가 없거나, 커널이 그 경로를 껐음
    pub const NO_DEVICE: i64 = -24;
}

/// 커널이 루트 태스크에 RO로 매핑해 주는 bootinfo 페이지 모듈입니다.
///
/// 페이지 선두에 [Header], 바로 뒤에 `cap_count`개의 [CapDesc] 배열이
/// 이어집니다. 배열 인덱스가 곧 루트 CNode의 슬롯 번호이고, `cap_count`
/// 이상의 슬롯은 전부 비어 있습니다. 내용은 이양 직전의 스냅샷이라 이후의
/// 변화는 반영되지 않습니다.
pub mod bootinfo {
    /// bootinfo 페이지의 사용자 VA(양 플랫폼 그래뉼 정렬)
    pub const VA: u64 = 0x0F00_0000;
    /// 레이아웃 버전(불일치 시 루트 태스크는 진행하면 안 됨)
    ///
    /// 버전 4부터 Console 케이퍼빌리티에 권한 비트가 있습니다(R = 입력 읽기,
    /// W = 출력). 디스크립터의 `rights`가 그 값을 알립니다
    pub const VERSION: u64 = 4;

    /// 루트 태스크의 진입 레지스터 x4로 오는 시드 정보에서, 시드에 들어간
    /// `rng-seed` 바이트 수를 담는 비트
    ///
    /// 버전 3부터 커널은 부트 엔트로피에서 파생한 32바이트 시드를 루트
    /// 태스크의 진입 레지스터 x0-x3에(리틀 엔디언 워드 넷), 그 재료에 관한
    /// 정보를 x4에 줍니다. 시드는 bootinfo 페이지에 두지 않습니다. 페이지는
    /// 루트 태스크가 사는 동안 읽기 전용으로 매핑돼 있기 때문입니다. 시드의
    /// 품질은 재료보다 나을 수 없으므로 받는 쪽은 이 정보를 보고 판단해야
    /// 합니다
    pub const SEED_RNG_SEED_BYTES: u64 = 0xFFFF;
    /// 시드 정보에서, RNDR 난수 256비트가 재료에 들어갔음을 뜻하는 비트
    pub const SEED_RNDR: u64 = 1 << 16;

    /// 루트 CNode에서 위치가 고정된 케이퍼빌리티의 슬롯 번호 모듈입니다.
    pub mod slot {
        /// 루트 태스크 자신의 TCB
        pub const TCB: u64 = 1;
        /// 루트 태스크의 주소 공간
        pub const ADDR_SPACE: u64 = 2;
        /// 디버그 콘솔
        pub const CONSOLE: u64 = 3;
        /// 루트 CNode 자신(복사와 이동의 목적지로 쓰임)
        pub const CNODE: u64 = 4;
    }

    /// bootinfo 페이지 선두의 헤더 구조체입니다.
    ///
    /// 커널과 루트 태스크가 레이아웃을 공유하므로 repr(C)로 고정합니다.
    #[repr(C)]
    pub struct Header {
        pub version: u64,
        pub frame_size: u64,
        /// 디스크립터 수이자 첫 빈 슬롯 번호
        pub cap_count: u64,
        /// 루트 CNode의 전체 슬롯 수
        pub cnode_slots: u64,
    }

    /// 케이퍼빌리티 슬롯 하나를 서술하는 구조체입니다.
    ///
    /// untyped는 `base` / `size`가 물리 구간입니다. 루트 태스크 이미지의
    /// Frame은 `base`가 그 프레임이 루트 주소 공간에 매핑된 VA, `size`가
    /// 프레임 크기, `rights`가 권한 비트([super::rights])입니다. Console은
    /// `rights`만 채웁니다. 나머지 종류는 전부 0입니다(커널 내부 주소 노출
    /// 최소화).
    #[repr(C)]
    pub struct CapDesc {
        pub kind: u64,
        pub base: u64,
        pub size: u64,
        pub rights: u64,
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
        pub const CNODE: u64 = 8;
    }
}

#[cfg(test)]
mod tests;
