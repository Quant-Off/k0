//! 진입 페이즈 0의 `boot.S`를 어셈블리 진입 코드를 바이너리에 포함합니다.
//!
//! 별도 .S 파일 + `global_asm!(include_str!(...))` 방식을 쓰는 이유는 다음과 같습니다.
//! - 어셈블리를 순수 텍스트로 유지해 감사(audit) 시 diff가 깨끗함
//! - build.rs나 외부 어셈블러 의존 없이 rustc 단독으로 빌드 가능(폐쇄적/재현 가능 빌드에 유리)

core::arch::global_asm!(include_str!("arch/aarch64/boot.S"));
