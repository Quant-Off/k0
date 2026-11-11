//! K0의 UEFI 로더입니다.
//!
//! # Features
//! UEFI 펌웨어가 실행하는 응용 프로그램으로, 내장한 커널 Image를 메모리에
//! 올리고 커널의 부트 계약(MMU 꺼짐, x0 = DTB 물리 주소, EL1 또는 EL2)을 만들어
//! 넘깁니다. 커널은 UEFI를 모릅니다. 하는 일은 다음 순서입니다.
//!
//! 1. 내장 Image의 헤더와 SHA-256(손상 검사)을 확인하고, 이 CPU가 그 커널의
//!    그래뉼을 지원하는지 봄
//! 2. 펌웨어가 구성 테이블로 준 디바이스 트리를 찾음. 없으면 거부
//! 3. 블록 하나를 할당해 `[DTB 버퍼 | 이미지 | 부트 윈도우 자리]`로 씀
//! 4. UEFI 메모리 맵에서 커널이 RAM으로 써도 되는 구간만 골라 DTB의 메모리
//!    기술을 다시 쓰고, 커널의 파서와 검증을 그 결과에 미리 돌림(k0-efi)
//! 5. 부트 서비스를 끝내고, 캐시를 정리하고, MMU를 끄고 커널로 점프
//!
//! 부트 서비스를 끝내기 전의 실패는 사유를 펌웨어 콘솔에 출력하고 오류를
//! 반환합니다(feature `refuse-halt`면 정지). 끝내기를 한 번이라도 시도한 뒤의
//! 실패는 펌웨어로 돌아가지 않고 정지합니다. 펌웨어가 이미 자신의 일부를
//! 정리했을 수 있기 때문입니다.
//!
//! 이 크레이트는 타겟 aarch64-unknown-uefi에서만 내용이 있습니다.

#![no_std]
#![no_main]

#[cfg(target_os = "uefi")]
mod efi;
#[cfg(target_os = "uefi")]
mod handoff;
#[cfg(target_os = "uefi")]
mod load;

#[cfg(not(target_os = "uefi"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
