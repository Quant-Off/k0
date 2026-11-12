# 서명 오라클 (키 볼트)

<div align="center">

[English](../en/vault.md) | 한국어

</div>

이 노트는 K0의 격리 코어 위에 올라가는 첫 번째 서비스를 설명합니다. **볼트** 태스크가 Ed25519 키 하나를 보관합니다. 클라이언트 태스크는 IPC로 서명과 공개 키를 받고, 키는 받지 못합니다. 코드는 `userspace/k0-vault`(크레이트), `userspace/root-task/src/oracle.rs`(연결과 자가 테스트), `crates/k0-boot`(부팅 시드)에 있습니다.

> [!NOTE]
> 볼트는 선택 사항이며 기본값은 꺼짐입니다. 기본 이미지에는 볼트도 암호 코드도 없습니다.

## 목적과 범위

볼트는 격리 코어가 비밀을 그것을 쓰는 태스크로부터 떼어 놓을 수 있음을 보여 줍니다. 서비스는 작고 고정되어 있습니다. 상태를 알려 주고, 공개 키를 돌려주고, 1024바이트까지의 메시지에 서명합니다.

알고리즘은 Ed25519입니다(SHA-512를 쓰는 PureEdDSA, FIPS 186-5와 RFC 8032). 구현은 vendor에 넣은 크레이트 `ed25519-dalek` 2.2.0이며 어떤 종류의 검증도 받지 않았습니다. 키는 승인된 난수 비트 생성기에서 나오지 않습니다. 이를 위해 `vendor/`에 크레이트 14개를 추가했고, 그중 절반은 빌드 시점 또는 다른 타겟용 코드라서 K0 이미지에 컴파일되어 들어가지 않습니다.

## 오브젝트와 누가 무엇을 매핑할 수 있는가

![루트 태스크가 볼트, 클라이언트, 엔드포인트 하나, 프레임 두 개를 만듭니다. 클라이언트는 송신 전용 배지 엔드포인트 케이퍼빌리티를 가집니다. 요청 프레임은 클라이언트가 읽기·쓰기, 볼트가 읽기 전용입니다. 응답 프레임은 볼트가 읽기·쓰기, 클라이언트가 읽기 전용입니다.](../../assets/vault-flow_KR.svg)

루트 태스크는 다음 오브젝트를 만듭니다.

- **엔드포인트 하나**: 볼트가 여기서 받습니다. 클라이언트의 CNode에는 송신 전용 배지 복사본만 있습니다(자가 테스트에서는 보고용 엔드포인트도 있습니다).
- **볼트 태스크**: 자체 주소 공간과 자체 스택을 가집니다. 키는 여기에 있습니다.
- **호출자마다 요청 프레임과 응답 프레임**: 요청 프레임은 호출자가 읽기·쓰기, 볼트가 읽기 전용입니다. 응답 프레임은 볼트가 읽기·쓰기, 호출자가 읽기 전용입니다. 프레임 케이퍼빌리티의 권한을 줄인 복사본으로 강제합니다.

> [!WARNING]
> 루트 태스크가 이 모든 것을 만들므로 격리의 바깥에 있으며 볼트를 읽을 수 있습니다. 성립하는 문장은 "루트 태스크 외의 어떤 태스크도 볼트의 메모리를 매핑할 수 없다"입니다.

## 프로토콜

클라이언트는 `CALL`을 사용합니다([ipc_KR.md](ipc_KR.md) 참고). 요청 레지스터는 다음과 같습니다.

| 레지스터 | 내용 |
| --- | --- |
| MR0 | 연산과 알고리즘(`proto::word`) |
| MR1 | 요청 프레임에 있는 본문의 길이 |
| MR2 | 호출자가 고른 nonce |

응답은 네 레지스터를 모두 설정합니다.

| 레지스터 | 내용 |
| --- | --- |
| MR0 | 상태 |
| MR1 | 길이(`STATUS`에서는 볼트 상태) |
| MR2 | 지표 비트 |
| MR3 | nonce |

연산은 `STATUS`, `PUBLIC_KEY`, `SIGN`(요청 프레임에 1024바이트까지의 메시지)이며, admin 배지에만 `INIT`(시드와 시드 정보 40바이트, 한 번)과 `ZEROIZE`가 있습니다.

**단일 가져오기**: `server::fetch`는 길이를 먼저 고정한 뒤 본문을 volatile 읽기로 비공개 버퍼에 한 번만 읽습니다. 사본만 사용하므로 호출 중에 프레임을 바꾸는 호출자는 얻는 것이 없습니다. 응답 프레임은 nonce와 길이로 시작하고, `server::publish`는 nonce를 마지막에 기록합니다. 클라이언트는 자신의 nonce가 담긴 `OK` 응답을 받은 뒤에만 프레임을 믿어야 합니다.

상태 기계는 `userspace/k0-vault/src/server.rs`의 `Vault::handle`입니다.

## 고정된 정책과 배지

정책은 빌드 시점에 고정되며 엔드포인트 배지를 키로 사용합니다. 배지는 커널이 전달하며 클라이언트가 위조할 수 없습니다([ipc_KR.md](ipc_KR.md) 참고). 역할은 다음과 같습니다.

| 역할 | 연산 |
| --- | --- |
| admin (루트 태스크) | `STATUS`, `INIT`, `ZEROIZE`, `SIGN`은 불가 |
| signer | `STATUS`, `PUBLIC_KEY`, `SIGN` |
| reader | `STATUS`, `PUBLIC_KEY` |

배지가 없거나 알 수 없는 배지의 호출은 거부됩니다. 어떤 연산도 정책을 바꾸지 않습니다. 모든 signer가 키 하나를 공유하므로 서명에는 클라이언트 신원이 담기지 않습니다.

## 프로바이더 경계

`userspace/k0-vault/src/provider.rs`만 암호 크레이트에 닿습니다.

- **자가 테스트**: 시작할 때 RFC 8032 벡터 두 개로 알려진 답 테스트를 합니다(공개 키, 서명, 검증, 바뀐 메시지와 바뀐 서명의 거부).
- **키 생성과 엔트로피 규칙**: 부팅 시드에 `rng-seed` 32바이트 이상 또는 RNDR 256비트가 없으면 볼트는 키 생성을 거부합니다(상태 `NO_ENTROPY`). QEMU virt는 32바이트 `rng-seed`를 제공합니다.
- **쌍 일관성 테스트**: 키 생성 뒤에 자신의 서명이 검증되는지 확인합니다. 자가 테스트 실패 또는 쌍 일관성 테스트 실패는 볼트를 `STATUS` 외의 모든 것을 거부하는 실패 상태로 만듭니다.
- **엄격한 검증**: 검증은 `verify_strict`를 사용합니다.
- **키 내보내기 없음**: 키를 돌려주는 연산이 없습니다.
- **제로화**: `ZEROIZE`는 키를 버립니다(크레이트가 지웁니다). 이후 볼트는 `SIGN`과 `PUBLIC_KEY`를 거부합니다.
- **지표 비트**: 모든 응답에 비트 네 개가 있습니다. 알고리즘이 FIPS 186-5에 있음, 자가 테스트 통과, `rng-seed` 32바이트 이상이 키 시드에 들어감, RNDR이 키 시드에 들어감입니다.

## 부팅 시드 경로

커널은 루트 태스크용 32바이트를 자체 도메인 문자열로 SHA-256에 넣어 만듭니다. 입력은 DTB 엔트로피, 카운터, 그리고 CPU에 RNDR이 있으면 새 RNDR 워드 네 개입니다(`k0_boot::derive_root_seed`). 도메인 분리로 PAC 키와는 서로 독립입니다. 시드는 입력만큼만 좋습니다.

시드는 루트 태스크의 진입 레지스터 `x0`부터 `x3`로 전달됩니다. `x4`는 사용한 `rng-seed` 바이트 수(비트 15부터 0)이고, 비트 16은 RNDR이 기여했음을 뜻합니다. bootinfo 페이지에는 없으며 `bootinfo::VERSION`은 3입니다. 루트 태스크는 `root: boot seed from rng-seed bytes 0x... and rndr` 또는 `... without rndr`만 출력하고 시드는 출력하지 않습니다. 이후 `INIT`으로 시드를 볼트에 줍니다.

**시드 지우기**: FDT 파서는 `rng-seed`와 `kaslr-seed` 값의 위치를 기록하고, 두 번째 `/chosen` 노드는 거부합니다(`DuplicateChosen`, 파서를 공유하는 UEFI 로더의 사전 검사에서도 같습니다). 커널은 루트 태스크에 진입하기 직전에 그 바이트를 0으로 만듭니다. `k0_mm::wipe_ro_alias`가 페이지의 읽기 전용 별칭 항목을 제자리에서 읽기·쓰기로 바꾸고(break-before-make), 0을 쓰고, 캐시 라인을 clean 및 invalidate한 뒤 되돌립니다. 이어서 커널은 그 바이트를 다시 읽어 확인하고, `BootInfo`에 있는 자신의 사본을 지우고, `k0: dtb seeds wiped <n>B`(QEMU virt에서는 40B)를 출력하고, 살아 있는 프레임 아래의 부트 스택을 0으로 만듭니다. 이것이 커널이 DTB 매핑으로 하는 유일한 쓰기이며, DTB를 파싱하고 검증한 뒤에 일어납니다.

> [!NOTE]
> 펌웨어나 UEFI 로더가 보관하는 사본과 부트 스택의 살아 있는 프레임 안의 값은 지워지지 않습니다. 스택 사본 지우기(커널 부트 스택, 서명 뒤의 볼트 스택)는 최선 노력입니다.

## feature와 고정된 이미지

`k0-kernel` 크레이트에는 feature가 둘 있으며 feature는 더하기만 합니다.

- `vault`: 서비스, 프로토콜, 정책, 자가 테스트이며 서명 알고리즘은 없습니다. 모든 서명 요청에 `UNSUPPORTED`로 답합니다.
- `vault-ed25519`: Ed25519를 추가합니다.

다음과 같이 빌드할 수 있습니다.

```sh
$ cargo build -p k0-kernel --features console,vault-ed25519
```

`crates/k0-boot/build.rs`가 feature를 루트 태스크의 중첩 빌드로 전달합니다. 볼트의 코드는 루트 태스크 이미지의 일부이며 이전처럼 부팅 시 SHA-256으로 검사됩니다. 부팅 뒤에는 아무것도 적재하지 않습니다. Ed25519를 넣으면 이미지는 4 KiB 프레임 24개입니다(한도 78개).

## 테스트 방법

**호스트 테스트**(`tools/host-test.sh`가 알고리즘 feature 유무로 `k0-vault`를 실행합니다). 테스트는 `userspace/k0-vault/src/tests.rs`에 있으며 Ed25519가 있으면 13개, 없으면 7개입니다. 이름은 다음과 같습니다.

- 항상: `badges_map_to_fixed_roles`, `policy_is_enforced_before_anything_else`, `malformed_requests_are_refused_whole`, `a_seed_without_real_entropy_makes_no_key`, `init_happens_once_and_zeroize_is_final`, `fetch_clamps_the_length_and_publish_writes_header_and_body`, 그리고 `without_an_algorithm_every_signing_request_is_unsupported`(마지막 것은 알고리즘이 없을 때만).
- Ed25519가 있을 때: `the_self_test_passes_and_is_reported`, `signatures_verify_and_the_key_follows_the_seed`, `a_public_key_request_carries_no_body`, `unknown_algorithms_are_unsupported`, `nothing_is_served_after_zeroize`, `a_small_order_public_key_is_refused_even_with_a_matching_signature`, `a_failed_vault_refuses_everything_but_status`.

다른 크레이트와 합치면 호스트 테스트는 ARM 호스트에서 276개, 그 외 호스트에서 190개입니다(`k0-mm` 제외).

**루트 태스크 자가 테스트**(`userspace/root-task/src/oracle.rs`의 `test_oracle`, `vault`로 빌드한 이미지에만 있음). 다음을 검사합니다.

- 자가 테스트가 보고되고, 배지 없는 호출이 거부됩니다.
- 부팅 시드로 키가 만들어지고, 두 번째 `INIT`은 거부됩니다.
- admin은 서명할 수 없습니다.
- signer 클라이언트는 검증되는 서명을 받고, 바뀐 메시지나 바뀐 서명에는 검증이 실패합니다.
- reader는 `SIGN`을 거부당합니다.
- 읽기 전용 응답 프레임에 쓰는 클라이언트와, 볼트가 다른 호출자의 요청을 보는 위치를 읽는 클라이언트는 폴트로 종료됩니다(`k0: task fault` 줄이 6개가 아니라 8개).
- `ZEROIZE` 뒤에는 서명이 거부됩니다.
- 삭제된 볼트에 대한 호출은 `TIMEOUT`으로 끝납니다.
- 볼트에서 회수한 메모리는 retype 뒤에 0으로 읽힙니다.

표지는 `root: oracle tests pass`입니다. 클라이언트 태스크는 루트 CNode 슬롯이 부족해 하나의 클라이언트 주소 공간에서 차례로 실행됩니다. 이 테스트는 볼트와 클라이언트 사이의 격리를 보여 주며 클라이언트끼리의 격리는 보여 주지 않습니다.

**부팅 매트릭스 행**: `vault`, `vault-max`(RNDR), `vault-el2`, `vault-16k`, `vault-no-alg`, `hvf-vault`, 그리고 정적 행 `vault-symbols`(기본 이미지에는 볼트나 암호 코드가 없고, 볼트 이미지에는 그 코드에서 온 쓰기 가능한 정적 변수가 없음)입니다. 매트릭스는 HVF가 있는 Mac에서 61행, CI에서 51행입니다. 이제 모든 이미지는 `k0: dtb seeds wiped <n>B`와 `root: boot seed from rng-seed bytes ...`도 출력합니다.

## 알려진 공백

> [!NOTE]
> 구현은 검증을 받지 않았고, 이 커널에서 크레이트의 부채널 분석은 없으며, 오라클은 QEMU(TCG와 HVF)에서만 실행했습니다.

- **키 하나**: 영속성, 키 가져오기·내보내기, DRBG, 운영자 입력, 양자 내성 알고리즘이 아직 없습니다.
- **가용성은 보호되지 않습니다**: 클라이언트는 엔드포인트 하나를 공유하며 모든 호출에 타임아웃을 써야 합니다([ipc_KR.md](ipc_KR.md) 참고).
- **루트 태스크는 신뢰 대상입니다**: 볼트를 만들고 읽을 수 있습니다.
- **클라이언트 신원 없음**: 모든 signer가 키 하나를 공유합니다.
- **엔트로피**: 시드는 입력만큼만 좋습니다. 키는 승인된 난수 비트 생성기에서 나오지 않습니다.
- **최선 노력의 지우기**: 부팅 시드 경로 절의 참고를 보십시오.
