# K0

AArch64 대상 마이크로커널입니다. seL4 방식의 케이퍼빌리티 자원 모델을 따르고 Rust no_std로 작성됐습니다. 1차 타겟은 QEMU virt(4K 그래뉼)와 Apple Silicon(16K 그래뉼)이고, AMD64를 비롯한 다른 아키텍처는 1차 타겟 완료 후 확장 대상입니다.

## 보안 상태

현재 버전(0.1.0)은 태스크 간 격리를 제공하지 않습니다. `TCB_CONFIGURE` / `TCB_RESUME`으로 만든 태스크는 루트 태스크의 스레드이며 격리된 프로세스가 아닙니다. 신뢰하지 않는 코드를 태스크로 실행하지 마세요. 자세한 내용과 신고 방법은 [SECURITY.md](SECURITY.md)([한국어](SECURITY_KR.md))를 참고하세요.

## 빌드와 실행

```sh
cargo virt    # QEMU virt에서 부팅
cargo apple   # Apple Silicon(m1n1) release 빌드
```

## TODO

- [ ] TCB/AddrSpace retype으로 다중 태스크 스폰 지원
- [ ] `k0-sched`에 라운드 로빈 스케줄러와 타이머 선점 추가
- [ ] Endpoint 오브젝트와 동기 랑데부 IPC(`SEND`/`RECV`/`CALL`/`REPLY_RECV`) 구현
- [ ] 파생 계보(CDT)와 `MINT`(배지 각인, 권한 축소 복사), `revoke` 구현
- [ ] IPC를 통한 케이퍼빌리티 전송(Grant)
- [ ] Notification 오브젝트로 IRQ를 사용자 공간에 전달
- [ ] EL0 폴트를 핸들러 태스크에 IPC로 전달
- [ ] 루트 태스크가 분리 배포되는 시점에 Ed25519 공개키 검증 추가
- [ ] Apple Silicon 실기 부팅 검증
- [ ] AMD64 등 타 아키텍처 확장