#!/usr/bin/env sh
# Build every image, per spec/18-deployment.md §2.4 — one per `[[bin]]`,
# named after the Python service it replaces so the Container App, the ADO
# pipeline and the `.env` file all keep their names.
#
#   docker/build-all.sh [tag]
set -eu

tag="${1:-dev}"
cd "$(dirname "$0")/.."

# The binary names, read from the manifests rather than listed here, so a
# new service cannot be forgotten.
binaries="$(awk '/^\[\[bin\]\]/{f=1; next} f&&/^name = /{gsub(/"/,""); print $3; f=0}' crates/*/Cargo.toml | sort)"

for binary in $binaries; do
    printf '=== %s\n' "$binary"
    podman build \
        -f docker/Dockerfile \
        --build-arg "BIN=$binary" \
        -t "ihb-rs/$binary:$tag" \
        .
done

printf '\nbuilt %s images\n' "$(printf '%s\n' "$binaries" | wc -l | tr -d ' ')"
