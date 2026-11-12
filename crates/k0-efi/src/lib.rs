//! UEFI 로더의 순수 로직 크레이트입니다.
//!
//! # Features
//! 펌웨어가 준 입력(UEFI 메모리 맵, DTB)에서 커널에 넘길 DTB를 만드는 계산이
//! 전부 여기 있습니다. 펌웨어를 부르는 코드와 unsafe는 로더 바이너리에만 있고,
//! 이 크레이트는 바이트열에서 바이트열로 가는 함수뿐이라 호스트에서 단위
//! 테스트와 퍼징으로 검증합니다.
//!
//! - [memmap]: 메모리 맵에서 커널이 RAM으로 써도 되는 구간을 계획
//! - [fdtw]: DTB 작성기와, 펌웨어 DTB의 메모리 기술을 바꿔 쓰는 재작성기
//! - [verify]: 계획과 다른 방법으로 결과를 다시 확인
//! - [prepare]: 위를 묶고 커널의 파서와 검증을 미리 돌림
//! - [image]: 내장한 커널 Image의 헤더 확인
//! - [acpi], [synth]: 펌웨어가 디바이스 트리를 주지 않을 때 ACPI 정적 테이블에서
//!   디바이스를 읽어 트리를 만듦

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod acpi;
pub mod fdtw;
pub mod image;
pub mod memmap;
pub mod prepare;
pub mod synth;
pub mod verify;

#[cfg(test)]
mod tests;
