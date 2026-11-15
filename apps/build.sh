#!/usr/bin/env bash
# Builds zdx-go, the one Go binary, without cargo. By default it embeds the
# Mini App (needs bun); --no-web skips it, for commands that do not serve the
# Mini App (the gateway then needs --web-dist to serve one).
# Usage: apps/build.sh [--no-web] [OUTPUT] (default apps/bin/zdx-go)
set -euo pipefail
apps="$(cd "$(dirname "$0")" && pwd)"
web=true
if [ "${1:-}" = "--no-web" ]; then
	web=false
	shift
fi
out="${1:-$apps/bin/zdx-go}"
tags=()

if $web; then
	(cd "$apps/web" && bun run build-cached.mjs)
	test -f "$apps/web/dist/index.html"
	dist="$apps/gateway/internal/gateway/webdist/dist"
	rm -rf "$dist"
	cp -R "$apps/web/dist" "$dist"
	tags=(-tags webdist)
fi

cd "$apps"
go build ${tags[@]+"${tags[@]}"} -trimpath -ldflags "$("$apps/shared/ldflags.sh")" -o "$out" ./cmd/zdx-go
echo "$out"
