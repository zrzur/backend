# Zrzur image — uses locally built base (never ghcr.io/stoatchat/base).
# Requires: docker build -t zrzur/base:local -f Dockerfile .  (from backend/)
FROM zrzur/base:local AS builder

FROM gcr.io/distroless/cc-debian13:nonroot
COPY --from=builder /home/rust/src/target/release/revolt-gifbox ./
COPY LICENSE /licenses/LICENSE

EXPOSE 14706
USER nonroot
CMD ["./revolt-gifbox"]
