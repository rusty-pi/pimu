#!/usr/bin/env bash
#
# An apt repository of the .deb files in $1, written to $2: `pool/` with the
# packages, `dists/stable/` with the indexes and the signed `Release`, and
# `pimu.gpg`, the public key to verify them with. Only these files are needed to
# serve it, from any static host.
#
# The key is the ASCII-armoured private key in $APT_GPG_PRIVATE_KEY, without a
# passphrase. It is imported into a throwaway keyring, so nothing is left behind
# in the caller's. Without it the script refuses, since an unsigned repository
# would make every client pass `[trusted=yes]`.
set -euo pipefail

debs="${1:?usage: build-apt-repo.sh <directory with .deb files> <output directory>}"
out="${2:?usage: build-apt-repo.sh <directory with .deb files> <output directory>}"
: "${APT_GPG_PRIVATE_KEY:?the repository is signed with the key in APT_GPG_PRIVATE_KEY}"

shopt -s nullglob
packages=("$debs"/*.deb)
[[ ${#packages[@]} -gt 0 ]] || { echo "no .deb files in $debs" >&2; exit 1; }

suite=stable
component=main
architectures=$(for deb in "${packages[@]}"; do dpkg-deb --field "$deb" Architecture; done | sort -u | tr '\n' ' ')

rm -rf "$out"
mkdir -p "$out/pool"
cp "${packages[@]}" "$out/pool/"

export GNUPGHOME
GNUPGHOME=$(mktemp -d)
trap 'gpgconf --kill gpg-agent 2>/dev/null || true; rm -rf "$GNUPGHOME"' EXIT
printf '%s\n' "$APT_GPG_PRIVATE_KEY" | gpg --batch --quiet --import
key=$(gpg --batch --list-secret-keys --with-colons | awk -F: '/^fpr/ { print $10; exit }')
[[ -n $key ]] || { echo "APT_GPG_PRIVATE_KEY holds no secret key" >&2; exit 1; }

cd "$out"
for arch in $architectures; do
    index="dists/$suite/$component/binary-$arch"
    mkdir -p "$index"
    apt-ftparchive --arch "$arch" packages pool > "$index/Packages"
    gzip -9nk "$index/Packages"
done

apt-ftparchive \
    -o "APT::FTPArchive::Release::Origin=pimu" \
    -o "APT::FTPArchive::Release::Label=pimu" \
    -o "APT::FTPArchive::Release::Suite=$suite" \
    -o "APT::FTPArchive::Release::Codename=$suite" \
    -o "APT::FTPArchive::Release::Architectures=${architectures% }" \
    -o "APT::FTPArchive::Release::Components=$component" \
    release "dists/$suite" > Release.new
mv Release.new "dists/$suite/Release"

gpg --batch --yes --local-user "$key" --clearsign \
    --output "dists/$suite/InRelease" "dists/$suite/Release"
gpg --batch --yes --local-user "$key" --armor --detach-sign \
    --output "dists/$suite/Release.gpg" "dists/$suite/Release"
gpg --batch --export "$key" > pimu.gpg
