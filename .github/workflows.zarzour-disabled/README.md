# Zarzour Phase 01 — disabled GitHub Actions (local)

These workflow files were moved out of `workflows/` so GitHub Actions will not run them.

Reason: they publish/push images or releases, target upstream Stoat orgs/registries (`ghcr.io/stoatchat`, `stoat-release[bot]`), or perform registry cleanup.

Do **not** re-enable without explicit owner approval after `zrzur/*` remotes, credentials, and image names are reviewed.

Upstream provenance is preserved by keeping the original YAML content here.
