#!/usr/bin/env python3
"""Rust 툴체인을 해시로 고정해 설치합니다.

CI 이미지를 만들 때 한 번 실행합니다. 채널 매니페스트 하나의 SHA-256만 이
파일에 고정하고, 설치하는 모든 꾸러미는 그 매니페스트에 적힌 SHA-256과
대조합니다. 그래서 내려받는 서버나 그 사이의 네트워크가 다른 파일을 주면
설치가 실패합니다. rustup은 쓰지 않습니다(설치 스크립트와 채널 매니페스트를
받는 서버를 그대로 믿게 되므로).

버전을 올릴 때는 CHANNEL과 MANIFEST_SHA256을 함께 바꾸고
rust-toolchain.toml과 맞춥니다. rustup을 쓰지 않으면 그 파일을 읽는 것이
없으므로, 이 스크립트가 그 파일과 자기 설정이 같은지 먼저 확인하고 다르면
설치하지 않습니다.

사용법: install-toolchain.py <설치할 디렉토리> <rust-toolchain.toml 경로>
"""

import hashlib
import platform
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.request
from pathlib import Path

CHANNEL = "1.89.0"
MANIFEST_URL = f"https://static.rust-lang.org/dist/channel-rust-{CHANNEL}.toml"
MANIFEST_SHA256 = "fbd1662e100e7b305908ece23b441cb7534eadfa6336c5f173ff08d1cec174a1"

# 커널의 베어메탈 타겟과 로더의 UEFI 타겟 (rust-toolchain.toml의 targets와 같음)
CROSS_TARGETS = ["aarch64-unknown-none-softfloat", "aarch64-unknown-uefi"]
# rust-toolchain.toml의 components 이름과 매니페스트의 꾸러미 이름
COMPONENTS = {"rust-src": "rust-src", "llvm-tools": "llvm-tools-preview"}

HOSTS = {
    "aarch64": "aarch64-unknown-linux-gnu",
    "arm64": "aarch64-unknown-linux-gnu",
    "x86_64": "x86_64-unknown-linux-gnu",
}


def fetch(url):
    if not url.startswith("https://static.rust-lang.org/"):
        sys.exit(f"install-toolchain: refusing to fetch {url}")
    with urllib.request.urlopen(url, timeout=120) as r:
        return r.read()


def checked(url, want):
    data = fetch(url)
    got = hashlib.sha256(data).hexdigest()
    if got != want:
        sys.exit(f"install-toolchain: {url}\n  sha256 {got}\n  wanted {want}")
    return data


def check_pin_file(path):
    """개발자가 쓰는 rust-toolchain.toml과 이 스크립트의 설정이 같은지 확인합니다."""
    pin = tomllib.loads(Path(path).read_text())["toolchain"]
    same = (
        pin.get("channel") == CHANNEL
        and sorted(pin.get("targets", [])) == sorted(CROSS_TARGETS)
        and sorted(pin.get("components", [])) == sorted(COMPONENTS)
    )
    if not same:
        sys.exit(f"install-toolchain: {path} and this script name different toolchains")


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    prefix = Path(sys.argv[1])
    check_pin_file(sys.argv[2])
    host = HOSTS.get(platform.machine())
    if host is None:
        sys.exit(f"install-toolchain: no toolchain for host {platform.machine()}")

    manifest = tomllib.loads(checked(MANIFEST_URL, MANIFEST_SHA256).decode())
    wanted = [
        ("rustc", host),
        ("cargo", host),
        ("rust-std", host),
        (COMPONENTS["llvm-tools"], host),
        (COMPONENTS["rust-src"], "*"),
    ] + [("rust-std", t) for t in CROSS_TARGETS]

    with tempfile.TemporaryDirectory() as tmp:
        for name, target in wanted:
            entry = manifest["pkg"][name]["target"].get(target)
            if not entry or not entry.get("available"):
                sys.exit(f"install-toolchain: {name} is not available for {target}")
            archive = Path(tmp) / "pkg.tar.xz"
            archive.write_bytes(checked(entry["xz_url"], entry["xz_hash"]))
            unpacked = Path(tmp) / f"{name}-{target.replace('*', 'any')}"
            # 꾸러미는 방금 해시를 대조한 것이므로 그대로 풉니다
            with tarfile.open(archive) as tar:
                tar.extractall(unpacked)
            (top,) = list(unpacked.iterdir())
            subprocess.run(
                ["sh", str(top / "install.sh"), f"--prefix={prefix}", "--disable-ldconfig"],
                check=True,
                stdout=subprocess.DEVNULL,
            )
            print(f"install-toolchain: {name} ({target}) ok", flush=True)


if __name__ == "__main__":
    main()
