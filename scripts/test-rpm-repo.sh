#!/usr/bin/env bash
#
# Builds a repository from two throwaway packages with a throwaway key, then
# checks what a client does: the signature of `repomd.xml` verifies, each
# package is signed with the key, each architecture's index lists its package,
# and the package's hash is the one the index promises. Needs rpmbuild,
# rpmsign, createrepo_c and gpg.
set -euo pipefail
cd "$(dirname "$0")/.."

work=$(mktemp -d)
trap 'GNUPGHOME=$work/keys gpgconf --kill gpg-agent 2>/dev/null || true; rm -rf "$work"' EXIT

mkdir -p "$work/rpms"
for arch in x86_64 aarch64; do
    cat > "$work/pimu.spec" <<SPEC
Name: pimu
Version: 1.2.3
Release: 1
Summary: test package
License: MIT
%description
test package
%files
SPEC
    rpmbuild -bb --quiet --target "$arch" --define "_topdir $work/top" \
        --define "_rpmdir $work/built" "$work/pimu.spec" 2>&1 | grep -v changelog || true
    cp "$work/built/$arch/pimu-1.2.3-1.$arch.rpm" "$work/rpms/"
done

mkdir -m 700 "$work/keys"
GNUPGHOME="$work/keys" gpg --batch --quiet --passphrase '' --quick-generate-key 'pimu test <test@example.com>' default default never
secret=$(GNUPGHOME="$work/keys" gpg --batch --armor --export-secret-keys)

if APT_GPG_PRIVATE_KEY= scripts/build-rpm-repo.sh "$work/rpms" "$work/none" 2>/dev/null; then
    echo "FAIL: built a repository without a key" >&2
    exit 1
fi

APT_GPG_PRIVATE_KEY=$secret scripts/build-rpm-repo.sh "$work/rpms" "$work/repo"

gpg --batch --quiet --dearmor < "$work/repo/pimu.asc" > "$work/pimu.keyring"
rpm --dbpath "$work/rpmdb" --initdb
rpm --dbpath "$work/rpmdb" --import "$work/repo/pimu.asc"
for arch in x86_64 aarch64; do
    repodata="$work/repo/$arch/repodata"
    gpgv --keyring "$work/pimu.keyring" "$repodata/repomd.xml.asc" "$repodata/repomd.xml" 2>/dev/null
    [[ -f $work/repo/$arch/pimu-1.2.3-1.$arch.rpm ]]
    rpm --dbpath "$work/rpmdb" --checksig "$work/repo/$arch/pimu-1.2.3-1.$arch.rpm" | grep -q 'signatures OK'
    ! rpm --dbpath "$work/rpmdb" --checksig "$work/rpms/pimu-1.2.3-1.$arch.rpm" | grep -q 'signatures OK'
    [[ $(ls "$work/repo/$arch"/*.rpm | wc -l) -eq 1 ]]
    primary=$(zstd -dc "$repodata"/*-primary.xml.zst 2>/dev/null || gzip -dc "$repodata"/*-primary.xml.gz)
    grep -q "<arch>$arch</arch>" <<<"$primary"
    sha=$(sha256sum "$work/repo/$arch/pimu-1.2.3-1.$arch.rpm" | cut -d' ' -f1)
    grep -q "<checksum type=\"sha256\" pkgid=\"YES\">$sha</checksum>" <<<"$primary"
done
echo "rpm repository ok"
