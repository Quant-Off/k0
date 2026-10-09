#!/bin/sh
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

host="$(rustc -vV | sed -n 's/^host: //p')"

cargo test --offline --target "$host" -p k0-abi -p k0-cap -p k0-boot "$@"

case "$(uname -m)" in
    arm64|aarch64)
        cargo test --offline --target "$host" -p k0-mm "$@"
        cargo test --offline --target "$host" -p k0-mm --no-default-features --features plat-apple "$@"
        ;;
    *)
        echo "host-test: skipping k0-mm, its inline assembly needs an aarch64 host"
        ;;
esac
