#!/bin/sh
# Giwa installer for macOS and Linux. Needs no root.
#   curl -fsSL https://raw.githubusercontent.com/<repo>/main/install.sh | sh
#
# GIWA_BASE_URL     where to download from (default: the latest GitHub release of $repo)
# GIWA_INSTALL_DIR  where to put giwa (default: ~/.local/bin)
# GIWA_TARGET       release target to use instead of detecting one
set -eu

# Set this when the repository goes public.
repo="OWNER/giwa"

fail() {
    echo "giwa install failed: $1" >&2
    echo "next: $2" >&2
    exit 1
}

base="${GIWA_BASE_URL:-}"
if [ -z "$base" ]; then
    case "$repo" in
    OWNER/*) fail "no public release is configured for this copy of the script" \
        "set GIWA_BASE_URL, or build from source: cargo install --locked --path crates/cli" ;;
    esac
    base="https://github.com/$repo/releases/latest/download"
fi
base="${base%/}"

target="${GIWA_TARGET:-}"
if [ -z "$target" ]; then
    case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *) fail "unsupported architecture: $(uname -m)" "build from source: cargo install --locked --path crates/cli" ;;
    esac
    case "$(uname -s)" in
    Linux) target="$arch-unknown-linux-musl" ;;
    Darwin) target="$arch-apple-darwin" ;;
    *) fail "unsupported system: $(uname -s)" "on Windows use install.ps1" ;;
    esac
fi

dir="${GIWA_INSTALL_DIR:-$HOME/.local/bin}"
archive="giwa-$target.tar.gz"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2" || fail "cannot download $1" "check the network and GIWA_BASE_URL"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$1" -O "$2" || fail "cannot download $1" "check the network and GIWA_BASE_URL"
    else
        fail "curl or wget is needed" "install one of them, or build from source"
    fi
}

fetch "$base/$archive" "$work/$archive"
fetch "$base/SHA256SUMS" "$work/SHA256SUMS"

want="$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1; exit }' "$work/SHA256SUMS")"
[ -n "$want" ] || fail "SHA256SUMS has no entry for $archive" "check GIWA_BASE_URL"
if command -v sha256sum >/dev/null 2>&1; then
    got="$(sha256sum "$work/$archive" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
    got="$(shasum -a 256 "$work/$archive" | awk '{ print $1 }')"
else
    fail "sha256sum or shasum is needed" "install one of them, or build from source"
fi
[ "$want" = "$got" ] || fail "checksum mismatch for $archive" "do not use this download; try again or report it"

tar -xzf "$work/$archive" -C "$work"
mkdir -p "$dir"
# Move into place so a running binary can be replaced.
cp "$work/giwa" "$dir/giwa.new"
chmod 755 "$dir/giwa.new"
mv "$dir/giwa.new" "$dir/giwa"

echo "installed $("$dir/giwa" --version)"
echo 'next: giwa init --preset blog --name "My site" --yes --json'
echo "$dir/giwa"
