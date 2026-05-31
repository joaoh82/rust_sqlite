# Multi-stage build for the SQLRite Go edge/IoT collector (SQLR-43).
#
# Build context MUST be the repository root, because the Go module
# links the engine's C library (built from `sqlrite-ffi`) and depends
# on `sdk/go` via a replace directive:
#
#   docker build -f examples/go-collector/Dockerfile -t sqlrite-collector .
#
# cgo + cross-compilation note: this image is built for the host's
# architecture. cgo binaries can't be cross-compiled with the plain Go
# toolchain, so produce per-arch images with `docker buildx --platform`
# (the Rust and Go stages both honor the target platform). See the
# README's "Build & distribution matrix" section.

# ---- Stage 1: build libsqlrite_c.so from the engine source ----
FROM rust:1-bookworm AS rust
WORKDIR /src
COPY . .
RUN cargo build --release -p sqlrite-ffi

# ---- Stage 2: build the Go binaries against that library ----
FROM golang:1.22-bookworm AS go
ENV CGO_ENABLED=1
WORKDIR /src
# Bring the whole tree over (we need sdk/go, sqlrite-ffi/include, and
# the freshly-built target/release/libsqlrite_c.so from the rust stage).
COPY --from=rust /src /src
WORKDIR /src/examples/go-collector
RUN go build -trimpath -o /out/collector ./cmd/collector \
 && go build -trimpath -o /out/loadgen ./cmd/loadgen

# ---- Stage 3: slim runtime ----
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 collector
# Install the shared library where the dynamic linker will find it.
# The Go binary was linked with an rpath pointing at the build tree;
# that path is absent here, so cgo falls through to the ldconfig cache.
COPY --from=rust /src/target/release/libsqlrite_c.so /usr/local/lib/
RUN ldconfig
COPY --from=go /out/collector /usr/local/bin/collector
COPY --from=go /out/loadgen /usr/local/bin/loadgen
USER collector
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8080
ENTRYPOINT ["collector"]
CMD ["-db", "/data/events.sqlrite", "-addr", ":8080"]
