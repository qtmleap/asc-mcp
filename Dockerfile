# syntax=docker/dockerfile:1.7
FROM rust:1.99-slim-bookworm AS builder
ARG TARGETARCH
WORKDIR /src
# Sources never enter a build layer; dependencies and objects survive rebuilds.
RUN --mount=type=bind,source=.,target=/src,readonly \
    --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=asc-mcp-target-${TARGETARCH},target=/cargo-target,sharing=locked \
    CARGO_TARGET_DIR=/cargo-target cargo build --release --locked \
 && install -D /cargo-target/release/asc-mcp /out/asc-mcp \
 && install -d -m 0700 -o 65532 -g 65532 /out/state

# Matches the builder's glibc; contains CA certificates, no shell/toolchain.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /out/asc-mcp /usr/local/bin/asc-mcp
COPY --from=builder --chown=65532:65532 /out/state /state
ENV HOME=/state
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/asc-mcp"]
CMD ["serve"]
