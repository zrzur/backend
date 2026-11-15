#!/usr/bin/env bash
# One version (audit ZR-002): the product version the API reports
# (ZRZUR_VERSION, read by crates/delta/build.rs) equals RELEASE.json.
# Usage: bash scripts/check-version.sh [repo root]
set -euo pipefail
root="${1:-$(dirname "$0")/..}"
product="$(grep -v '^[[:space:]]*#' "$root/ZRZUR_VERSION" | grep -v '^[[:space:]]*$' | head -1 | tr -d '[:space:]')"
release="$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' "$root/RELEASE.json" | head -1)"
target="$(sed -n 's/^ *"targetVersion": *"\([^"]*\)".*/\1/p' "$root/RELEASE.json" | head -1)"
if ! printf '%s' "$product" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta|rc)\.[0-9]+)?$'; then
  echo "ZRZUR_VERSION '$product' is not a version"; exit 1
fi
if [ "$product" != "$release" ] || [ "$release" != "$target" ]; then
  echo "version mismatch: ZRZUR_VERSION=$product RELEASE.json version=$release targetVersion=$target"; exit 1
fi
workflow="$root/.github/workflows/source-check.yml"
if [ -f "$workflow" ] && ! grep -qF "assert release['version'] == '$product'" "$workflow"; then
  echo "source-check.yml expects another version than $product"; exit 1
fi
echo "version $product: ZRZUR_VERSION, RELEASE.json and source-check.yml agree"
