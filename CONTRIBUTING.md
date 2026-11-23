# Contributing to Zrzur

Thank you for helping. Keep each change small and focused, with the tests that show it works.

Zrzur is a volunteer project. It is built in free time, alongside work and school, so a review can take a while. Thank you for your patience.

## Where changes go

- `main` is the released version. `beta` is what comes next.
- Branch from `beta` and open your pull request into `beta`.
- One topic per pull request. Fill in the checklist in the pull request template.

## Before you open a pull request

- Run the build and the tests listed in [README.md](./README.md).
- New user-facing text comes in English, Arabic (Modern Standard Arabic) and German.
- Keep every licence file and copyright notice as it is. Code that came from Stoat (formerly Revolt) keeps its notices.
- Never commit passwords, keys, tokens, `.env` files, real configuration, logs, build output or user data.

## The publication guard

Every public Zrzur repository checks its files against `PUBLIC_SOURCE_MANIFEST.json`, so nothing private is published by mistake. If CI stops with `source differs from reviewed manifest`:

```bash
python3 scripts/publication_guard.py --write   # writes a candidate manifest
git diff PUBLIC_SOURCE_MANIFEST.json            # check that only your files changed
python3 scripts/publication_guard.py            # must print PASS
```

Commit the manifest with your change. The maintainer reviews it before merging.

## Developer Certificate of Origin

Every commit must be signed off. By signing off you confirm the
[Developer Certificate of Origin 1.1](https://developercertificate.org/): that you wrote the
change, or have the right to submit it under this repository's licence.

```bash
git commit -s -m "Short description of the change"
```

Your contribution is published under the licence in [LICENSE](./LICENSE).

## Issues

- **Bugs and ideas:** open an issue in this repository and choose a form.
- **Security problems:** do not open an issue. Follow [SECURITY.md](./SECURITY.md).
- **Illegal content on Zrzur:** report it at [zrzur.com/report](https://zrzur.com/report).

Please do not report Zrzur issues to the Stoat team.

## Conduct

This project follows the [Code of Conduct](./CODE_OF_CONDUCT.md).
