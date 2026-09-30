FROM rust:1-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release --locked

FROM rust:1-bookworm AS tester
WORKDIR /app
COPY . .
CMD ["cargo", "test", "--locked"]

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    bash \
    python3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/bv /usr/local/bin/bv
COPY --from=builder /app/target/release/bvd /usr/local/bin/bvd
COPY --from=builder /app/scripts /app/scripts

WORKDIR /app
EXPOSE 7878

ENTRYPOINT ["bvd"]
CMD ["serve", "--data", "/vault", "--listen", "0.0.0.0:7878"]
