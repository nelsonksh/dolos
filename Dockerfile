# 1. Builder
FROM debian:bookworm-slim AS builder

# Install build deps
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      build-essential \
      git \
      pkg-config \
      libssl-dev \
      libgmp-dev \
      libmpfr-dev \
      ca-certificates \
      curl \
      && rm -rf /var/lib/apt/lists/*

# Install Rust toolchain
RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app

# Copy source
COPY . .

# Use a Cargo config to prefer the workspace `pallas` and lockfile
RUN cargo clean
RUN cargo build --release

# 2. Runtime image
FROM debian:bookworm-slim

# Needed for TLS etc.
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      ca-certificates \
      libssl3 \
      libgmp10 \
      libmpfr6 \
      && rm -rf /var/lib/apt/lists/*

WORKDIR /root/

# Copy the built binary
COPY --from=builder /app/target/release/dolos /usr/local/bin/dolos

ENTRYPOINT ["dolos"]
