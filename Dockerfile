FROM rust:1.89.0-slim-bookworm AS build

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libclang-dev \
    libssl-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN curl -L https://foundry.paradigm.xyz | bash && \
    /root/.foundry/bin/foundryup
COPY . .

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    openssl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /app/target/release/enso-temper /enso-temper
COPY --from=build /root/.foundry/bin/anvil /usr/local/bin/anvil
EXPOSE 8080

CMD ["./enso-temper"]
