FROM rust:1.85-bookworm AS builder

WORKDIR /build
COPY . .
RUN cargo build --locked --release --features native-backend

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder --chown=nonroot:nonroot /build/target/release/dwg2geo /usr/local/bin/dwg2geo

USER nonroot:nonroot
ENTRYPOINT ["dwg2geo"]
