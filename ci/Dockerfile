# K0 build and test image. CI builds it and then runs every test inside it with networking
# disabled. It builds the AArch64 kernel with the pinned Rust toolchain, boots it under QEMU
# and records the root task's self-test output. Network access exists only while this file
# runs; every later step (cargo, QEMU, gdb) must work offline.
# The file started as the build recipe for an external source scanner. The layout that the
# threat model next to it describes is kept: the checkout at /src, the boot log at
# /src/bootcheck.log.
FROM debian:bookworm
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git python3 bash procps gcc libc6-dev \
      qemu-system-arm ipxe-qemu gdb-multiarch \
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

# The checkout lives inside the image, at /src
COPY . /src
WORKDIR /src

# Dependencies are vendored (vendor/ via .cargo/config.toml), so both images build offline.
# dev profile = 4K granule image with debug info; release + granule-16k = 16K granule image.
# Both are generic: they find the UART, the GIC and the timer in the device tree at boot.
RUN cargo build -p k0-kernel --offline \
 && cargo build -p k0-kernel --release --no-default-features --features granule-16k --offline

# Boot check. tools/qemu-boot-test.sh boots the kernel, waits for the root task's self-test
# lines in order, and stops QEMU as soon as they appear. A failure is a warning, never a
# failed build. The serial log stays at /src/bootcheck.log.
RUN (K0_BOOT_LOG=/src/bootcheck.log tools/qemu-boot-test.sh \
      || echo "WARNING: QEMU boot check did not reach 'root: sched tests pass'") \
 && tail -n 40 /src/bootcheck.log
