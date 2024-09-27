FROM rust:1.69.0-slim-buster AS build
WORKDIR /app
COPY . .
RUN cargo build --release


FROM debian:buster-slim

RUN apt-get update && apt-get install -y \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /app/target/release/enso-temper /usr/local/bin/enso-temper
EXPOSE 8080

CMD ["/usr/local/bin/enso-temper"]