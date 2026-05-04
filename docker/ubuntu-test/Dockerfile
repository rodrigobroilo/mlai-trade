FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
ENV PATH=/root/.cargo/bin:${PATH}
ENV RUST_BACKTRACE=1

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        git \
        jq \
        libclang-dev \
        libomp-dev \
        libssl-dev \
        pkg-config \
        rsync \
        sqlite3 \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable \
    && rustup component add rustfmt

WORKDIR /workspace

CMD ["bash", "-lc", "cargo fmt --check && cargo check --no-default-features && cargo test --no-default-features && cargo build --release --no-default-features"]
