# Stage 1: build all workspace binaries.
# protoc-bin-vendored in build.rs supplies its own protoc — no system protoc needed.
FROM rust:1.96 AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --workspace

# Stage 2: master image
FROM debian:bookworm-slim AS master
COPY --from=builder /build/target/release/faultforge-master /usr/local/bin/
CMD ["faultforge-master"]

# Stage 3: agent image
FROM debian:bookworm-slim AS agent
COPY --from=builder /build/target/release/faultforge-agent /usr/local/bin/
CMD ["faultforge-agent"]
