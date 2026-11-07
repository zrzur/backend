#!/usr/bin/env bash
# Build Zrzur-controlled backend images from this source tree only.
# Tags ghcr.io/zrzur/<service>:${ZRZUR_BACKEND_TAG:-local}, the names the
# self-hosted compose file uses. Does not push to any registry.
#   scripts/build-zrzur-images.sh
#   cd ../self-hosted && ZRZUR_BACKEND_TAG=local docker compose up -d
set -euo pipefail
cd "$(dirname "$0")/.."

echo "Building zrzur/base:local from Dockerfile (rust toolchain + workspace deps)..."
docker build --build-arg CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" -t zrzur/base:local -f Dockerfile .

build_one() {
  local name="$1"
  local recipe="$2"
  local tag="ghcr.io/zrzur/${name}:${ZRZUR_BACKEND_TAG:-local}"
  echo "Building ${tag} from ${recipe}..."
  docker build \
    --label "org.opencontainers.image.version=$(tail -n 1 ZRZUR_VERSION)" \
    -t "${tag}" -f "${recipe}" .
}

build_one api crates/delta/Dockerfile
build_one events crates/bonfire/Dockerfile
build_one autumn crates/services/autumn/Dockerfile
build_one january crates/services/january/Dockerfile
build_one gifbox crates/services/gifbox/Dockerfile
build_one crond crates/daemons/crond/Dockerfile
build_one pushd crates/daemons/pushd/Dockerfile
build_one voice-ingress crates/daemons/voice-ingress/Dockerfile

echo "Done. Local tags:"
docker images 'ghcr.io/zrzur/*' --format 'table {{.Repository}}:{{.Tag}}\t{{.ID}}\t{{.CreatedSince}}'
