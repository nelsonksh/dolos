# ----------------------------
# 1. Builder stage
# ----------------------------
FROM rust:1.81-slim-bullseye AS builder

# Install system dependencies for building GMP/MPFR and Rust crates
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    libgmp-dev \
    libmpfr-dev \
    m4 \
    ca-certificates \
    curl \
    git \
    bash \
    && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy Cargo manifests first to leverage Docker caching
COPY Cargo.toml Cargo.lock ./

# Fetch dependencies (so we can rebuild only app source later)
RUN cargo fetch

# Copy the full source code
COPY . .

# Set environment variables for GMP/MPFR (optional but sometimes needed)
ENV GMP_LIB_DIR=/usr/lib
ENV GMP_INCLUDE_DIR=/usr/include
ENV MPFR_LIB_DIR=/usr/lib
ENV MPFR_INCLUDE_DIR=/usr/include

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
