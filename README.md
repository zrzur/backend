<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/branding/bird_white.svg" />
  <img src="assets/branding/bird_black.svg" alt="Zarzour" width="64" />
</picture>

# Zarzour Backend

Rust services for **Zarzour 26**: REST API, real-time events, files, media proxy, and supporting daemons.

</div>

<br/>

## Purpose

Server side of Zarzour: authentication, messaging, communities, permissions, presence, bots, file storage, and WebSocket events.

Config file name `Revolt.toml` and crate names such as `revolt-*` are kept for compatibility with the existing protocol and toolchain.

## Architecture

| Crate | Path | Role |
| --- | --- | --- |
| `core/config` | [crates/core/config](crates/core/config) | Configuration (`Revolt.toml`) |
| `core/database` | [crates/core/database](crates/core/database) | Database |
| `core/files` | [crates/core/files](crates/core/files) | S3 and encryption helpers |
| `core/models` | [crates/core/models](crates/core/models) | API models |
| `core/permissions` | [crates/core/permissions](crates/core/permissions) | Permissions |
| `core/presence` | [crates/core/presence](crates/core/presence) | Presence |
| `core/result` | [crates/core/result](crates/core/result) | Errors |
| `core/coalesced` | [crates/core/coalesced](crates/core/coalesced) | Coalescion |
| `delta` | [crates/delta](crates/delta) | REST API |
| `bonfire` | [crates/bonfire](crates/bonfire) | WebSocket events |
| `services/january` | [crates/services/january](crates/services/january) | Metadata / image proxy |
| `services/gifbox` | [crates/services/gifbox](crates/services/gifbox) | Tenor proxy |
| `services/autumn` | [crates/services/autumn](crates/services/autumn) | File server |
| `daemons/crond` | [crates/daemons/crond](crates/daemons/crond) | Timed cleanup |
| `daemons/pushd` | [crates/daemons/pushd](crates/daemons/pushd) | Push notifications |

Clients use **Delta** (HTTP) and **Bonfire** (WebSocket). Autumn serves files. MongoDB, Redis/Valkey, and RabbitMQ back the stack.

## Requirements

- Rust 1.86.0+
- [mise](https://mise.jdx.dev/)
- Docker
- Git
- mold (optional)

Nix: `nix develop`

## Development ports

| Service | Port |
| --- | :---: |
| MongoDB | 27017 |
| Redis | 6379 |
| MinIO | 14009 |
| Maildev | 14025 / 14080 |
| Web client (typical) | 14701 |
| RabbitMQ | 5672 / 15672 |
| Grafana LGTM (OTLP) | 13000 / 4318 |
| `crates/delta` | 14702 |
| `crates/bonfire` | 14703 |
| `crates/services/autumn` | 14704 |
| `crates/services/january` | 14705 |
| `crates/services/gifbox` | 14706 |

## Build and run

```bash
git clone https://github.com/zrzur/backend.git
cd backend
mise install
mise build
```

Optional mold builder in `.env`:

```bash
BUILDER = "mold --run cargo"
```

Default config: `Revolt.toml`. Overrides: `Revolt.overrides.toml`.

```bash
cp livekit.example.yml livekit.yml
mise start
```

Stop with Ctrl+C, then `mise docker:stop`.

Local signup emails: http://localhost:14080

Web client: [Zarzour Web](https://github.com/zrzur/web)

### Port conflicts

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

## Environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `RUST_LOG` | `info` | Log filter |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | | OTLP logs |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | | OTLP traces |
| `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | | OTLP metrics |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4318` | Collector base URL |
| `OTEL_RESOURCE_ATTRIBUTES` | | Extra resource attributes |
| `OTEL_TRACES_SAMPLER` | `parentbased_always_on` | Trace sampler |
| `OTEL_TRACES_SAMPLER_ARG` | `1.0` | Sampling ratio |

Optional Sentry: `[sentry]` in `Revolt.overrides.toml`.

## Testing

```sh
docker compose up -d
TEST_DB=REFERENCE cargo nextest run
TEST_DB=MONGODB cargo nextest run
```

## Releases (maintainers)

```bash
just patch   # 0.0.X
just minor   # 0.X.0
just major   # X.0.0
just publish
just release
```

## Contributing

Issues and pull requests on this repository after publication. Keep changes focused and preserve AGPL attribution.

## License

[AGPL-3.0](./LICENSE). Individual crates may supply their own licenses.

## Upstream Project & Attribution

Zarzour Backend is independently developed from the open-source Stoat backend. Stoat does not sponsor or endorse Zarzour.

- Upstream: https://github.com/stoatchat/stoatchat
- Basis tag: `v0.16.2`
- License: AGPL-3.0-or-later (see [LICENSE](./LICENSE))
