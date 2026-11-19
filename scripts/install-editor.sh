#!/bin/sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cargo install --path "$root/crates/check-cli"
cd "$root/editors/vscode"
npm ci
npx vsce package --skip-license --out check.vsix
code --install-extension check.vsix --force
rm check.vsix
