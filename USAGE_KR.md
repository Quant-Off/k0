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
cargo virt --release                                                     # 최적화 빌드
K0_QEMU_CPU=max cargo virt                                               # 보안 기능(PAC, PAN, RNDR)이 더 있는 최신 CPU 모델
K0_QEMU_MACHINE=virt,gic-version=3,virtualization=on cargo virt          # QEMU가 커널을 하이퍼바이저 단계(EL2)에서 시작하게 함
K0_QEMU_MACHINE=virt,gic-version=2 cargo virt                            # GICv2 인터럽트 컨트롤러
K0_QEMU_MEM=4G cargo virt                                                # 메모리 늘리기
```

러너 스크립트 `tools/qemu-virt-runner.sh`가 이 변수들로 QEMU 명령줄을 만듭니다. 변수를 빈 값으로 두면 해당 QEMU 옵션을 뺍니다.

| 변수 | 기본값 |
| --- | --- |
| `K0_QEMU_MACHINE` | `virt,gic-version=3` |
| `K0_QEMU_CPU` | `cortex-a72` |
| `K0_QEMU_SMP` | `1` |
| `K0_QEMU_MEM` | `512M` |
| `K0_QEMU_ACCEL` | 없음(QEMU의 소프트웨어 에뮬레이션, TCG) |
| `K0_IMAGE_TEXT_OFFSET` | 없음. 이미지 헤더의 `text_offset`을 바꾼 사본으로 부팅해서 같은 이미지를 다른 물리 주소에 적재함 |
| `K0_QEMU_MONITOR` | 없음. QEMU 모니터용 유닉스 소켓 경로로, 메모리에서 부트 로그를 읽을 때 씀 |

`--` 뒤의 인자도 여전히 그대로 QEMU에 전달됩니다.

### 16 KiB 빌드

커널은 기본으로 4 KiB 메모리 페이지를 씁니다. 16 KiB 버전은 빌드 옵션이며 실행 중에 고를 수 없습니다.

```sh
cargo build -p k0-kernel --offline --no-default-features --features granule-16k --target-dir target/g16
K0_QEMU_CPU=max tools/qemu-boot-test.sh target/g16/aarch64-unknown-none-softfloat/debug/k0-kernel
```

4 KiB feature가 기본으로 켜져 있고 둘이 함께 켜지면 빌드가 멈추기 때문에 `--no-default-features`가 필요합니다. `--target-dir`은 16 KiB 이미지를 일반 이미지와 따로 둡니다. 16 KiB 이미지는 16 KiB 페이지를 지원하지 않는 CPU(예: `cortex-a72`)에서는 부팅을 거부합니다.

### 고정 빌드

이미지를 한 플랫폼에 묶으려면, 타겟을 한 번 부팅해서 부트 로그의 `k0: platform ` 뒤에 나오는 글자를 복사하고 `K0_PIN`에 넣어 빌드합니다.

```sh
K0_PIN='<"k0: platform " 뒤의 글자>' cargo build -p k0-kernel --offline --target-dir target/pin
```

고정한 이미지는 찾아낸 플랫폼, 진입 단계, RAM 범위가 다르면 부팅을 거부합니다. 보드를 잘못 골랐거나 디바이스 트리를 바꿔치기한 경우를 잡아냅니다. 비교에 쓰는 값이 디바이스 트리에서 나오므로, 악의적인 부트 체인을 막는 방어는 아닙니다.

### 메모리에서 부트 로그 읽기

커널이 출력하는 모든 줄은 메모리의 16 KiB 버퍼에도 남습니다. 커널이 부팅을 거부했거나 지원하는 콘솔이 없을 때는 이 버퍼가 유일한 기록입니다. QEMU를 `K0_QEMU_MONITOR=<소켓 경로>`로 시작했다면 다음으로 읽습니다.

```sh
tools/boot-log.py <커널 ELF> <적재 주소> <QEMU 모니터 소켓>
```

적재 주소는 이미지를 올린 물리 주소입니다. 이미지를 고치지 않은 QEMU `virt`에서는 `0x40080000`입니다.

## 자동 테스트

```sh
tools/host-test.sh                    # 내 컴퓨터에서 단위 테스트와 디바이스 트리 퍼징
cargo build -p k0-kernel --offline    # 커널 빌드
tools/qemu-boot-test.sh               # QEMU에서 부팅하고 자가 테스트를 전부 확인
tools/boot-matrix.sh                  # 지원하는 구성을 전부 부팅(16 KiB 이미지와 고정 이미지도 빌드함)
```

- `tools/host-test.sh`는 호스트 테스트 183개를 실행합니다(x86-64 컴퓨터에서는 123개). 커널의 부분 하나하나를 내 컴퓨터에서 일반 프로그램처럼 돌리는 작은 테스트(단위 테스트)입니다. 퍼징도 합니다. 디바이스 트리 파서에 무작위로 망가뜨린 입력을 넣어 충돌을 찾는 방법입니다. 메모리 관리 테스트는 ARM 명령을 쓰기 때문에 ARM 컴퓨터(Apple Silicon Mac이나 Linux arm64)에서만 돕니다.
- `tools/qemu-boot-test.sh`는 커널을 부팅하고, 로그를 정해진 순서대로 한 줄씩 확인하고, 에러 줄이 하나라도 나오면 실패하고, 일부러 일으킨 폴트 줄의 개수를 정확히 맞춘 뒤 QEMU를 끕니다. 성공하면 `boot-test: pass`를 출력하고 전체 로그를 `target/qemu-boot-test.log`에 남깁니다. 머신은 위의 `K0_QEMU_*` 변수에서 가져오며, 맞는 크기의 untyped 영역이 없는 메모리 배치에서는 루트 태스크가 재사용 시험을 건너뛰므로 그때는 `K0_BOOT_REUSE=skip`을 받습니다.
- `tools/boot-matrix.sh`는 K0가 지원한다고 말하는 구성마다 커널을 부팅해서 결과를 확인합니다. GICv3와 GICv2의 QEMU virt를 EL1과 EL2로, CPU 모델 세 가지, 코어 4개, 16 KiB 이미지, 다른 적재 주소, `xlnx-versal-virt`, 고정 이미지, 이미지가 물리 타이머를 건드리지 않는지 정적 검사입니다. 거부해야 하는 구성은 메모리에 남은 부트 로그에서 사유를 읽습니다. 행 이름을 인자로 주면 그 행만 돌립니다. CI가 이것을 실행합니다. Apple Silicon Mac에서는 `K0_MATRIX_HVF=1 tools/boot-matrix.sh`로 QEMU의 HVF 가속기를 통해 실제 CPU 코어에서 커널을 돌리는 행을 더할 수 있습니다.

QEMU의 기본 CPU 모델에는 PAC, PAN, RNDR이 없습니다. 이것까지 시험하려면 `max` 모델로 부팅하고 해당 로그 줄을 요구하세요.

```sh
K0_QEMU_CPU=max K0_BOOT_REQUIRE='rndr=on|k0: pac=on bti=present pan=on' \
  tools/qemu-boot-test.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel
```

GitHub도 풀 리퀘스트와 `main` 푸시가 있을 때마다 이 테스트를 자동으로 돌립니다(CI). x86-64와 ARM64 Linux에서, 네트워크를 끈 밀폐된 빌드 환경(컨테이너) 안에서 돌리고, 매주 더 긴 퍼징도 돌립니다. 디바이스 트리 파서를 직접 퍼징하거나 CI와 똑같이 돌리는 방법은 [CONTRIBUTING_KR.md](CONTRIBUTING_KR.md)에 있습니다.

## 문제가 생기면

| 증상 | 해결 |
| --- | --- |
| `qemu-system-aarch64`를 찾을 수 없음 | "준비물"에 적힌 대로 QEMU를 설치 |
| `boot-test: kernel ELF not found` | 먼저 `cargo build -p k0-kernel --offline`로 빌드 |
| 느린 컴퓨터에서 부팅 테스트가 시간 초과 | 시간을 늘림. 예: `K0_BOOT_TIMEOUT=300 tools/qemu-boot-test.sh` |
| 16 KiB 이미지가 `cortex-a72`에서 부팅되지 않음 | 정상입니다. 그 CPU는 16 KiB 페이지를 지원하지 않습니다. `K0_QEMU_CPU=max`를 쓰세요 |
| 부팅이 일찍 멈추고 아무것도 출력되지 않음 | 지원하는 콘솔이 없을 수 있습니다. `tools/boot-log.py`로 메모리에서 부트 로그를 읽으세요 |
