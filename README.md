<!-- zrzur-publication -->
# Zrzur Backend

The server side of [Zrzur](https://zrzur.com): the REST API, real-time events, file storage, the media proxy and the background daemons. Written in Rust.

Part of Zrzur 26. Versions and the other components: [ZRZUR_RELEASE.md](./ZRZUR_RELEASE.md).

## Quick start

```bash
git clone https://github.com/zrzur/backend.git
cd backend
docker compose up -d                              # Redis, RabbitMQ, MinIO, MongoDB, maildev, LiveKit on 127.0.0.1
cargo check --workspace --tests
TEST_DB=REFERENCE cargo nextest run --workspace   # in-memory database
```

Needs Git, Docker, Rust 1.92 (`rust-toolchain.toml` selects it) and `cargo-nextest`, on Linux or WSL 2.

## Run the services

The repository's tasks run with [mise](https://mise.jdx.dev/), which also installs the pinned tools.

```bash
mise install
mise build
cp livekit.example.yml livekit.yml
mise start
```

Stop with Ctrl+C, then `mise docker:stop`.

- Sign-up emails of the local stack: http://localhost:14080
- A client to talk to it: [Zrzur Web](https://github.com/zrzur/web)
- Nix users: `nix develop`
- Faster linking with [mold](https://github.com/rui314/mold) (optional), in `.env`: `BUILDER = "mold --run cargo"`

## What is inside

Clients talk to **Delta** over HTTP and to **Bonfire** over WebSocket. **Autumn** serves files. MongoDB, Redis or Valkey, and RabbitMQ are behind them.

| Crate | Role | Local port |
| --- | --- | :---: |
| [`delta`](crates/delta) | REST API | 14702 |
| [`bonfire`](crates/bonfire) | WebSocket events | 14703 |
| [`services/autumn`](crates/services/autumn) | File server | 14704 |
| [`services/january`](crates/services/january) | Link metadata and image proxy | 14705 |
| [`services/gifbox`](crates/services/gifbox) | Tenor proxy | 14706 |
| [`daemons/crond`](crates/daemons/crond) | Timed cleanup | |
| [`daemons/pushd`](crates/daemons/pushd) | Push notifications | |
| [`core/config`](crates/core/config) | Configuration (`Revolt.toml`) | |
| [`core/database`](crates/core/database) | Database | |
| [`core/models`](crates/core/models) | API models | |
| [`core/permissions`](crates/core/permissions) | Permissions | |
| [`core/presence`](crates/core/presence) | Presence | |
| [`core/files`](crates/core/files) | S3 and encryption helpers | |
| [`core/result`](crates/core/result) | Errors | |
| [`core/coalesced`](crates/core/coalesced) | Coalescion | |

The file name `Revolt.toml` and crate names such as `revolt-*` are kept so the existing protocol and tools keep working.

Ports of the data services from `compose.yml`:

| Service | Port |
| --- | :---: |
| MongoDB | 27017 |
| Redis | 6379 |
| RabbitMQ | 5672 / 15672 |
| MinIO | 14009 |
| Maildev | 14025 / 14080 |
| Grafana LGTM (OTLP) | 13000 / 4318 |
| Web client (usual) | 14701 |

## Configuration

Defaults are in `Revolt.toml`. Your own values go in `Revolt.overrides.toml`, which Git ignores.

### The default values are examples

`crates/core/config/Revolt.toml`, `Revolt.docker.toml.example`, `livekit.example.yml` and `compose.yml` hold public example values so a local stack starts with no setup. They are published with the upstream project: **never use them outside your own machine.**

| Setting | Generate with |
| --- | --- |
| `files.encryption_key` | `openssl rand -base64 32` |
| `pushd.vapid.private_key`, `public_key` | `openssl ecparam -name prime256v1 -genkey -noout -out vapid_private.pem`, then base64 the key pair (see `generate_config.sh` in [zrzur/self-hosted](https://github.com/zrzur/self-hosted)) |
| LiveKit `secret`, `key` | `openssl rand -hex 24`, `openssl rand -hex 6` |
| RabbitMQ, MinIO/S3 and SMTP passwords | `openssl rand -base64 32` |

The default MongoDB and Redis addresses have no password and listen on 127.0.0.1 only. Set a password if they are reachable from anywhere else. There is no JWT or session secret to set: session tokens are random and stored per session.

### Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `RUST_LOG` | `info` | Log filter |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4318` | Collector base URL |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | | OTLP logs |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | | OTLP traces |
| `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | | OTLP metrics |
| `OTEL_RESOURCE_ATTRIBUTES` | | Extra resource attributes |
| `OTEL_TRACES_SAMPLER` | `parentbased_always_on` | Trace sampler |
| `OTEL_TRACES_SAMPLER_ARG` | `1.0` | Sampling ratio |

Sentry is optional: `[sentry]` in `Revolt.overrides.toml`.

### When a port is taken

Move the data services to other ports in three files:

```yaml
# compose.override.yml
services:
  redis:
    ports: !override
      - "14079:6379"
  database:
    ports: !override
      - "14017:27017"
  rabbit:
    ports: !override
      - "14072:5672"
      - "14672:15672"
  otel-lgtm:
    ports: !override
      - "13000:3000"
      - "14318:4318"
```

```toml
# Revolt.overrides.toml
[database]
mongodb = "mongodb://127.0.0.1:14017/?directConnection=true"
redis = "redis://127.0.0.1:14079/"

[rabbit]
port = 14072
```

```bash
# .env
OTEL_EXPORTER_OTLP_ENDPOINT = "http://localhost:14318"
```

## Testing

```bash
docker compose up -d
TEST_DB=REFERENCE cargo nextest run   # in-memory database
TEST_DB=MONGODB cargo nextest run     # database-test from compose.yml (in memory, port 27018)
```

## Contributing and security

- Contributing: [CONTRIBUTING.md](./CONTRIBUTING.md). Keep changes focused and keep the attribution.
- Security reports: [SECURITY.md](./SECURITY.md).
- Changes: [CHANGELOG.md](./CHANGELOG.md).

## Licence and origin

Zrzur is an independent, Arabic-first chat platform based on the open-source Stoat software (formerly Revolt), licensed under AGPL-3.0 or later. Zrzur is not affiliated with Stoat, stoat.chat or Revolt Platforms Ltd. Please do not report Zrzur issues to the Stoat team.

- Licence: AGPL-3.0-or-later, see [LICENSE](./LICENSE). Individual crates may supply their own licences.
- Upstream: [stoatchat/stoatchat](https://github.com/stoatchat/stoatchat), basis tag `v0.16.2`.
- Notices and changes: [NOTICE](./NOTICE), [MODIFICATIONS.md](./MODIFICATIONS.md), [THIRD-PARTY-LICENSES.md](./THIRD-PARTY-LICENSES.md).
