FROM rust:1.98-slim-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src src
COPY web web
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --no-create-home --uid 1000 stacktory
WORKDIR /app

COPY --from=builder /app/target/release/stacktory ./stacktory
RUN mkdir -p /app/data /app/logs && chown stacktory:stacktory /app/data /app/logs

EXPOSE 18761
USER stacktory
CMD ["./stacktory"]
