#!/usr/bin/env bash
# Records a new docview version: sets it in Cargo.toml, Cargo.lock and the PKGBUILD,
# commits, and tags it vVERSION. Nothing is pushed.
#
#   scripts/release.sh 0.1.1
#
# Write packaging/release-notes/vVERSION.md first: it becomes the commit's and the tag's
# description, and the GitHub release's notes. Then push with
#
#   git push && git push origin vVERSION
#
# and start the Release workflow on the tag:
#
#   gh workflow run release.yml --ref vVERSION
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:-}"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
    echo "usage: scripts/release.sh VERSION   (for example 0.1.1)" >&2
    exit 2
fi
tag="v$version"
notes="packaging/release-notes/$tag.md"

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    echo "$tag already exists." >&2
    exit 1
fi
if [[ ! -s "$notes" ]]; then
    echo "Write $notes first: what changed in $tag, for the tag and the release." >&2
    exit 1
fi
if [[ -n "$(git status --porcelain -- . ":!$notes")" ]]; then
    echo "Commit or set aside other changes first; the release commit holds only the version." >&2
    exit 1
fi

# The package's own version line, the first in the file.
sed -i "0,/^version = \".*\"/s//version = \"$version\"/" Cargo.toml
cargo update --workspace --offline --quiet
# pkgver has no '-': a pre-release suffix becomes '_'.
sed -i "s/^pkgver=.*/pkgver=${version//-/_}/" packaging/PKGBUILD

git add Cargo.toml Cargo.lock packaging/PKGBUILD "$notes"
git commit -q -F - <<MSG
Release $tag

$(cat "$notes")
MSG
git tag -a "$tag" -F "$notes"

echo "Tagged $tag. Push with: git push && git push origin $tag"
echo "Then build the release: gh workflow run release.yml --ref $tag"
