# Zrzur 26 — release 26.1.0 Beta

This repository is one component of Zrzur. The shared product version is **26.1.0-beta.1** (shown as "26.1.0 Beta"), on **main** (tag `v26.1.0-beta.1`). It is a public beta: what runs on app.zrzur.com, with features that may still change. The previous release is 26.0.0 (tag `v26.0.0`). Original dependency, SDK and protocol versions remain compatibility identifiers.

| Component | Source |
|---|---|
| Backend | https://github.com/zrzur/backend |
| Web | https://github.com/zrzur/web |
| Desktop | https://github.com/zrzur/desktop |
| Self-hosted | https://github.com/zrzur/self-hosted |
| JavaScript SDK | https://github.com/zrzur/sdk-js (npm `@zrzur/sdk`) |
| Website | https://github.com/zrzur/website |

Upstream licences, copyright notices and attribution are retained (see NOTICE). Git history was not carried over: this repository starts with a single initial commit.

## Known limits of 26.1.0 Beta

- It is a beta: features and wording may still change before the next stable release.
- Desktop apps are not code-signed: Windows SmartScreen and macOS Gatekeeper ask before the first start. There is no automatic update yet; new versions are downloaded from zrzur.com/download.
- The Linux app needs the system NSS libraries (`libnss3`).
- Do not publish development configuration or reuse demo credentials in production.
