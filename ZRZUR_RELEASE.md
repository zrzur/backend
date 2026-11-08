# Zrzur 26 — release 26.0.0

This repository is one component of Zrzur. The shared product version is **26.0.0**, the first official release, on **main** (tag `v26.0.0`). Original dependency, SDK and protocol versions remain compatibility identifiers.

| Component | Source |
|---|---|
| Backend | https://github.com/zrzur/backend |
| Web | https://github.com/zrzur/web |
| Desktop | https://github.com/zrzur/desktop |
| Self-hosted | https://github.com/zrzur/self-hosted |
| JavaScript SDK | https://github.com/zrzur/sdk-js |
| Website | https://github.com/zrzur/website |

Upstream licences, copyright notices and attribution are retained (see NOTICE). Git history was not carried over: this repository starts with a single initial commit.

## Known limits of 26.0.0

- Desktop apps are not code-signed: Windows SmartScreen and macOS Gatekeeper ask before the first start. There is no automatic update yet; new versions are downloaded from zrzur.com/download.
- The Linux app needs the system NSS libraries (`libnss3`).
- Do not publish development configuration or reuse demo credentials in production.
