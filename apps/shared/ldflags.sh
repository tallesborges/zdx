#!/usr/bin/env bash
# Prints the Go linker flag that sets version.Zdx to the workspace version in
# Cargo.toml (Rust's CARGO_PKG_VERSION). Usage: go build -ldflags "$(apps/shared/ldflags.sh)"
set -euo pipefail
cargo_toml="$(cd "$(dirname "$0")/../.." && pwd)/Cargo.toml"
version="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version *= *"\([^"]*\)".*/\1/p' "$cargo_toml")"
if [ -z "$version" ]; then
	echo "ldflags.sh: no [workspace.package] version in $cargo_toml" >&2
	exit 1
fi
echo "-X github.com/tallesborges/zdx/apps/shared/version.Zdx=$version"
