#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "${1:-}" != "--no-build" ]]; then
    cargo build --release --locked --manifest-path "$project_dir/Cargo.toml"
fi
mkdir -p "$HOME/.local/bin"
binary_tmp="$(mktemp "$HOME/.local/bin/.ai-top.XXXXXX")"
trap 'rm -f -- "$binary_tmp"' EXIT
install -m755 "$project_dir/target/release/ai-top" "$binary_tmp"
mv -f -- "$binary_tmp" "$HOME/.local/bin/ai-top"
echo "Installed $HOME/.local/bin/ai-top"
