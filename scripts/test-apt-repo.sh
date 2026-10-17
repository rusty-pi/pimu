#!/usr/bin/env bash
#
# Builds a repository from two throwaway packages with a throwaway key, then
# checks what a client does: the signature verifies, the indexes list both
# architectures, and a file's hash is the one the index promises. Needs only
# dpkg-deb, apt-ftparchive and gpg.
set -euo pipefail
cd "$(dirname "$0")/.."

work=$(mktemp -d)
trap 'GNUPGHOME=$work/keys gpgconf --kill gpg-agent 2>/dev/null || true; rm -rf "$work"' EXIT

for arch in amd64 arm64; do
    root="$work/root-$arch"
    mkdir -p "$root/DEBIAN" "$root/usr/bin" "$work/debs"
    printf '#!/bin/sh\n' > "$root/usr/bin/pimu"
    chmod +x "$root/usr/bin/pimu"
    cat > "$root/DEBIAN/control" <<CONTROL
Package: pimu
Version: 1.2.3
Architecture: $arch
Maintainer: Test <test@example.com>
Description: test package
CONTROL
    dpkg-deb --root-owner-group --build "$root" "$work/debs/pimu_1.2.3_$arch.deb" >/dev/null
done

mkdir -m 700 "$work/keys"
GNUPGHOME="$work/keys" gpg --batch --quiet --passphrase '' --quick-generate-key 'pimu test <test@example.com>' default default never
secret=$(GNUPGHOME="$work/keys" gpg --batch --armor --export-secret-keys)

if APT_GPG_PRIVATE_KEY= scripts/build-apt-repo.sh "$work/debs" "$work/none" 2>/dev/null; then
    echo "FAIL: built a repository without a key" >&2
    exit 1
fi

APT_GPG_PRIVATE_KEY=$secret scripts/build-apt-repo.sh "$work/debs" "$work/repo"
release="$work/repo/dists/stable"

gpg_verify() { gpgv --keyring "$work/pimu.keyring" "$@" 2>/dev/null; }
cp "$work/repo/pimu.gpg" "$work/pimu.keyring"
gpg_verify "$release/Release.gpg" "$release/Release"
gpg_verify "$release/InRelease"

grep -qx 'Architectures: amd64 arm64' "$release/Release"
for arch in amd64 arm64; do
    index="main/binary-$arch/Packages"
    grep -qx "Architecture: $arch" "$release/$index"
    grep -q "^Filename: pool/pimu_1.2.3_$arch.deb$" "$release/$index"
    [[ $(grep -c '^Package:' "$release/$index") -eq 1 ]]
    grep -q " $(sha256sum "$release/$index" | cut -d' ' -f1) .* $index$" "$release/Release"
    sha=$(sed -n 's/^SHA256: //p' "$release/$index")
    [[ $sha == $(sha256sum "$work/repo/pool/pimu_1.2.3_$arch.deb" | cut -d' ' -f1) ]]
done
echo "apt repository ok"
