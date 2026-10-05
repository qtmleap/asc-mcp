FROM rust:1.99-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY data ./data
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && useradd --create-home --uid 10001 appuser
COPY --from=builder /app/target/release/asc-mcp /usr/local/bin/asc-mcp
USER appuser
ENTRYPOINT ["/usr/local/bin/asc-mcp"]
CMD ["serve"]
