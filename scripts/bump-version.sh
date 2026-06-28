#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $(basename "$0") <new-version>" >&2
    echo "example: $(basename "$0") 0.2.4" >&2
    exit 64
fi

new_version="$1"

if [[ ! "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "error: '$new_version' is not a valid semver version" >&2
    exit 1
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo_toml="$repo_root/Cargo.toml"
package_json="$repo_root/npm/package.json"

for f in "$cargo_toml" "$package_json"; do
    if [[ ! -f "$f" ]]; then
        echo "error: $f not found" >&2
        exit 1
    fi
done

current_cargo="$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$cargo_toml" | head -n1)"
current_npm="$(sed -n 's/.*"version": "\([^"]*\)".*/\1/p' "$package_json" | head -n1)"

if [[ "$current_cargo" != "$current_npm" ]]; then
    echo "warning: Cargo.toml ($current_cargo) and npm/package.json ($current_npm) versions differ" >&2
fi

echo "bumping: cargo $current_cargo -> $new_version"
echo "bumping: npm   $current_npm -> $new_version"

# Cargo.toml: replace the first `version = "..."` line (the [package] one).
sed -i "0,/^version = \"[^\"]*\"/{s//version = \"$new_version\"/}" "$cargo_toml"

# npm/package.json: replace the first `"version": "..."` field.
sed -i "0,/\"version\": \"[^\"]*\"/{s//\"version\": \"$new_version\"/}" "$package_json"

echo "running cargo check to refresh Cargo.lock..."
(cd "$repo_root" && cargo check)

echo "done."
