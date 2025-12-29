# 1. Builder stage
FROM debian:bookworm AS builder

# Set noninteractive for apt
ENV DEBIAN_FRONTEND=noninteractive

# Install build essentials and dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    git \
    pkg-config \
    libgmp-dev \
    libmpfr-dev \
    m4 \
    cmake \
    bash \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Install Rust (nightly)
RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --default-toolchain nightly
ENV PATH="/root/.cargo/bin:${PATH}"

# Set workdir
WORKDIR /app

# Copy Cargo files to cache dependencies first
COPY Cargo.toml Cargo.lock ./

# Fetch dependencies (cached if Cargo.toml didn't change)
RUN cargo fetch

# Copy full source code
COPY . .

# Build release
RUN cargo build --release

# 2. Runtime stage
FROM debian:bookworm-slim AS runtime

# Install minimal runtime dependencies
RUN apt-get update && apt-get install -y \
    libgmp10 \
    libmpfr6 \
    ca-certificates \
    bash \
    && rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /app/target/release/dolos /usr/local/bin/dolos

# Set entrypoint
ENTRYPOINT ["/usr/local/bin/dolos"]
