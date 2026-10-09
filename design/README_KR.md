# K0 설계 노트

[English](README.md)

이 노트들은 K0의 각 부분이 어떻게 동작하는지, 왜 그렇게 만들었는지, 어떻게 테스트하는지, 아직 무엇이 없는지를 설명합니다. 버전 0.1.0 기준의 코드를 서술합니다. 노트와 코드가 다르면 실제로 실행되는 것은 코드입니다. 이슈를 열어주시면 노트를 고치겠습니다.

처음에는 아래 순서대로 읽어주세요. 커널이 부팅할 때 일을 하는 순서와 같습니다.

| 노트 | 주제 |
| --- | --- |
| [boot_KR.md](boot_KR.md) | 첫 명령에서 루트 태스크까지: 코어 선택, EL1로의 하강, higher-half로의 점프, 고정된 초기화 순서 |
| [memory_KR.md](memory_KR.md) | 커널 페이지 테이블, 정적 테이블 풀과 롤백, W^X, 가드 페이지, 부팅 시 변환 검사 |
| [user-address-space_KR.md](user-address-space_KR.md) | 루트 태스크의 주소 공간, 사용자 권한, 커널이 테이블을 할당하지 않고도 `MAP`이 주소 공간을 늘리는 방법 |
| [capabilities_KR.md](capabilities_KR.md) | 루트 CNode, untyped 메모리, retype과 실패 시 보장, 0.1.0에 격리가 없는 이유 |
| [dtb_KR.md](dtb_KR.md) | 적대적 입력으로서의 디바이스 트리: 파서가 읽는 것, 한계, 퍼징 방법 |
| [hardening_KR.md](hardening_KR.md) | PAC 키 파생, PAN, 카운터·디버그·PMU에 대한 EL0 접근 차단, 루트 태스크 무결성 검사와 각각이 아직 보호하지 못하는 것 |
| [exceptions_KR.md](exceptions_KR.md) | 벡터 테이블, 사용자 상태 저장, 시스템 콜 디스패치, 폴트 격리 |
| [scheduling_KR.md](scheduling_KR.md) | TCB, 태스크 생성, 라운드 로빈 스케줄러, 타이머 선점 |
| [ipc_KR.md](ipc_KR.md) | 엔드포인트, `SEND`, `RECV`, `CALL`, `REPLY_RECV`, 1회성 응답 자격 |

모든 노트는 "알려진 공백" 절로 끝납니다. 열려 있는 보안 어드바이저리는 [SECURITY_KR.md](../SECURITY_KR.md)에 정리되어 있습니다.
