FROM debian:bookworm-slim@sha256:4724b8cc51e33e398f0e2e15e18d5ec2851ff0c2280647e1310bc1642182655d

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
       ca-certificates curl gcc libc6-dev gcc-powerpc-linux-gnu \
       libc6-dev-powerpc-cross qemu-user locales \
    && rm -rf /var/lib/apt/lists/*

# Locale files contain target-endian data; the host C.UTF-8 files are little endian.
RUN mkdir -p /opt/ppc-locales \
    && localedef --big-endian --no-archive -i en_US -f UTF-8 /opt/ppc-locales/en_US.UTF-8

ARG RUST_VERSION
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain "${RUST_VERSION}" \
      --target powerpc-unknown-linux-gnu
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION} \
    LOCPATH=/opt/ppc-locales \
    CARGO_TARGET_DIR=/work/target \
    CARGO_HOME=/work/target-audit/powerpc/cargo-home \
    CARGO_TARGET_POWERPC_UNKNOWN_LINUX_GNU_LINKER=powerpc-linux-gnu-gcc \
    CARGO_TARGET_POWERPC_UNKNOWN_LINUX_GNU_RUNNER="qemu-ppc -L /usr/powerpc-linux-gnu" \
    CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
    CARGO_BUILD_JOBS=1 TOKIO_WORKER_THREADS=2 RUST_TEST_THREADS=2 RUST_MIN_STACK=8388608
WORKDIR /work
