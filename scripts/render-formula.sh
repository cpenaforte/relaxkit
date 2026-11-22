#!/bin/sh
# Renders the Homebrew formula for one release (Task 27 part A).
#
# Usage: render-formula.sh VERSION CHECKSUMS_FILE > relaxkit.rb
#
# CHECKSUMS_FILE is the relaxkit-<version>-checksums.txt the release workflow writes: one `<sha256>  <asset>` line per
# asset. The formula is written to stdout. Nothing is written when any of the four checksums is missing or malformed.
set -eu

fail() {
  echo "render-formula.sh: $*" >&2
  exit 1
}

if [ "$#" -ne 2 ]; then
  echo "usage: render-formula.sh VERSION CHECKSUMS_FILE" >&2
  exit 2
fi
version=$1
checksums=$2

if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'; then
  fail "not a version: $version"
fi
[ -r "$checksums" ] || fail "cannot read $checksums"

here=$(cd "$(dirname "$0")" && pwd)
template="$here/../packaging/homebrew/relaxkit.rb.tmpl"
[ -r "$template" ] || fail "cannot read $template"

# Prints the sha256 of one release asset. Exactly one checksums line must name it.
checksum_of() {
  asset="relaxkit-$version-$1.tar.gz"
  hash=$(awk -v name="$asset" '
    { file = $2; sub(/^\*/, "", file) }
    file == name { print $1; found++ }
    END { if (found != 1) exit 1 }
  ' "$checksums") || fail "the checksums file has no single entry for $asset"
  printf '%s\n' "$hash" | grep -Eq '^[0-9a-f]{64}$' || fail "the checksum for $asset is not a sha256 hash"
  printf '%s\n' "$hash"
}

sha_aarch64_apple_darwin=$(checksum_of aarch64-apple-darwin)
sha_x86_64_apple_darwin=$(checksum_of x86_64-apple-darwin)
sha_aarch64_unknown_linux_musl=$(checksum_of aarch64-unknown-linux-musl)
sha_x86_64_unknown_linux_musl=$(checksum_of x86_64-unknown-linux-musl)

rendered=$(sed \
  -e "s|@VERSION@|$version|g" \
  -e "s|@SHA256_AARCH64_APPLE_DARWIN@|$sha_aarch64_apple_darwin|g" \
  -e "s|@SHA256_X86_64_APPLE_DARWIN@|$sha_x86_64_apple_darwin|g" \
  -e "s|@SHA256_AARCH64_UNKNOWN_LINUX_MUSL@|$sha_aarch64_unknown_linux_musl|g" \
  -e "s|@SHA256_X86_64_UNKNOWN_LINUX_MUSL@|$sha_x86_64_unknown_linux_musl|g" \
  "$template")

if printf '%s\n' "$rendered" | grep -Eq '@[A-Z0-9_]+@'; then
  fail "the template has a placeholder this script does not fill"
fi
printf '%s\n' "$rendered"
