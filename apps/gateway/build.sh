#!/usr/bin/env bash
# Builds zdx-gateway with the Mini App embedded, without cargo.
# Usage: apps/gateway/build.sh [OUTPUT] (default apps/gateway/bin/zdx-gateway)
set -euo pipefail
gateway="$(cd "$(dirname "$0")" && pwd)"
web="$gateway/../web"
out="${1:-$gateway/bin/zdx-gateway}"

(cd "$web" && bun run build-cached.mjs)
test -f "$web/dist/index.html"

dist="$gateway/internal/gateway/webdist/dist"
rm -rf "$dist"
cp -R "$web/dist" "$dist"

cd "$gateway"
go build -tags webdist -trimpath -ldflags "$("$gateway/../shared/ldflags.sh")" -o "$out" ./cmd/zdx-gateway
echo "$out"
