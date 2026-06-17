#!/bin/sh
# cctop installer — downloads the latest prebuilt binary for your platform.
#   curl -fsSL https://raw.githubusercontent.com/y0av/cctop/master/install.sh | sh
set -e

REPO="y0av/cctop"
BIN="cctop"
INSTALL_DIR="${CCTOP_INSTALL_DIR:-$HOME/.local/bin}"

err() { echo "error: $*" >&2; exit 1; }

# Detect platform → release asset target triple.
os=$(uname -s)
arch=$(uname -m)
case "$os" in
    Linux)  os_part="unknown-linux-gnu" ;;
    Darwin) os_part="apple-darwin" ;;
    *) err "unsupported OS '$os' — install with: cargo install --git https://github.com/$REPO" ;;
esac
case "$arch" in
    x86_64|amd64) arch_part="x86_64" ;;
    arm64|aarch64) arch_part="aarch64" ;;
    *) err "unsupported arch '$arch' — install with: cargo install --git https://github.com/$REPO" ;;
esac
target="${arch_part}-${os_part}"
asset="${BIN}-${target}.tar.gz"

# Resolve the latest release tag via the GitHub API.
tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep '"tag_name"' | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
[ -n "$tag" ] || err "could not find a release — install with: cargo install --git https://github.com/$REPO"

url="https://github.com/$REPO/releases/download/$tag/$asset"
echo "downloading $BIN $tag ($target)..."

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$url" -o "$tmp/$asset" \
    || err "no prebuilt binary for '$target' in $tag — install with: cargo install --git https://github.com/$REPO"
tar -xzf "$tmp/$asset" -C "$tmp"

mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/$BIN" "$INSTALL_DIR/$BIN"
echo "installed $BIN -> $INSTALL_DIR/$BIN"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "note: add $INSTALL_DIR to your PATH, e.g.:"
       echo "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.profile" ;;
esac
echo "run: $BIN"
