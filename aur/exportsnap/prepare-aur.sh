#!/usr/bin/env bash
# prepare-aur.sh — refresh checksums and .SRCINFO before committing/pushing to AUR.
set -Eeuo pipefail
# Resolve symlinks so the script works when invoked via a path that goes through one.
cd "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"

updpkgsums
makepkg --printsrcinfo > .SRCINFO
echo 'updated PKGBUILD checksums + .SRCINFO'
