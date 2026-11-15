# Nockmark registry — Railway/container build.
# The nockchain crates are git deps pinned by `rev` in registry/Cargo.toml and
# tock/Cargo.toml (that rev is the pin; there is no nockchain checkout here),
# and the build is --locked to registry/Cargo.lock. The roswell verifier kernel
# is compiled IN via include_bytes (KERNEL_JAM_PATH → tock/assets/). Kernel jams
# are prebuilt on the dev machine at the same rev and shipped from
# deploy/assets/ (jammed nouns are architecture-independent).

FROM debian:bookworm-slim AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential clang pkg-config libssl-dev git curl xz-utils ca-certificates \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

# Pinned nightly toolchain, no rustup (same tarball install as tock/setup-bench.sh)
ENV RUST_TRIPLE=x86_64-unknown-linux-gnu TOOLCHAIN_DATE=2026-04-03
RUN mkdir -p /opt/rust /tmp/rust-dl && cd /tmp/rust-dl && \
    for c in rustc cargo rust-std; do \
      curl -sSfLO "https://static.rust-lang.org/dist/${TOOLCHAIN_DATE}/${c}-nightly-${RUST_TRIPLE}.tar.xz" && \
      tar xf "${c}-nightly-${RUST_TRIPLE}.tar.xz" && \
      "./${c}-nightly-${RUST_TRIPLE}/install.sh" --prefix=/opt/rust --disable-ldconfig >/dev/null; \
    done && rm -rf /tmp/rust-dl
ENV PATH="/opt/rust/bin:${PATH}" RUST_MIN_STACK=8388608

COPY . /build/nockmark
RUN mkdir -p /build/nockmark/tock/assets && \
    cp /build/nockmark/deploy/assets/roswell.jam /build/nockmark/tock/assets/roswell.jam

# From inside registry/, so cargo picks up registry/.cargo/config.toml.
WORKDIR /build/nockmark/registry
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/nockmark/registry/target/release/nockmark-registry /opt/nockmark/nockmark-registry
COPY deploy/assets/registry.jam /opt/nockmark/registry.jam
ENV RUST_MIN_STACK=8388608
# Railway injects PORT; /data must be a mounted volume or the leaderboard resets on redeploy.
CMD /opt/nockmark/nockmark-registry --listen "0.0.0.0:${PORT:-8080}" --kernel /opt/nockmark/registry.jam --data-dir /data
