#!/usr/bin/env bash
#
# The files of the page that GitHub Pages serves, copied from web/ into $1:
# what the page loads, not the tests and not their tooling. web/pkg/ goes along
# when it has been built.
set -euo pipefail
cd "$(dirname "$0")/.."

out="${1:?usage: assemble-site.sh <directory>}"
mkdir -p "$out"
cp web/index.html web/style.css "$out/"
cp $(ls web/*.mjs | grep -Ev '\.test\.mjs$|^web/playwright\.config\.mjs$') "$out/"
if [[ -d web/pkg ]]; then
    cp -r web/pkg "$out/pkg"
    rm -f "$out"/pkg/.gitignore "$out"/pkg/package.json "$out"/pkg/*.d.ts
fi
touch "$out/.nojekyll"
