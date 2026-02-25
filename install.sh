#!/usr/bin/env bash
#
# Switchboard CLI installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/liberuum/switchboard-cli/main/install.sh | bash
#
# Environment variables:
#   INSTALL_DIR   — where to place the binary (default: /usr/local/bin)
#   VERSION       — specific version to install (default: latest)

set -euo pipefail

REPO="liberuum/switchboard-cli"
BINARY_NAME="switchboard"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
TMPDIR_CLEANUP=""

cleanup() { [ -n "$TMPDIR_CLEANUP" ] && rm -rf "$TMPDIR_CLEANUP"; }
trap cleanup EXIT

# --- helpers ----------------------------------------------------------------

info()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()    { printf '\033[1;32m ✓\033[0m  %s\n' "$*"; }
err()   { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

need_cmd() {
  command -v "$1" > /dev/null 2>&1 || err "Required command not found: $1"
}

# --- detect platform --------------------------------------------------------

detect_platform() {
  local os arch

  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Linux*)  os="linux" ;;
    Darwin*) os="darwin" ;;
    *) err "Unsupported OS: $os (only Linux and macOS are supported)" ;;
  esac

  case "$arch" in
    x86_64|amd64)  arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *) err "Unsupported architecture: $arch" ;;
  esac

  echo "${os}-${arch}"
}

# --- resolve version --------------------------------------------------------

resolve_version() {
  if [ -n "${VERSION:-}" ]; then
    echo "$VERSION"
    return
  fi

  need_cmd curl

  local latest
  latest="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' \
    | head -1 \
    | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"

  [ -n "$latest" ] || err "Could not determine latest version. Set VERSION= explicitly."
  echo "$latest"
}

# --- download and install ---------------------------------------------------

install() {
  need_cmd curl
  need_cmd tar

  local platform version archive_name url tmpdir

  platform="$(detect_platform)"
  version="$(resolve_version)"
  archive_name="${BINARY_NAME}-${version}-${platform}.tar.gz"
  url="https://github.com/${REPO}/releases/download/${version}/${archive_name}"

  info "Installing ${BINARY_NAME} ${version} (${platform})"

  tmpdir="$(mktemp -d)"
  TMPDIR_CLEANUP="$tmpdir"

  info "Downloading ${url}"
  curl -fsSL "$url" -o "${tmpdir}/${archive_name}" \
    || err "Download failed. Check that version '${version}' exists and has a release for ${platform}."

  info "Extracting archive"
  tar -xzf "${tmpdir}/${archive_name}" -C "$tmpdir"

  # The archive should contain the binary at the top level
  if [ ! -f "${tmpdir}/${BINARY_NAME}" ]; then
    # Try to find it nested
    local found
    found="$(find "$tmpdir" -name "$BINARY_NAME" -type f | head -1)"
    [ -n "$found" ] || err "Binary '${BINARY_NAME}' not found in archive"
    mv "$found" "${tmpdir}/${BINARY_NAME}"
  fi

  # macOS: remove quarantine flag so Gatekeeper doesn't block the binary
  if [ "$(uname -s)" = "Darwin" ]; then
    xattr -d com.apple.quarantine "${tmpdir}/${BINARY_NAME}" 2>/dev/null || true
  fi

  info "Installing to ${INSTALL_DIR}/${BINARY_NAME}"
  if [ -w "$INSTALL_DIR" ]; then
    mv "${tmpdir}/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"
  else
    sudo mv "${tmpdir}/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"
  fi
  chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

  ok "Installed ${BINARY_NAME} ${version} to ${INSTALL_DIR}/${BINARY_NAME}"

  # Check if INSTALL_DIR is in PATH
  case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
      echo ""
      info "${INSTALL_DIR} is not in your PATH. Add it with:"
      echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
      ;;
  esac

  # --- setup shell completions -----------------------------------------------

  info "Setting up shell completions"
  "${INSTALL_DIR}/${BINARY_NAME}" completions --install 2>/dev/null || true

  # --- welcome message -------------------------------------------------------

  echo ""
  echo ""
  printf '\033[1;36m'
  cat << 'BANNER'
  ____          _ _       _     _                         _
 / ___|_      _(_) |_ ___| |__ | |__   ___   __ _ _ __ __| |
 \___ \ \ /\ / / | __/ __| '_ \| '_ \ / _ \ / _` | '__/ _` |
  ___) \ V  V /| | || (__| | | | |_) | (_) | (_| | | | (_| |
 |____/ \_/\_/ |_|\__\___|_| |_|_.__/ \___/ \__,_|_|  \__,_|
BANNER
  printf '\033[0m'
  printf '\033[1m                        CLI %s\033[0m\n' "${version}"
  echo ""
  printf '\033[0;37m  Welcome to the Switchboard CLI! You can now interact with any\n'
  printf '  Switchboard instance straight from your terminal — manage drives,\n'
  printf '  documents, permissions, and more.\033[0m\n'
  echo ""
  printf '\033[1;34m  Get started:\033[0m\n'
  echo ""
  printf '  1. Connect to an instance:\n'
  echo ""
  printf '\033[1;33m     $ switchboard init\033[0m\n'
  echo ""
  printf '     You will be prompted for a GraphQL URL, for example:\n'
  printf '\033[0;37m     https://switchboard.powerhouse.xyz/graphql\033[0m\n'
  echo ""
  printf '  2. Explore your data:\n'
  echo ""
  printf '\033[1;33m     $ switchboard drives list\033[0m\n'
  printf '\033[1;33m     $ switchboard docs tree --drive <slug>\033[0m\n'
  printf '\033[1;33m     $ switchboard models list\033[0m\n'
  echo ""
  printf '  3. Launch interactive mode:\n'
  echo ""
  printf '\033[1;33m     $ switchboard -i\033[0m\n'
  echo ""
  printf '\033[0;37m  Run \033[1mswitchboard guide overview\033[0;37m for a full walkthrough.\033[0m\n'
  echo ""
}

install
