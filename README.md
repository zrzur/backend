<div align="center">

<img src="assets/branding/bird_white.svg" alt="Zarzour" width="96" class="gh-dark-mode-only" />
<img src="assets/branding/bird_black.svg" alt="Zarzour" width="96" class="gh-light-mode-only" />

# Zarzour Backend

Rust services for **Zarzour 26**: REST API, real-time events, files, media proxy, and supporting daemons.

</div>

<br/>

## Purpose

This repository implements the server side of Zarzour: authentication, messaging, communities (servers), permissions, presence, bots, file storage, and WebSocket events.

Crate and config names such as `revolt-*` and `Revolt.toml` are **compatibility identifiers** from upstream. They are not user-facing product branding and should not be renamed without a tested migration.

## Architecture

| Crate | Path | Role |
| --- | --- | --- |
| `core/config` | [crates/core/config](crates/core/config) | Configuration (`Revolt.toml`) |
| `core/database` | [crates/core/database](crates/core/database) | Database implementation |
| `core/files` | [crates/core/files](crates/core/files) | S3 and encryption helpers |
| `core/models` | [crates/core/models](crates/core/models) | API models |
| `core/permissions` | [crates/core/permissions](crates/core/permissions) | Permission logic |
| `core/presence` | [crates/core/presence](crates/core/presence) | User presence |
| `core/result` | [crates/core/result](crates/core/result) | Result and error types |
| `core/coalesced` | [crates/core/coalesced](crates/core/coalesced) | Coalescion service |
| `delta` | [crates/delta](crates/delta) | REST API server |
| `bonfire` | [crates/bonfire](crates/bonfire) | WebSocket events server |
| `services/january` | [crates/services/january](crates/services/january) | Metadata / image proxy |
| `services/gifbox` | [crates/services/gifbox](crates/services/gifbox) | Tenor proxy |
| `services/autumn` | [crates/services/autumn](crates/services/autumn) | File server |
| `daemons/crond` | [crates/daemons/crond](crates/daemons/crond) | Timed cleanup |
| `daemons/pushd` | [crates/daemons/pushd](crates/daemons/pushd) | Push notifications |

Typical flow: clients call **Delta** (HTTP) and **Bonfire** (WebSocket); Autumn serves attachments; optional January/Gifbox proxies sit behind the edge; MongoDB, Redis/Valkey, and RabbitMQ back the stack.

## Requirements

- **MSRV:** Rust 1.86.0 or higher
- [mise](https://mise.jdx.dev/)
- Docker (for dependent services)
- Git
- mold (optional, faster linking)

> Nix users: `nix develop` enters the flake dev shell.

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

> [!TIP]
> Override `BUILDER` in `.env` to use mold:
>
> ```bash
> BUILDER = "mold --run cargo"
> ```

Default development configuration is `Revolt.toml`. Overrides go in `Revolt.overrides.toml`.

```bash
cp livekit.example.yml livekit.yml
mise start
```

Stop with Ctrl+C, then `mise docker:stop`.

Signup confirmation emails (local): http://localhost:14080

Pair with the [Zarzour Web](https://github.com/zrzur/web) client for a full local stack (repository publication pending).

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

## Configuration and environment

| Variable | Default | Purpose |
| --- | --- | --- |
| `RUST_LOG` | `info` | Log filter, e.g. `info,revolt_autumn=debug` |
| `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | | OTLP/HTTP logs |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | | OTLP/HTTP traces |
| `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | | OTLP/HTTP metrics |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4318` | Collector base URL |
| `OTEL_RESOURCE_ATTRIBUTES` | | Extra resource attributes |
| `OTEL_TRACES_SAMPLER` | `parentbased_always_on` | Trace sampler |
| `OTEL_TRACES_SAMPLER_ARG` | `1.0` | Sampling ratio |

Optional Sentry hooks can be set under `[sentry]` in `Revolt.overrides.toml`.

## Security considerations

- Do not expose MongoDB, Redis, or RabbitMQ to the public internet.
- Keep secrets out of Git; use ignored env files and deployment secret stores.
- Production hosts must be Zarzour-owned configuration — never fall back to upstream Stoat/Revolt cloud endpoints.
- Review authentication, invite, and captcha options in `Revolt.toml` before any public deployment.

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
just publish # crates
just release # binary tag flow
```

GitHub Releases under [github.com/zrzur](https://github.com/zrzur) are **pending** until the owner publishes repositories.

## Contributing

Issues and pull requests will be accepted on this repository after public publication. Keep changes focused, preserve AGPL attribution, and include the exact commands you ran. Do not open public issues for exploitable vulnerabilities — use private reporting when enabled.

## License

Generally [AGPL-3.0](./LICENSE). **Individual crates may supply their own licenses.**

## Upstream Project & Attribution

Zarzour Backend is independently developed from the open-source Stoat backend. Stoat does not sponsor or endorse Zarzour.

- Upstream: https://github.com/stoatchat/stoatchat
- Basis tag: `v0.16.2`
- License: AGPL-3.0-or-later (see [LICENSE](./LICENSE))
