#!/bin/sh
set -eu

REPO="ixxie/cella"
INSTALL_DIR="${CELLA_INSTALL_DIR:-/usr/local/bin}"

detect_platform() {
  os=$(uname -s | tr '[:upper:]' '[:lower:]')
  arch=$(uname -m)

  case "$os" in
    linux) os="linux" ;;
    darwin) os="darwin" ;;
    *) echo "unsupported OS: $os" >&2; exit 1 ;;
  esac

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *) echo "unsupported architecture: $arch" >&2; exit 1 ;;
  esac

  echo "cella-${arch}-${os}"
}

main() {
  artifact=$(detect_platform)

  if [ -n "${CELLA_VERSION:-}" ]; then
    tag="v${CELLA_VERSION}"
    url="https://github.com/${REPO}/releases/download/${tag}/${artifact}.tar.gz"
  else
    url="https://github.com/${REPO}/releases/latest/download/${artifact}.tar.gz"
  fi

  echo "downloading ${url}..."
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT

  curl -fsSL "$url" -o "${tmp}/cella.tar.gz"
  tar xzf "${tmp}/cella.tar.gz" -C "$tmp"

  if [ -w "$INSTALL_DIR" ]; then
    mv "${tmp}/cella" "${INSTALL_DIR}/cella"
  else
    echo "installing to ${INSTALL_DIR} (requires sudo)..."
    sudo mv "${tmp}/cella" "${INSTALL_DIR}/cella"
  fi

  chmod +x "${INSTALL_DIR}/cella"

  # symlink git remote helper
  ln -sf "${INSTALL_DIR}/cella" "${INSTALL_DIR}/git-remote-cella"

  echo "installed cella to ${INSTALL_DIR}/cella"
}

main
