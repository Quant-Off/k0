#!/bin/sh
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

host="$(rustc -vV | sed -n 's/^host: //p')"

cargo test --offline --target "$host" -p k0-abi -p k0-cap -p k0-boot -p k0-efi -p k0-conin "$@"
# 키 볼트: 알고리즘을 넣은 것과 뺀 것을 따로 돌림(feature가 다르면 다른 코드)
cargo test --offline --target "$host" -p k0-vault --features ed25519 "$@"
cargo test --offline --target "$host" -p k0-vault "$@"
# 운영자 터미널: 볼트 프로토콜을 아는 것과 모르는 것
cargo test --offline --target "$host" -p k0-term --features vault "$@"
cargo test --offline --target "$host" -p k0-term "$@"

case "$(uname -m)" in
    arm64|aarch64)
        cargo test --offline --target "$host" -p k0-mm "$@"
        cargo test --offline --target "$host" -p k0-mm --no-default-features --features granule-16k "$@"
        ;;
    *)
        echo "host-test: skipping k0-mm, its inline assembly needs an aarch64 host"
        ;;
esac
