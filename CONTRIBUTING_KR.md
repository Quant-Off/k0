# K0 기여 안내

[English](CONTRIBUTING.md)

K0에 관심을 가져 주셔서 감사합니다. K0는 정식 릴리스 전 단계의 연구용 마이크로커널입니다. 무엇이든 수정하기 전에 [SECURITY_KR.md](SECURITY_KR.md)의 격리 모델을 먼저 읽어 주세요. 커널이 지금 무엇을 보장하고 무엇을 보장하지 않는지가 정리되어 있습니다.

이 문서는 영문판 [CONTRIBUTING.md](CONTRIBUTING.md)의 번역입니다. 두 문서가 다르면 영문판이 우선합니다.

## 시작하기 전에

- **보안 문제**는 공개 이슈나 풀 리퀘스트로 올리지 마세요. [SECURITY_KR.md](SECURITY_KR.md)의 절차를 따라 주세요.
- **작은 수정**(오타, 테스트 누락, 명백한 버그)은 바로 풀 리퀘스트를 열어도 됩니다.
- **그보다 큰 변경**은 이슈로 먼저 설계를 합의한 뒤에 코드를 작성해 주세요. 새 시스템 콜, 새 커널 오브젝트, 케이퍼빌리티 모델 변경, `unsafe` 코드나 어셈블리를 건드리는 모든 변경이 여기에 해당합니다.
- 바꾸려는 서브시스템의 **설계 문서**를 [design/](design/README_KR.md)에서 먼저 읽어 주세요. 각 문서 끝의 알려진 공백 절이 시작하기 좋은 지점입니다.

## 라이선스와 기여자 계약

K0는 [PolyForm Noncommercial License 1.0.0](LICENSE)을 따릅니다. 퀀트스페이스는 K0를 별도의 상업 라이선스로 제공할 수도 있습니다.

첫 풀 리퀘스트가 병합되기 전에 [기여자 라이선스 계약(CLA)](CLA.md)에 서명해야 합니다. 풀 리퀘스트에 아래 문장을 그대로 댓글로 달면 서명이 됩니다.

```
I have read the K0 CLA (CLA.md) and I agree to its terms for this and all my future contributions to K0.
```

계약서는 현재 법률 검토를 기다리는 초안입니다. 내용이 바뀌면 새 버전에 다시 서명해 달라는 요청을 받게 됩니다. 법적 효력은 영문 원문에만 있습니다.

## 환경 준비

K0는 완전히 오프라인으로 빌드됩니다. 서드파티 크레이트는 전부 `vendor/`에 들어 있고, `.cargo/config.toml`이 그 밖의 다운로드를 막습니다.

필요한 것은 다음과 같습니다.

- `rustup`. `rust-toolchain.toml`에 고정된 툴체인(Rust 1.89.0, `rust-src`, `llvm-tools`, `aarch64-unknown-none-softfloat` 타겟)은 처음 실행할 때 자동으로 설치됩니다.
- 펌웨어 ROM을 포함한 `qemu-system-aarch64`. Debian과 Ubuntu에서는 `qemu-system-arm`과 `ipxe-qemu` 패키지, macOS에서는 `brew install qemu`입니다.
- 빌드 스크립트용 호스트 C 컴파일러와 링커(`gcc` 또는 Xcode command line tools).

[.oss-scanner/Dockerfile](.oss-scanner/Dockerfile)이 이 환경 전체를 검증된 형태로 담고 있습니다.

## 빌드

```sh
cargo virt                                   # QEMU virt 빌드 후 부팅
cargo build -p k0-kernel --offline           # QEMU virt 이미지만 빌드
cargo apple                                  # Apple Silicon(m1n1) release 빌드
```

커널은 종료하지 않습니다. 루트 태스크가 자가 테스트를 마치면 계속 양보하고, 커널은 1초마다 `k0: tick N`을 출력합니다. QEMU는 `Ctrl-A X`로 끕니다.

## 테스트

풀 리퀘스트를 열기 전에 세 가지를 모두 실행해 주세요.

```sh
tools/host-test.sh                           # 호스트 단위 테스트와 DTB 퍼징
cargo build -p k0-kernel --offline
tools/qemu-boot-test.sh                      # QEMU 부팅과 자가 테스트 확인
```

`tools/host-test.sh`는 `k0-abi`, `k0-cap`, `k0-boot`의 테스트를 어느 호스트에서나 실행합니다. `k0-mm` 테스트는 AArch64 인라인 어셈블리를 포함하기 때문에 AArch64 호스트(Apple Silicon 또는 Linux arm64)에서만 실행되며, 스크립트가 4 KiB와 16 KiB 그래뉼 두 구성으로 모두 돌립니다.

`tools/qemu-boot-test.sh`는 디버그 커널을 부팅하고 부팅 로그를 정해진 순서대로 한 줄씩 확인합니다. 에러 표식이 하나라도 나오면 실패하고, 루트 태스크가 `root: sched tests pass`를 출력하면 곧바로 QEMU를 종료합니다. 느린 환경에서는 `K0_BOOT_TIMEOUT`(초)을 늘려 주세요.

기본 QEMU CPU 모델(`cortex-a72`)에는 PAC, PAN, RNDR이 없습니다. 이 경로까지 검증하려면 QEMU의 `max` 모델로 부팅하고 해당 로그 줄을 필수로 지정하세요. 커널 경로 뒤의 인자는 그대로 QEMU에 전달됩니다.

```sh
K0_BOOT_REQUIRE='rndr=on|k0: pac=on bti=present pan=on' \
  tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel -cpu max
```

### 영역별 테스트 위치

| 영역 | 호스트 단위 테스트 | 루트 태스크의 부팅 자가 테스트 |
| --- | --- | --- |
| `k0-abi` 상수와 공유 레이아웃 | 있음 | 간접적으로 |
| `k0-cap` untyped 부트스트랩과 retype | 있음 | 있음 |
| `k0-boot` DTB 파서, PAC 키 파생, 루트 태스크 이미지 | 있음, 퍼징 포함 | 있음 |
| `k0-mm` 페이지 테이블과 W^X 속성 | AArch64 호스트에서 있음 | 있음 |
| `k0-arch`, `k0-task`, `k0-sched`, `k0-ipc` | 없음 | 있음 |

`k0-arch`는 ELF 전용 어셈블러 지시어를 쓰고 스케줄러와 IPC 경로는 시스템 레지스터를 읽기 때문에 부팅으로만 검증됩니다. 이 부분을 바꿨다면 `userspace/root-task/src/main.rs`의 자가 테스트를 늘리고, 새 통과 줄을 `tools/qemu-boot-test.sh`의 기대 목록에 추가해 주세요.

### DTB 파서 퍼징

DTB 파서는 디바이스 트리를 신뢰하지 않는 입력으로 다룹니다. 퍼저는 추가 크레이트가 필요 없는 결정적 변이 퍼저입니다. 무작위 디바이스 트리를 만들어 파서가 생성기가 서술한 값을 정확히 돌려주는지 확인하고, 이어서 blob을 변이시켜 파서가 panic하지 않는지, 받아들인 결과가 상한을 지키는지 확인합니다. 커버리지 피드백은 쓰지 않습니다.

```sh
host="$(rustc -vV | sed -n 's/^host: //p')"
K0_FUZZ_ITERS=5000000 cargo test --release --offline --target "$host" -p k0-boot fuzz -- --nocapture
```

`K0_FUZZ_SEED`로 기준 시드를 바꿀 수 있습니다. 케이스가 실패하면 테스트가 `K0_FUZZ_CASE=0x...` 값을 출력합니다. 같은 명령에 이 변수를 지정하면 그 케이스만 재현하고 입력을 덤프합니다.

### CI와 같은 방식으로 실행

CI는 스캐너 이미지를 빌드한 뒤 네트워크를 끊은 컨테이너 안에서 테스트를 돌립니다. 빌드와 테스트에 저장소 밖의 무엇도 필요하지 않다는 것을 이 방식으로 증명합니다.

```sh
docker build --file .oss-scanner/Dockerfile --tag k0-ci .
docker run --rm --network none k0-ci tools/host-test.sh
docker run --rm --network none k0-ci tools/qemu-boot-test.sh
```

빌드 컨텍스트에 작업 디렉터리의 모든 파일이 들어가므로 깨끗한 체크아웃에서 실행해 주세요.

## 커널 변경 규칙

- 커널과 루트 태스크 크레이트에 **새 의존성을 추가하지 않습니다**. 정말 피할 수 없다면 이슈에서 먼저 논의하고, 그 뒤 vendor 디렉터리에 넣어 버전을 고정합니다.
- **모든 `unsafe` 블록**에는 그 지점에서 필요한 불변식이 왜 성립하는지 설명하는 `// SAFETY:` 주석이 있어야 합니다. "안전함"은 이유가 아닙니다.
- **Fail-secure.** 부팅 중 실패는 코어를 파킹하고, 실패한 시스템 콜은 에러를 반환하며 중간 상태를 남기지 않습니다.
- **커널 밖에서 온 입력은 전부 적대적으로 취급합니다.** DTB와 모든 시스템 콜 인자가 여기에 해당합니다. 검사 산술을 쓰고, 역참조 전에 검증합니다.
- **W^X를 지킵니다.** 어느 예외 레벨에서든 쓰기 가능하면서 실행 가능한 매핑은 없습니다.
- 동작을 바꾸면 **테스트를 함께 추가합니다**. 순수 로직은 호스트 단위 테스트로, 시스템 콜 동작은 루트 태스크 자가 테스트로 검증합니다.
- 이 저장소의 **코드 주석은 한국어**로 씁니다. 기여자는 영어로 써도 되고, 병합할 때 메인테이너가 번역할 수 있습니다.

## 커밋과 풀 리퀘스트

- 커밋 하나에는 논리적 변경 하나만 담습니다. 리뷰어가 커밋을 하나씩 따로 읽을 수 있어야 합니다.
- 제목은 짧고 평이하게 씁니다. 한국어와 영어 모두 괜찮습니다. `feat:`나 `fix:` 같은 접두어는 쓰지 않습니다.
- 풀 리퀘스트에는 무엇을, 왜 바꿨고, 어떻게 테스트했는지 적고, 관련 이슈가 있으면 연결합니다.
- CI를 통과해야 합니다. CI는 두 플랫폼 이미지를 빌드하고, x86-64와 AArch64에서 호스트 테스트를 돌리고, 두 가지 CPU 모델의 QEMU로 커널을 부팅합니다.

## AI 보조 기여

K0의 일부는 AI의 도움을 받아 작성되었고, 히스토리의 일부 커밋에는 AI 모델이 공동 작성자로 표기되어 있습니다. 기여에도 AI 도구를 쓸 수 있으며 조건은 다음과 같습니다.

- 제출하는 모든 줄을 이해하고 리뷰에서 설명할 수 있어야 합니다. 특히 `unsafe` 코드, 어셈블리, 신뢰 경계에 걸친 모든 코드가 그렇습니다.
- 풀 리퀘스트 설명에 AI의 도움을 받은 부분을 밝힙니다.
- [CLA](CLA.md)가 요구하는 대로 그 결과물을 제출할 권리가 있어야 합니다.

어떤 방식으로 만들어졌든 모든 변경은 병합 전에 메인테이너가 리뷰합니다.

## 행동 강령

K0에 참여하는 모든 사람은 [행동 강령](CODE_OF_CONDUCT.md)을 따라야 합니다. 공식 한국어 번역은 <https://www.contributor-covenant.org/ko/version/2/1/code_of_conduct/>에서 볼 수 있습니다.
