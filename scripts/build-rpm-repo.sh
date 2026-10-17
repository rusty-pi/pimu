#!/usr/bin/env bash
#
# A yum/dnf repository of the .rpm files in $1, written to $2: one directory per
# architecture (`x86_64/`, `aarch64/`) holding its packages and a `repodata/`
# whose `repomd.xml` is signed, and `pimu.asc`, the public key to verify it with.
# Only these files are needed to serve it, from any static host.
#
# The key is the ASCII-armoured private key in $APT_GPG_PRIVATE_KEY, without a
# passphrase, imported into a throwaway keyring. It signs every package, in the
# copies written to $2 (the files in $1 are left as they are), and each
# `repomd.xml`, so a client can check both `gpgcheck` and `repo_gpgcheck`.
# Without the key the script refuses.
set -euo pipefail

rpms="${1:?usage: build-rpm-repo.sh <directory with .rpm files> <output directory>}"
out="${2:?usage: build-rpm-repo.sh <directory with .rpm files> <output directory>}"
: "${APT_GPG_PRIVATE_KEY:?the repository is signed with the key in APT_GPG_PRIVATE_KEY}"

shopt -s nullglob
packages=("$rpms"/*.rpm)
[[ ${#packages[@]} -gt 0 ]] || { echo "no .rpm files in $rpms" >&2; exit 1; }

rm -rf "$out"
mkdir -p "$out"
for rpm in "${packages[@]}"; do
    arch=$(rpm -qp --qf '%{ARCH}' "$rpm" 2>/dev/null)
    mkdir -p "$out/$arch"
    cp "$rpm" "$out/$arch/"
done

export GNUPGHOME
GNUPGHOME=$(mktemp -d)
trap 'gpgconf --kill gpg-agent 2>/dev/null || true; rm -rf "$GNUPGHOME"' EXIT
printf '%s\n' "$APT_GPG_PRIVATE_KEY" | gpg --batch --quiet --import
key=$(gpg --batch --list-secret-keys --with-colons | awk -F: '/^fpr/ { print $10; exit }')
[[ -n $key ]] || { echo "APT_GPG_PRIVATE_KEY holds no secret key" >&2; exit 1; }

rpmsign --addsign --define "_gpg_name $key" --define "__gpg $(command -v gpg)" "$out"/*/*.rpm > /dev/null

for dir in "$out"/*/; do
    createrepo_c --quiet "$dir"
    gpg --batch --yes --local-user "$key" --armor --detach-sign \
        --output "$dir/repodata/repomd.xml.asc" "$dir/repodata/repomd.xml"
done
gpg --batch --armor --export "$key" > "$out/pimu.asc"
