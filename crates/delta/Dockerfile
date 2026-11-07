# Zrzur local image — uses locally built base (never ghcr.io/stoatchat/base).
# Requires: docker build -t zrzur/base:local -f Dockerfile .  (from backend/)
FROM zrzur/base:local AS builder
FROM debian:13 AS debian

FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=builder /home/rust/src/target/release/revolt-delta ./
COPY --from=debian /usr/bin/uname /usr/bin/uname
COPY LICENSE /licenses/LICENSE

EXPOSE 14702
ENV ROCKET_ADDRESS=0.0.0.0
USER nonroot
CMD ["./revolt-delta"]
