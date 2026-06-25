#!/usr/bin/env bash
#
# Installer for `tend` — downloads the latest release binary into ~/.local/bin.
#
#   curl -fsSL https://raw.githubusercontent.com/jah2488/tend/main/install.sh | bash
#
# What it does, in order:
#   1. Detects your OS/CPU and maps it to a release asset name.
#   2. Downloads that binary and its .sha256 checksum over HTTPS from GitHub.
#   3. Verifies the checksum and refuses to install on any mismatch.
#   4. Installs to ~/.local/bin, marks it executable, clears macOS quarantine.
#
# It writes only to ~/.local/bin (override with TEND_INSTALL_DIR) and a private
# temp dir. It needs no root and runs nothing it downloads. Read it in full
# before piping to a shell — that's good practice for any curl|bash installer.

set -euo pipefail

REPO="jah2488/tend"
BIN="tend"
INSTALL_DIR="${TEND_INSTALL_DIR:-$HOME/.local/bin}"
BASE_URL="https://github.com/${REPO}/releases/latest/download"

err() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
info() { printf '\033[36m==>\033[0m %s\n' "$*"; }

# Fail early with a clear message if a required tool is missing.
need() { command -v "$1" >/dev/null 2>&1 || err "required command not found: $1"; }
need curl
need uname
need mktemp

# Resolve this machine's Rust target triple (matches the release asset names).
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin) os_part="apple-darwin" ;;
  Linux)  os_part="unknown-linux-gnu" ;;
  *) err "unsupported OS: $os — build from source instead (see the README)" ;;
esac
case "$arch" in
  arm64 | aarch64) arch_part="aarch64" ;;
  x86_64 | amd64)  arch_part="x86_64" ;;
  *) err "unsupported CPU: $arch — build from source instead (see the README)" ;;
esac
asset="${BIN}-${arch_part}-${os_part}"

# Pick a SHA-256 tool: shasum on macOS, sha256sum on most Linux distros.
if command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
elif command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$1" | awk '{print $1}'; }
else
  err "need 'shasum' or 'sha256sum' to verify the download"
fi

# Work in a private temp dir that is always cleaned up, even on failure.
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

info "Downloading ${asset} from the latest release"
if ! curl -fsSL "${BASE_URL}/${asset}" -o "${tmp}/${asset}"; then
  err "no release asset for your platform (${asset}).
     See https://github.com/${REPO}/releases for what is published, or build from source."
fi
curl -fsSL "${BASE_URL}/${asset}.sha256" -o "${tmp}/${asset}.sha256" \
  || err "could not download the checksum for ${asset}; aborting for safety"

# Verify integrity: compare the downloaded binary's hash to the published one.
expected="$(awk '{print $1}' "${tmp}/${asset}.sha256")"
actual="$(sha256 "${tmp}/${asset}")"
[ -n "$expected" ] || err "published checksum is empty; aborting"
if [ "$expected" != "$actual" ]; then
  err "checksum mismatch — refusing to install.
     expected: $expected
     actual:   $actual"
fi
info "Checksum verified"

# Install atomically: chmod and de-quarantine in the temp dir, then move into place.
chmod 755 "${tmp}/${asset}"
if [ "$os" = "Darwin" ]; then
  xattr -d com.apple.quarantine "${tmp}/${asset}" >/dev/null 2>&1 || true
fi
mkdir -p "$INSTALL_DIR"
mv -f "${tmp}/${asset}" "${INSTALL_DIR}/${BIN}"

# macOS can SIGKILL a freshly-replaced binary whose ad-hoc signature no longer
# matches the one it approved at this path (com.apple.provenance), bricking an
# in-place update. Clear tracking xattrs and re-sign ad-hoc so updates run clean.
# Best-effort: a machine without codesign keeps the linker signature, which is
# fine for a first install to a fresh path.
if [ "$os" = "Darwin" ]; then
  xattr -c "${INSTALL_DIR}/${BIN}" >/dev/null 2>&1 || true
  codesign --force --sign - "${INSTALL_DIR}/${BIN}" >/dev/null 2>&1 || true
fi

info "Installed ${INSTALL_DIR}/${BIN}"
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*)
    info "Run: ${BIN}" ;;
  *)
    info "Add ${INSTALL_DIR} to your PATH, then run ${BIN}:"
    printf '      echo '\''export PATH="%s:$PATH"'\'' >> ~/.zshrc\n' "$INSTALL_DIR" ;;
esac
