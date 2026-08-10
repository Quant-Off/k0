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

/// 시스템 콜 번호(x8) 모듈입니다. 인자와 반환값은 x0을 사용합니다.
pub mod syscall {
    /// 바이트 하나를 커널 콘솔로 출력, x0 = 바이트(하위 8비트만 사용)
    pub const DEBUG_PUTC: u64 = 0;
    /// 스케줄러 양보(현재는 단일 태스크라 즉시 복귀)
    pub const YIELD: u64 = 1;
    /// 태스크 종료, x0 = 종료 코드
    pub const EXIT: u64 = 2;
}
