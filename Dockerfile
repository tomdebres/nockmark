# Nockmark registry — Railway/container build.
# Builder clones the pinned nockchain fork (path deps expect ../../../nockchain
# relative to registry/) and compiles the registry binary; the roswell verifier
# kernel is compiled IN via include_bytes (KERNEL_JAM_PATH → tock/assets/).
# Kernel jams are prebuilt on the dev machine and shipped from deploy/assets/
# (jammed nouns are architecture-independent).

FROM debian:bookworm-slim AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential clang pkg-config libssl-dev git curl xz-utils ca-certificates \
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

ARG NOCKCHAIN_REPO=https://github.com/tomdebres/nockchain.git
ARG NOCKCHAIN_COMMIT=31b8a015
WORKDIR /build
RUN git clone --filter=blob:none "$NOCKCHAIN_REPO" nockchain && \
    git -C nockchain checkout "$NOCKCHAIN_COMMIT"

# Repo layout the path deps expect: /build/nockchain + /build/nockmark/m0-prover-spike
COPY . /build/nockmark/m0-prover-spike
RUN mkdir -p /build/nockmark/m0-prover-spike/tock/assets && \
    cp /build/nockmark/m0-prover-spike/deploy/assets/roswell.jam \
       /build/nockmark/m0-prover-spike/tock/assets/roswell.jam

WORKDIR /build/nockmark/m0-prover-spike/registry
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/nockmark/m0-prover-spike/registry/target/release/nockmark-registry /opt/nockmark/nockmark-registry
COPY deploy/assets/registry.jam /opt/nockmark/registry.jam
ENV RUST_MIN_STACK=8388608
# Railway injects PORT; /data must be a mounted volume or the leaderboard resets on redeploy.
CMD /opt/nockmark/nockmark-registry --listen "0.0.0.0:${PORT:-8080}" --kernel /opt/nockmark/registry.jam --data-dir /data
