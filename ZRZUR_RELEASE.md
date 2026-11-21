# Zrzur 26 — release 26.2.0

This repository is one component of Zrzur. The shared product version is **26.2.0**, the official release for everyone, on **main** (tag `v26.2.0`); **beta** carries the next test versions. The previous official release is 26.0.0 (tag `v26.0.0`); the test versions before this one are 26.2.0-beta.1 and 26.2.0-beta.2. Original dependency, SDK and protocol versions remain compatibility identifiers.

| Component | Source |
|---|---|
| Backend | https://github.com/zrzur/backend |
| Web | https://github.com/zrzur/web |
| Desktop | https://github.com/zrzur/desktop |
| Self-hosted | https://github.com/zrzur/self-hosted |
| JavaScript SDK | https://github.com/zrzur/sdk-js (npm `@zrzur/sdk`) |
| Website | https://github.com/zrzur/website |

Upstream licences, copyright notices and attribution are retained (see NOTICE). Git history was not carried over: this repository starts with a single initial commit.

## Known limits of 26.2.0

- Desktop apps are not code-signed yet: Windows SmartScreen and macOS Gatekeeper ask before the first start. Downloads and checksums are on zrzur.com/download and GitHub Releases (`v26.2.0`).
- Voice clips, recorded rooms and team applications are built but switched off on zrzur.com.
- The Linux app needs the system NSS libraries (`libnss3`).
- Do not publish development configuration or reuse demo credentials in production.
