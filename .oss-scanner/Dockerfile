# K0 build recipe for the Anthropic OSS Scanner (https://github.com/anthropics/oss-scanner).
# Builds the AArch64 kernel on an x86-64 host with the pinned Rust toolchain, then boots it
# under QEMU and records the root task's self-test output. Network access exists only while
# this file runs; every later step (cargo, QEMU, gdb) must work offline.
FROM debian:bookworm
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git python3 bash procps \
      qemu-system-arm gdb-multiarch \
    && rm -rf /var/lib/apt/lists/*

# Pinned toolchain, identical to rust-toolchain.toml: 1.89.0 + rust-src + llvm-tools + bare-metal target.
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal \
      --default-toolchain 1.89.0 \
      --component rust-src --component llvm-tools \
      --target aarch64-unknown-none-softfloat \
    && rustup --version && cargo --version \
    && host="$(rustc -vV | sed -n 's/^host: //p')" \
    && for t in llvm-objcopy llvm-objdump llvm-nm llvm-readelf llvm-size; do \
         ln -s "$(rustc --print sysroot)/lib/rustlib/$host/bin/$t" "/usr/local/bin/$t"; done \
    && llvm-objdump --version | head -n 1

# scanner contract: the checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# Dependencies are vendored (vendor/ via .cargo/config.toml), so both images build offline.
# dev profile = QEMU virt image with debug info; release + plat-apple = Apple Silicon image (compile only).
RUN cargo build -p k0-kernel --offline \
 && cargo build -p k0-kernel --release --no-default-features --features plat-apple --offline

# Boot check. The root task runs its self-tests on the serial console and then idles forever,
# so QEMU is stopped by timeout (exit 124 is the normal outcome). A missing pass line is a
# warning, never a failed build. The log stays at /src/bootcheck.log for the scanner.
RUN (timeout 120 tools/qemu-virt-runner.sh target/aarch64-unknown-none-softfloat/debug/k0-kernel > /src/bootcheck.log 2>&1 || true) \
 && (grep -q "root: sched tests pass" /src/bootcheck.log || echo "WARNING: QEMU boot check did not reach 'root: sched tests pass'") \
 && tail -n 40 /src/bootcheck.log
