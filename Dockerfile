# Use Rust nightly on Debian Bookworm
FROM rust:nightly-bookworm AS builder

# Install system dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    libgmp-dev \
    libmpfr-dev \
    m4 \
    pkg-config \
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy Cargo manifests first (cache dependencies)
COPY Cargo.toml Cargo.lock ./

# Fetch dependencies
RUN cargo fetch

# Copy source code
COPY . .

# Build release
RUN cargo build --release

# ----------------------------
# 2. Runtime stage
# ----------------------------
FROM debian:bullseye-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    libgmp10 \
    libmpfr6 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy the compiled binary from the builder
WORKDIR /app
COPY --from=builder /app/target/release/dolos .

# Default command
CMD ["./dolos"]
