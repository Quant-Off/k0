# K0 사용법

<div align="center">

[English](USAGE.md) | 한국어

</div>

이 문서는 K0를 빌드하고 실행해서 제대로 동작하는지 확인하는 방법을 설명합니다. ARM 하드웨어는 필요 없습니다. 모든 것을 ARM 컴퓨터처럼 동작하는 프로그램인 QEMU에서 실행합니다.

## 준비물

- Rust 설치 도구 `rustup`. 첫 빌드 때 K0가 쓰는 컴파일러와 도구 묶음(툴체인: Rust 1.89.0과 `rust-toolchain.toml`에 적힌 구성 요소)을 알아서 내려받습니다.
- 펌웨어 파일을 포함한 `qemu-system-aarch64`. macOS에서는 `brew install qemu`, Debian이나 Ubuntu에서는 `qemu-system-arm`과 `ipxe-qemu` 패키지를 설치합니다.
- 빌드 스크립트용 C 컴파일러. macOS는 Xcode 명령줄 도구, 그 밖에는 `gcc`.

외부 라이브러리는 전부 `vendor/` 폴더에 들어 있어서, 툴체인을 한 번 설치한 뒤에는 빌드에 인터넷 연결이 필요 없습니다.

## K0 실행

```sh
cargo virt
```

커널을 빌드하고 QEMU에서 시작하며, 부팅 메시지가 터미널에 나타납니다. 자가 테스트가 끝나도 커널은 계속 돌면서 1초마다 `k0: tick N`을 출력합니다. 스스로 끝나지 않으므로 QEMU를 끄려면 `Ctrl-A`를 누른 다음 `X`를 누르세요.

### 출력 읽는 법

`k0:`로 시작하는 줄은 커널이, `root:`로 시작하는 줄은 첫 프로그램인 루트 태스크가 출력한 것입니다.

| 줄 | 뜻 |
| --- | --- |
| `root: hello from EL0` | 첫 프로그램이 사용자 모드에서 돌기 시작함 |
| `root: ... pass` | 자가 테스트 한 묶음이 통과함 |
| `root: secret at 0x...` 다음에 같은 주소의 `k0: task fault ... far 0x...` | 격리된 태스크가 루트 태스크만 볼 수 있는 메모리를 읽으려다 정확히 그 주소에서 멈춤 |
| `k0: task fault ...`와 `k0: unknown syscall 65535, killing task` | 테스트가 일부러 일으킨 규칙 위반. 매번 규칙을 어긴 태스크만 멈춤. 정상 실행에서는 `k0: task fault` 줄이 정확히 6개, `k0: unknown syscall` 줄이 1개 나옴 |
| `root: sched tests pass` | 마지막 자가 테스트 묶음이 통과함 |
| `k0: tick N` | 커널이 아직 돌고 있음 |

`root: FAIL`, `k0: FATAL`, `k0: PANIC`, `k0: EXCEPTION` 같은 줄은 문제가 생겼다는 뜻입니다. 아래의 자동 부팅 테스트가 전체 목록을 대신 확인해 줍니다.

### 다른 실행 방법

```sh
cargo virt --release                                          # 최적화 빌드
cargo virt -- -cpu max                                        # 보안 기능(PAC, PAN, RNDR)이 더 있는 최신 CPU 모델
cargo virt -- -machine virt,gic-version=3,virtualization=on   # QEMU가 커널을 하이퍼바이저 단계(EL2)에서 시작하게 함
cargo virt -- -m 4G                                           # 메모리 늘리기
```

`--` 뒤의 인자는 그대로 QEMU에 전달됩니다.

## 자동 테스트

```sh
tools/host-test.sh                    # 내 컴퓨터에서 단위 테스트와 디바이스 트리 퍼징
cargo build -p k0-kernel --offline    # 커널 빌드
tools/qemu-boot-test.sh               # QEMU에서 부팅하고 자가 테스트를 전부 확인
```

- `tools/host-test.sh`는 커널의 부분 하나하나를 내 컴퓨터에서 일반 프로그램처럼 돌리는 작은 테스트(단위 테스트)를 실행합니다. 퍼징도 합니다. 디바이스 트리 파서에 무작위로 망가뜨린 입력을 넣어 충돌을 찾는 방법입니다. 메모리 관리 테스트는 ARM 명령을 쓰기 때문에 ARM 컴퓨터(Apple Silicon Mac이나 Linux arm64)에서만 돕니다.
- `tools/qemu-boot-test.sh`는 커널을 부팅하고, 로그를 정해진 순서대로 한 줄씩 확인하고, 에러 줄이 하나라도 나오면 실패하고, 일부러 일으킨 폴트 줄의 개수를 정확히 맞춘 뒤 QEMU를 끕니다. 성공하면 `boot-test: pass`를 출력하고 전체 로그를 `target/qemu-boot-test.log`에 남깁니다.

QEMU의 기본 CPU 모델에는 PAC, PAN, RNDR이 없습니다. 이것까지 시험하려면 `max` 모델로 부팅하고 해당 로그 줄을 요구하세요.

```sh
K0_BOOT_REQUIRE='rndr=on|k0: pac=on bti=present pan=on' \
  tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel -cpu max
```

GitHub도 풀 리퀘스트와 `main` 푸시가 있을 때마다 이 테스트를 자동으로 돌립니다(CI). x86-64와 Arm64 Linux에서, 네트워크를 끈 밀폐된 빌드 환경(컨테이너) 안에서 돌리고, 매주 더 긴 퍼징도 돌립니다. 디바이스 트리 파서를 직접 퍼징하거나 CI와 똑같이 돌리는 방법은 [CONTRIBUTING_KR.md](CONTRIBUTING_KR.md)에 있습니다.

## Apple Silicon용 빌드

```sh
cargo apple
```

16 KiB 메모리 페이지를 쓰고 m1n1 부트로더가 읽어 들이는 Apple Silicon용 버전을 빌드합니다. 컴파일과 호스트 테스트만 했고, 실제 기기에서 부팅한 적은 없습니다.

`cargo apple`은 QEMU 최적화 빌드와 같은 파일(`target/aarch64-unknown-none-softfloat/release/k0-kernel`)에 결과를 씁니다. `cargo apple` 직후 그 파일을 QEMU에서 부팅하면(예를 들어 `tools/qemu-boot-test.sh`로) Apple 빌드라서 부팅되지 않습니다. 먼저 `cargo build -p k0-kernel --offline --release`로 다시 빌드하세요. `cargo virt --release`도 부팅 전에 다시 빌드합니다.

## 문제가 생기면

| 증상 | 해결 |
| --- | --- |
| `qemu-system-aarch64`를 찾을 수 없음 | "준비물"에 적힌 대로 QEMU를 설치 |
| `boot-test: kernel ELF not found` | 먼저 `cargo build -p k0-kernel --offline`로 빌드 |
| 느린 컴퓨터에서 부팅 테스트가 시간 초과 | 시간을 늘림. 예: `K0_BOOT_TIMEOUT=300 tools/qemu-boot-test.sh` |
| `cargo apple` 뒤 `target/.../release/`의 최적화 파일이 QEMU에서 부팅되지 않음 | 파일에 Apple 빌드가 들어 있음. `cargo build -p k0-kernel --offline --release`로 다시 빌드 |
