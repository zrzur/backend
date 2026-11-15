#!/usr/bin/env bash
# Shows that check-version.sh fails on a mismatch and passes on a match.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/zrzur-version.XXXXXXXX")"
trap 'case "$work" in */zrzur-version.????????) rm -rf -- "$work" ;; esac' EXIT
printf '# comment\n26.1.0-beta.2\n' > "$work/ZRZUR_VERSION"
printf '{\n  "version": "26.1.0-beta.1",\n  "targetVersion": "26.1.0-beta.1"\n}\n' > "$work/RELEASE.json"
if bash "$here/check-version.sh" "$work" >/dev/null; then echo "FAIL: a mismatch passed"; exit 1; fi
printf '{\n  "version": "26.1.0-beta.2",\n  "targetVersion": "26.1.0-beta.2"\n}\n' > "$work/RELEASE.json"
bash "$here/check-version.sh" "$work" >/dev/null || { echo "FAIL: a match failed"; exit 1; }
echo "check-version self-test: ok (mismatch fails, match passes)"
