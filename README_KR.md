# K0

[English](README.md)

AArch64 대상 마이크로커널입니다. seL4 방식의 케이퍼빌리티 자원 모델을 따르고 Rust `no_std`로 작성됐습니다. 커널에는 heap이 없습니다. 남는 물리 메모리 전부를 untyped 케이퍼빌리티로 첫 사용자 태스크에 넘기고, 부팅 이후 생기는 커널 오브젝트는 전부 그 태스크가 retype한 메모리 안에 자리 잡습니다. 그 밖의 커널 상태는 고정 크기 정적 저장소(예: 커널 자체 매핑용 페이지 테이블 32장 풀)뿐입니다. 1차 타겟은 QEMU virt(4K 그래뉼)와 Apple Silicon(16K 그래뉼)이고, AMD64를 비롯한 다른 아키텍처는 1차 타겟 완료 후 확장 대상입니다. 현재 버전은 정식 릴리스 전 단계인 0.1.0이며, 커널 쪽 코드는 주석을 포함해 Rust와 어셈블리 약 5,200줄이라 처음부터 끝까지 읽을 수 있는 분량입니다.

## 보안 상태

현재 버전(0.1.0)은 태스크 간 격리를 제공하지 않습니다. `TCB_CONFIGURE` / `TCB_RESUME`으로 만든 태스크는 루트 태스크의 스레드이며 격리된 프로세스가 아닙니다. 신뢰하지 않는 코드를 태스크로 실행하지 마세요. 자세한 내용과 신고 방법은 [SECURITY_KR.md](SECURITY_KR.md)([English](SECURITY.md))를 참고하세요.

## 지금 동작하는 것

- **부팅**: Linux arm64 Image 헤더로 QEMU와 m1n1에서 적재됩니다. 부트 코어 하나만 진행하고, EL2로 들어오면 EL1으로 내려가며, 지원하지 않는 진입 상태는 파킹합니다.
- **메모리**: 커널 매핑은 W^X이고 `SCTLR_EL1.WXN`이 켜져 있습니다. 부트 스택 아래 가드 페이지, higher-half 별칭, AT 명령을 이용한 부팅 시 자가 검사가 있습니다.
- **신뢰하지 않는 부트 입력**: DTB는 적대적 입력으로 파싱하고, 메모리 맵이 커널 MMIO 창과 겹치면 부팅을 거부합니다.
- **하드닝**: CPU가 지원하면 PAC 키를 파생해 넣고 PAN을 켜며, EL0에 노출되는 카운터, 디버그, PMU 접근을 막습니다. 현재 stable 컴파일러는 포인터 인증 명령을 만들지 않으므로 커널 복귀 주소는 아직 서명되지 않습니다.
- **케이퍼빌리티**: 루트 CNode 하나, untyped 메모리, `Frame` / `PageTable` / `TCB` / `Endpoint`로의 retype, 케이퍼빌리티로만 대상 주소 공간을 정하는 `MAP`.
- **태스크와 스케줄링**: `TCB_CONFIGURE`, `TCB_RESUME`, `YIELD`, `EXIT`, 타이머 선점이 있는 라운드 로빈.
- **IPC**: 레지스터로만 메시지를 옮기는 동기 랑데부 `SEND` / `RECV` / `CALL` / `REPLY_RECV`와 1회성 응답 자격.
- **폴트 격리**: EL0 폴트는 시스템 전체가 아니라 그 태스크만 종료합니다.
- **루트 태스크 무결성**: 내장 루트 태스크 이미지를 부팅 때 SHA-256으로 다시 검사합니다. 이것은 손상 검사이지 서명 검증이 아닙니다(이유는 [SECURITY_KR.md](SECURITY_KR.md)의 범위 절 참고).

위 항목은 전부 루트 태스크가 부팅할 때마다 자가 테스트로 확인합니다.

## 아직 없는 것

- 태스크별 케이퍼빌리티 공간과 `AddrSpace` retype
- 파생 계보(CDT)와 `MINT`(배지 각인, 권한 축소 복사), `revoke`
- IPC를 통한 케이퍼빌리티 전송(Grant)
- IRQ를 사용자 공간에 전달하는 Notification 오브젝트
- EL0 폴트를 핸들러 태스크에 IPC로 전달
- 루트 태스크가 분리 배포되는 시점의 Ed25519 공개키 검증
- Apple Silicon 실기 부팅 검증(현재는 빌드만 검증)
- AMD64 등 다른 아키텍처

## 빌드와 실행

```sh
cargo virt    # QEMU virt에서 부팅
cargo apple   # Apple Silicon(m1n1) release 빌드
```

`rustup`, 펌웨어 ROM을 포함한 `qemu-system-aarch64`, 호스트 C 컴파일러가 필요합니다. 의존성은 전부 vendor 디렉터리에 들어 있어 빌드에 네트워크가 필요 없습니다. 자세한 준비 과정은 [CONTRIBUTING_KR.md](CONTRIBUTING_KR.md)에 있습니다.

## 테스트

```sh
tools/host-test.sh                  # 호스트 단위 테스트와 DTB 퍼징
cargo build -p k0-kernel --offline
tools/qemu-boot-test.sh             # QEMU 부팅과 루트 태스크 자가 테스트 확인
```

CI는 x86-64와 AArch64에서 네트워크를 끊은 컨테이너 안에 같은 테스트를 돌리고, 매주 더 긴 퍼징을 실행합니다.

## 설계 문서

[설계 문서](design/README_KR.md)는 서브시스템마다 동작 방식, 그렇게 만든 이유, 테스트 방법, 아직 없는 것을 설명합니다.

## 저장소 구조

| 경로 | 내용 |
| --- | --- |
| `kernel/` | 커널 바이너리. 부트 어셈블리, 초기화 순서, 시스템 콜 정책, 링커 스크립트 |
| `crates/k0-abi` | 사용자 공간과 공유하는 시스템 콜 번호, 에러 코드, bootinfo 레이아웃 |
| `crates/k0-arch` | 예외 벡터, EL0 진입과 복귀, GICv3와 타이머, PAC과 PAN, 초기 콘솔 |
| `crates/k0-boot` | DTB 파서, 루트 태스크 이미지 내장과 무결성 검사, PAC 키 파생 |
| `crates/k0-cap` | 케이퍼빌리티, untyped 메모리, retype |
| `crates/k0-mm` | 커널 페이지 테이블, MMU 활성화, 사용자 주소 공간 |
| `crates/k0-task` | TCB, 엔드포인트, 루트 태스크 적재 |
| `crates/k0-sched` | 라운드 로빈 스케줄러 |
| `crates/k0-ipc` | 동기 IPC |
| `userspace/root-task` | 첫 사용자 태스크와 자가 테스트 |
| `tools/` | QEMU 러너, 부팅 테스트, 호스트 테스트 |
| `vendor/` | vendor 처리한 서드파티 크레이트(`sha2`와 그 의존성) |

## 기여

[CONTRIBUTING_KR.md](CONTRIBUTING_KR.md)를 참고하세요. 문서나 주석의 오타 또는 깨진 링크만 고치는 경우가 아니라면, 첫 기여가 병합되기 전에 [기여자 라이선스 계약](CLA.md)에 서명해야 합니다. 모든 참여자는 [행동 강령](CODE_OF_CONDUCT.md)을 따릅니다.

## 라이선스

K0는 [PolyForm Noncommercial License 1.0.0](LICENSE)을 따르는 소스 공개 소프트웨어입니다. 비상업적 목적의 사용, 수정, 재배포는 허용되며 라이선스 전문과 `Required Notice:` 줄(저작권 표기)을 함께 전달해야 합니다. 상업적 이용은 허용되지 않으며, 필요하면 <qtfelix@qu4nt.space>로 별도 라이선스를 문의하세요.

`vendor/` 아래의 서드파티 크레이트는 각자의 라이선스(MIT 또는 Apache-2.0)를 따릅니다.
