#!/bin/sh
# Regenerate .github/workflows/release.yml with dist, then patch two
# problems in dist 0.34's output for custom publish jobs:
#   1. It writes `uses: $/.github/workflows/...`; GitHub needs `./`.
#   2. It grants the job only id-token and packages; checkout also needs
#      `contents: read`.
# dist-workspace.toml sets allow-dirty = ["ci"] so dist's CI checks accept
# the patched file. With that set, `dist generate` skips the workflow, so it
# is hidden for the duration of the run.
set -eu
cd "$(dirname "$0")/.."
cfg=dist-workspace.toml
cp "$cfg" "$cfg.orig"
trap 'mv "$cfg.orig" "$cfg"' EXIT
perl -ni -e 'print unless /^allow-dirty\s*=/' "$cfg"
dist generate
f=.github/workflows/release.yml
perl -pi \
    -e 's{uses: \$/\.github/workflows/}{uses: ./.github/workflows/};' \
    -e 's{^(      )"id-token": "write"$}{$1"contents": "read"\n$1"id-token": "write"};' \
    "$f"
if grep -q 'uses: \$/' "$f"; then
    echo "error: unpatched custom job path in $f" >&2
    exit 1
fi
echo "patched $f"
