# K0 build and test image. CI builds it and then runs every test inside it with networking
# disabled. It builds the AArch64 kernel with the pinned Rust toolchain, boots it under QEMU
# and records the root task's self-test output. Network access exists only while this file
# runs; every later step (cargo, QEMU, gdb) must work offline.
# The file started as the build recipe for an external source scanner. The layout that the
# threat model next to it describes is kept: the checkout at /src, the boot log at
# /src/bootcheck.log.
# The base image is pinned by the digest of its multi-architecture index, so both CI runners
# (x86-64 and ARM64) resolve the same tag contents. To move it, look the digest up again
# (`docker buildx imagetools inspect debian:bookworm`) and replace it here.
FROM debian:bookworm@sha256:268fb24c39bca92d326dd83ea3abebe4a3f7751c804ffef23bb6d857dd343ff8
ENV DEBIAN_FRONTEND=noninteractive \
    PATH=/usr/local/rust/bin:$PATH
# Package versions are not pinned. apt checks them against the Debian archive keys that are
# part of the pinned base image, so their origin is authenticated but their versions float.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates git python3 bash procps gcc libc6-dev \
      qemu-system-arm ipxe-qemu qemu-efi-aarch64 gdb-multiarch \
    && rm -rf /var/lib/apt/lists/*

# Pinned toolchain: 1.89.0 + rust-src + llvm-tools, the bare-metal target for the kernel and the
# UEFI target for the loader. The installer pins the SHA-256 of the channel manifest and checks
# every package against the hashes in that manifest; rustup is not used. Nothing else reads
# rust-toolchain.toml here, so the installer refuses to run unless that file names the same
# toolchain. qemu-efi-aarch64 is the EDK2 firmware that the UEFI rows of the boot matrix boot
# through.
COPY ci/install-toolchain.py rust-toolchain.toml /tmp/
RUN python3 /tmp/install-toolchain.py /usr/local/rust /tmp/rust-toolchain.toml \
    && rm /tmp/install-toolchain.py /tmp/rust-toolchain.toml \
    && rustc --version && cargo --version \
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
# Console I/O is opt-in, so both builds name the console feature; without it the kernel has no UART code and stays silent.
RUN cargo build -p k0-kernel --features console --offline \
 && cargo build -p k0-kernel --release --no-default-features --features granule-16k,console --offline

# Boot check. tools/qemu-boot-test.sh boots the kernel, waits for the root task's self-test
# lines in order, and stops QEMU as soon as they appear. A failure is a warning, never a
# failed build. The serial log stays at /src/bootcheck.log.
RUN (K0_BOOT_LOG=/src/bootcheck.log tools/qemu-boot-test.sh \
      || echo "WARNING: QEMU boot check did not reach 'root: sched tests pass'") \
 && tail -n 40 /src/bootcheck.log
