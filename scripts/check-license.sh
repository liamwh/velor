#!/usr/bin/env bash
# License consistency gate: fails if workspace license metadata drifts away
# from PolyForm Noncommercial 1.0.0 (see LICENSE and the README license
# section). Run via `just check-license` (part of `just check`).
set -euo pipefail
cd "$(dirname "$0")/.."

expected='license = "PolyForm-Noncommercial-1.0.0"'
status=0

grep -q 'PolyForm Noncommercial License 1.0.0' LICENSE \
  || { echo "check-license: LICENSE is not PolyForm Noncommercial 1.0.0" >&2; status=1; }

grep -q 'PolyForm Noncommercial' README.md \
  || { echo "check-license: README.md does not reference the license" >&2; status=1; }

manifests=(
  crates/velor-core/Cargo.toml
  crates/automations/Cargo.toml
  crates/velor-vault/Cargo.toml
  apps/velor-cli/Cargo.toml
  apps/velor/src-tauri/Cargo.toml
)

for manifest in "${manifests[@]}"; do
  if ! grep -qF "$expected" "$manifest"; then
    echo "check-license: $manifest must declare $expected" >&2
    status=1
  fi
  if grep -qE '^license[[:space:]]*=[[:space:]]*"(UNLICENSED|MIT|Apache[^"]*)"' "$manifest"; then
    echo "check-license: $manifest declares a conflicting license" >&2
    status=1
  fi
done

exit "$status"
