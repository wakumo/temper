FROM rust:1.89.0-slim-bookworm AS build

WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    libclang-dev \
    libssl-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release && \
    cargo build --release --manifest-path \
    /usr/local/cargo/git/checkouts/foundry-87057ca846c16966/*/crates/anvil/Cargo.toml \
    --no-default-features --features cli --bin anvil

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    openssl \
    && rm -rf /var/lib/apt/lists/*

# Copy the binary from the build stage to the current directory in the new stage
COPY --from=build /app/target/release/enso-temper /enso-temper
COPY --from=build /usr/local/cargo/git/checkouts/foundry-87057ca846c16966/*/target/release/anvil /usr/local/bin/anvil
EXPOSE 8080
CMD ["./enso-temper"]
