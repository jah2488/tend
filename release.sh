#!/usr/bin/env bash
#
# Cut a new tend release. Automates the whole dance so no step gets skipped:
#
#   ./release.sh patch      # 0.2.5 -> 0.2.6   (default)
#   ./release.sh minor      # 0.2.5 -> 0.3.0
#   ./release.sh major      # 0.2.5 -> 1.0.0
#   ./release.sh 0.4.1      # explicit version
#
# What it does, in order:
#   1. Preflight: required tools, on main, clean tree, gh auth, tag not taken.
#   2. Computes the new version from the current Cargo.toml.
#   3. Gate: cargo test + cargo clippy must pass (nothing is changed before this).
#   4. Bumps Cargo.toml (+ Cargo.lock), builds the release binary, writes a checksum.
#   5. Commits the bump and creates an annotated tag locally.
#   6. Asks for confirmation, then pushes main + tag and creates the GitHub release
#      with the binary, its .sha256, and notes generated from the commit log.
#
# Local steps (1-5) are reversible. The script pauses before anything is pushed.
# Set RELEASE_YES=1 to skip the prompt (CI). Read it before trusting it.

set -euo pipefail

err()  { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }
info() { printf '\033[36m==>\033[0m %s\n' "$*"; }
need() { command -v "$1" >/dev/null 2>&1 || err "required command not found: $1"; }

# --- 1. Preflight -----------------------------------------------------------
need git; need cargo; need rustc; need gh
command -v shasum >/dev/null 2>&1 || command -v sha256sum >/dev/null 2>&1 \
  || err "need 'shasum' or 'sha256sum'"

cd "$(git rev-parse --show-toplevel)" || err "not inside a git repository"

[ "$(git rev-parse --abbrev-ref HEAD)" = "main" ] \
  || err "releases are cut from main; you are on $(git rev-parse --abbrev-ref HEAD)"
git diff-index --quiet HEAD -- \
  || err "working tree is dirty; commit or stash your changes first"
gh auth status >/dev/null 2>&1 || err "gh is not authenticated (run: gh auth login)"

REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner)" \
  || err "could not resolve the GitHub repo"

# --- 2. Compute the new version ---------------------------------------------
current="$(awk -F'"' '/^version = "/{print $2; exit}' Cargo.toml)"
[ -n "$current" ] || err "could not read version from Cargo.toml"
IFS='.' read -r MA MI PA <<<"$current"

bump="${1:-patch}"
case "$bump" in
  major) version="$((MA + 1)).0.0" ;;
  minor) version="${MA}.$((MI + 1)).0" ;;
  patch) version="${MA}.${MI}.$((PA + 1))" ;;
  [0-9]*.[0-9]*.[0-9]*) version="$bump" ;;
  *) err "usage: release.sh [patch|minor|major|X.Y.Z]" ;;
esac
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || err "invalid version: $version"

tag="v${version}"
git rev-parse -q --verify "refs/tags/${tag}" >/dev/null && err "tag ${tag} already exists"
git ls-remote --exit-code --tags origin "$tag" >/dev/null 2>&1 \
  && err "tag ${tag} already exists on origin"

info "Releasing ${REPO}: ${current} -> ${version} (${tag})"

# --- 3. Gate: tests + lint (before touching anything) -----------------------
info "Running tests"
cargo test --quiet
info "Running clippy"
cargo clippy --quiet -- -D warnings

# --- 4. Bump, build, checksum -----------------------------------------------
info "Bumping Cargo.toml to ${version}"
# Replace only the first `version = "..."` line (the [package] version).
awk -v v="$version" '
  !done && /^version = "/ { sub(/"[^"]*"/, "\"" v "\""); done = 1 }
  { print }
' Cargo.toml > Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml

info "Building release binary"
cargo build --release --quiet          # also refreshes Cargo.lock with the new version

triple="$(rustc -vV | sed -n 's/host: //p')"
asset="tend-${triple}"
out="$(mktemp -d)"; trap 'rm -rf "$out"' EXIT
cp "target/release/tend" "${out}/${asset}"
if command -v shasum >/dev/null 2>&1; then
  ( cd "$out" && shasum -a 256 "$asset" > "${asset}.sha256" )
else
  ( cd "$out" && sha256sum "$asset" > "${asset}.sha256" )
fi
info "Built ${asset} ($(du -h "${out}/${asset}" | cut -f1)), checksum: $(awk '{print $1}' "${out}/${asset}.sha256")"

# --- 5. Commit + tag (local) ------------------------------------------------
git add Cargo.toml Cargo.lock
git commit -q -m "Bump to ${version}"
git tag -a "$tag" -m "tend ${tag}"
info "Committed bump and tagged ${tag} locally"

# Release notes: commit subjects since the previous tag, plus the install line.
prev_tag="$(git describe --tags --abbrev=0 "${tag}^" 2>/dev/null || true)"
range="${prev_tag:+${prev_tag}..}${tag}"
changes="$(git log --no-merges --pretty='- %s' "$range" 2>/dev/null | grep -v '^- Bump to ' || true)"
notes="$(printf '## Changes\n\n%s\n\nInstall: `curl -fsSL https://raw.githubusercontent.com/%s/main/install.sh | bash`\n' \
  "${changes:-- (no notable changes)}" "$REPO")"

# --- 6. Confirm, then push + publish (irreversible) -------------------------
printf '\n%s\n\n' "$notes"
if [ "${RELEASE_YES:-}" != "1" ]; then
  printf 'Push main + %s and publish the GitHub release? [y/N] ' "$tag"
  read -r reply </dev/tty
  case "$reply" in
    y | Y | yes) ;;
    *) err "aborted before pushing. Undo local steps with:
     git tag -d ${tag} && git reset --hard HEAD~1" ;;
  esac
fi

info "Pushing main and ${tag}"
git push origin main
git push origin "$tag"

info "Creating GitHub release ${tag}"
gh release create "$tag" "${out}/${asset}" "${out}/${asset}.sha256" \
  --title "$tag" --notes "$notes"

info "Released: $(gh release view "$tag" --json url -q .url)"
