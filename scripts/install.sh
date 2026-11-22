#!/bin/sh
# Installs relaxkit from a GitHub release on Linux and macOS. It is also the way to update: run it again.
#
# Environment:
#   RELAXKIT_VERSION       release to install, such as 1.2.3 or v1.2.3 (default: the latest release)
#   RELAXKIT_BIN_DIR       where relaxkit and the rlx alias go (default: $HOME/.local/bin)
#   RELAXKIT_RELEASE_BASE  download base; the script fetches <base>/v<version>/<file> (default: GitHub)
#   RELAXKIT_LATEST_URL    API URL that names the latest release, read only when no version is set
#                          (default: the GitHub API for cpenaforte/relaxkit)
#
# Flags:
#   --no-setup             install the binary and stop; do not run `relaxkit setup`
#   -h, --help             print this help
#
# The archive is checked against the release's checksums file (sha256). The script does not check the minisign
# signature; it prints where the signature is and how to check it with minisign.

set -eu

REPO="cpenaforte/relaxkit"
DEFAULT_BASE="https://github.com/$REPO/releases/download"
DEFAULT_LATEST="https://api.github.com/repos/$REPO/releases/latest"

usage() {
  cat <<'EOF'
Usage: install.sh [--no-setup]

Installs relaxkit into $RELAXKIT_BIN_DIR (default $HOME/.local/bin), adds the rlx alias when that name is free,
and runs `relaxkit setup` unless --no-setup is given.
EOF
}

die() {
  printf 'install.sh: %s\n' "$1" >&2
  exit 1
}

# Download URL DEST. curl fails on an HTTP error, so a missing file never leaves a partial success behind.
fetch() {
  curl -fsSL -o "$2" "$1" || die "cannot download $1"
}

# The SHA-256 of FILE as lowercase hex, with whichever tool this machine has.
sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    die "neither shasum nor sha256sum is installed"
  fi
}

# The release target for this OS and CPU, as the release assets name them.
detect_target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Darwin)
      case "$arch" in
        arm64 | aarch64) echo aarch64-apple-darwin ;;
        x86_64) echo x86_64-apple-darwin ;;
        *) return 1 ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64 | amd64) echo x86_64-unknown-linux-musl ;;
        aarch64 | arm64) echo aarch64-unknown-linux-musl ;;
        *) return 1 ;;
      esac
      ;;
    *) return 1 ;;
  esac
}

run_setup=1
for arg in "$@"; do
  case "$arg" in
    --no-setup) run_setup=0 ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      printf 'install.sh: unknown argument: %s\n' "$arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

TARGET=$(detect_target) || die "there is no relaxkit release for $(uname -s) $(uname -m)"

if [ -n "${RELAXKIT_BIN_DIR:-}" ]; then
  bin_dir=$RELAXKIT_BIN_DIR
else
  bin_dir=${HOME:?HOME is not set; set RELAXKIT_BIN_DIR}/.local/bin
fi
base=${RELAXKIT_RELEASE_BASE:-$DEFAULT_BASE}

mkdir -p "$bin_dir"
# The work directory is inside the bin directory, so nothing is written outside it. It is removed on exit.
work=$(mktemp -d "$bin_dir/.relaxkit-install.XXXXXX") || die "cannot create a work directory in $bin_dir"
trap 'rm -rf "$work"' EXIT

if [ -n "${RELAXKIT_VERSION:-}" ]; then
  VERSION=${RELAXKIT_VERSION#v}
else
  fetch "${RELAXKIT_LATEST_URL:-$DEFAULT_LATEST}" "$work/latest.json"
  VERSION=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"[v]*\([^"]*\)".*/\1/p' "$work/latest.json" | head -n 1)
  [ -n "$VERSION" ] || die "cannot read the latest release tag"
fi
printf '%s' "$VERSION" | grep -Eq '^[0-9A-Za-z][0-9A-Za-z.+-]*$' || die "not a release version: $VERSION"

release="$base/v$VERSION"
sums_name="relaxkit-$VERSION-checksums.txt"
asset="relaxkit-$VERSION-$TARGET.tar.gz"

printf 'Installing relaxkit %s for %s\n' "$VERSION" "$TARGET"
fetch "$release/$sums_name" "$work/$sums_name"
fetch "$release/$asset" "$work/$asset"

expected=$(awk -v name="$asset" '{ f = $2; sub(/^\*/, "", f); if (f == name) { print $1; n++ } } END { if (n != 1) exit 1 }' "$work/$sums_name") \
  || die "the checksums file does not list $asset exactly once"
expected=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
printf '%s' "$expected" | grep -Eq '^[0-9a-f]{64}$' || die "the checksums file has no SHA-256 for $asset"
actual=$(sha256_of "$work/$asset")
[ "$actual" = "$expected" ] || die "checksum mismatch for $asset: the release lists $expected, the download is $actual"
echo "Checksum of $asset verified."

mkdir "$work/extract"
tar -xzf "$work/$asset" -C "$work/extract" || die "cannot unpack $asset"
if [ ! -f "$work/extract/relaxkit" ] || [ -L "$work/extract/relaxkit" ]; then
  die "$asset has no relaxkit binary"
fi

# The binary is written to a new file in the work directory, then renamed over the old one, so a failure before the
# rename leaves the installed binary as it was.
staged=$(mktemp "$work/relaxkit.XXXXXX")
cat "$work/extract/relaxkit" > "$staged"
chmod 755 "$staged"
mv -f "$staged" "$bin_dir/relaxkit"
printf 'relaxkit %s installed at %s\n' "$VERSION" "$bin_dir/relaxkit"

# The rlx alias is a relative symlink to relaxkit. An rlx that already exists, of any kind, is never replaced.
alias_path="$bin_dir/rlx"
if [ -L "$alias_path" ] && [ "$(readlink "$alias_path")" = "relaxkit" ]; then
  printf 'rlx alias already points to relaxkit.\n'
elif [ -e "$alias_path" ] || [ -L "$alias_path" ]; then
  printf 'note: %s already exists and is not a link to relaxkit, so it was left alone. Use the name relaxkit.\n' "$alias_path"
else
  ln -s relaxkit "$alias_path"
  printf 'rlx alias created: %s -> relaxkit\n' "$alias_path"
fi

if [ "$run_setup" -eq 1 ]; then
  # Setup may ask for the GitHub login. Its stdin is the terminal when there is one, never this script's pipe.
  # The probe runs in a subshell: a failed redirect on `:` makes dash exit the whole script, silently, with no terminal.
  setup_stdin=/dev/null
  if (exec </dev/tty) 2>/dev/null; then
    setup_stdin=/dev/tty
  fi
  "$bin_dir/relaxkit" setup <"$setup_stdin" \
    || die "relaxkit setup failed; the binary is installed at $bin_dir/relaxkit, so run relaxkit setup again"
fi

cat <<EOF

Release signature: $release/$sums_name.minisig
To check the checksums file with minisign, fetch $sums_name and its .minisig, then run:
  minisign -Vm $sums_name -p <path to release/minisign.pub from the relaxkit repository> -x $sums_name.minisig
EOF
