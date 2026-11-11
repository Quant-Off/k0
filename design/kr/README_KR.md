# K0 설계 노트

<div align="center">

[English](../en/README.md) | 한국어

</div>

이 노트들은 K0의 각 부분이 어떻게 동작하는지, 왜 그렇게 만들었는지, 어떻게 테스트하는지, 아직 무엇이 없는지를 설명합니다. 태스크 격리를 포함한 현재 `main` 브랜치(미릴리즈, 크레이트 버전 0.1.0)의 코드를 서술합니다. 릴리즈된 `v0.1.0` 태그에는 격리가 없습니다. 노트와 코드가 다르면 실제로 실행되는 것은 코드입니다. 이슈를 열어주시면 노트를 고치겠습니다.

처음에는 아래 순서대로 읽어주세요. 커널이 부팅할 때 일을 하는 순서와 같습니다.

| 노트 | 주제 |
| --- | --- |
| [boot_KR.md](boot_KR.md) | 첫 명령에서 루트 태스크까지: 코어 선택, EL1로의 하강, higher-half로의 점프, 고정된 초기화 순서 |
| [memory_KR.md](memory_KR.md) | 커널 페이지 테이블, 정적 테이블 풀과 롤백, W^X, 가드 페이지, 부팅 시 변환 검사 |
| [user-address-space_KR.md](user-address-space_KR.md) | `AddrSpace` 오브젝트와 레지스트리, 사용자 권한, 커널이 테이블을 할당하지 않는 `MAP`과 `UNMAP`, 케이퍼빌리티가 제거될 때 매핑을 정리하는 방법 |
| [capabilities_KR.md](capabilities_KR.md) | 태스크별 CNode, 권한과 배지, 파생 목록, untyped 메모리, retype과 실패 시 보장, revoke와 delete, 태스크 격리가 다루는 것과 다루지 않는 것 |
| [dtb_KR.md](dtb_KR.md) | 적대적 입력으로서의 디바이스 트리: 파서가 읽는 것, 한계, 퍼징 방법 |
| [hardening_KR.md](hardening_KR.md) | PAC 키 파생, PAN, 카운터·디버그·PMU에 대한 EL0 접근 차단, 루트 태스크 무결성 검사와 각각이 아직 보호하지 못하는 것 |
| [exceptions_KR.md](exceptions_KR.md) | 벡터 테이블, 사용자 상태 저장, 시스템 콜 디스패치, 폴트 격리 |
| [scheduling_KR.md](scheduling_KR.md) | TCB와 거기에 묶인 CNode, 주소 공간, 태스크 생성과 정리, 라운드 로빈 스케줄러, 타이머 선점 |
| [ipc_KR.md](ipc_KR.md) | 엔드포인트와 배지, `SEND`, `RECV`, `CALL`, `REPLY_RECV`, 1회성 응답 자격, 엔드포인트가 삭제될 때 대기 중인 태스크의 처리 |

모든 노트는 "알려진 공백" 절로 끝납니다. 열려 있는 보안 어드바이저리는 [SECURITY_KR.md](../../SECURITY_KR.md)에 정리되어 있습니다.
