#!/usr/bin/env bash
#
# Bump every product's version string in one pass.
#
# Usage:
#     scripts/bump-version.sh 0.2.0
#
# Rewrites the version field in every manifest that carries one (seven
# Cargo.toml / pyproject.toml files, plus three JSON manifests — ten
# files total). Then you run `cargo build` yourself to refresh
# Cargo.lock. Idempotent: running twice with the same version is a
# no-op; running twice with different versions lands on the second.
#
# Used by:
#   - `release-pr.yml` GitHub Actions workflow as the version-bump
#     step before opening a Release PR.
#   - Humans locally to rehearse a bump without GitHub in the loop:
#
#         ./scripts/bump-version.sh 0.2.0
#         cargo build                 # refresh Cargo.lock
#         git diff                    # inspect before committing
#         git checkout .              # or back out if it looks wrong
#
# Portability: targets both BSD sed (macOS) and GNU sed (Linux) by
# writing through a temp file instead of using `-i` (which takes a
# mandatory argument on BSD but not GNU).

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $(basename "$0") X.Y.Z[-prerelease][+build]" >&2
    exit 1
fi

VERSION="$1"

# Validate semver — allows standard X.Y.Z plus optional -prerelease and
# +build metadata segments per semver.org. A release workflow won't
# normally use pre-releases for a public publish, but the format is
# handy for testing ("0.2.0-rc.1" against TestPyPI, etc.).
SEMVER_REGEX='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
if ! [[ "$VERSION" =~ $SEMVER_REGEX ]]; then
    echo "error: '$VERSION' is not a valid semver (X.Y.Z[-pre][+build])" >&2
    exit 1
fi

# Find the repo root — this script lives at <root>/scripts/, so one
# directory up from the script's own dir. Bash-specific
# `$BASH_SOURCE[0]` works regardless of how the script was invoked
# (relative path, symlink, `bash scripts/bump-version.sh`).
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# ---------------------------------------------------------------------------
# TOML files — match `^version = "..."` anchored at line start.
#
# This catches exactly the `[package]` / `[project]` version line in
# every file. Dependency versions in Cargo.toml are always indented
# inside tables (`[dependencies.foo]` or inline `foo = { version = ... }`),
# so the `^` anchor rules them out.
#
# One gotcha for the future: if we ever adopt workspace-unified
# versioning via `[workspace.package]`, that section also has a
# `^version = ...` line and the script would catch it. We don't use
# that pattern today; revisit if we do.

TOML_FILES=(
    "Cargo.toml"
    "sqlrite-ffi/Cargo.toml"
    "sdk/python/Cargo.toml"
    "sdk/python/pyproject.toml"
    "sdk/nodejs/Cargo.toml"
    "sdk/wasm/Cargo.toml"
    "desktop/src-tauri/Cargo.toml"
)

for file in "${TOML_FILES[@]}"; do
    if [[ ! -f "$file" ]]; then
        echo "error: $file not found (are you in the repo root?)" >&2
        exit 1
    fi
    sed "s/^version = \"[^\"]*\"/version = \"${VERSION}\"/" "$file" > "$file.tmp"
    mv "$file.tmp" "$file"
done

# ---------------------------------------------------------------------------
# JSON files — match `  "version": "..."` with exactly two leading spaces.
#
# All three of our JSON manifests use 2-space indentation and put
# `"version"` as a top-level object key. Dependency version pins in
# `package.json` use the package *name* as the key (e.g.,
# `"rustyline": "^18.0.0"`), never the literal string `"version"`, so
# there's no ambiguity.
#
# We use `sed -E` (extended regex) rather than jq to avoid adding a
# dependency on a tool that isn't on every CI runner by default.

JSON_FILES=(
    "sdk/nodejs/package.json"
    "desktop/package.json"
    "desktop/src-tauri/tauri.conf.json"
)

for file in "${JSON_FILES[@]}"; do
    if [[ ! -f "$file" ]]; then
        echo "error: $file not found (are you in the repo root?)" >&2
        exit 1
    fi
    sed -E "s/^(  \"version\"): *\"[^\"]*\"/\\1: \"${VERSION}\"/" "$file" > "$file.tmp"
    mv "$file.tmp" "$file"
done

# ---------------------------------------------------------------------------
# Verify every file actually updated. Catches future refactors that
# change manifest shape (e.g., someone reformats package.json to
# 4-space indent — our 2-space-anchored regex would silently
# no-op; this loop catches that immediately).

echo
echo "Bumped to ${VERSION}. Verifying…"
FAILURES=0

for file in "${TOML_FILES[@]}"; do
    expected="version = \"${VERSION}\""
    actual="$(grep -E '^version = ' "$file" | head -1)"
    if [[ "$actual" != "$expected" ]]; then
        echo "  ✗ $file — expected: $expected  got: $actual" >&2
        FAILURES=$((FAILURES + 1))
    else
        echo "  ✓ $file"
    fi
done

for file in "${JSON_FILES[@]}"; do
    # grep catches the line; we verify the version substring matches.
    # Trailing comma / closing brace handled by not matching beyond the
    # version value.
    if grep -qE "^  \"version\": \"${VERSION}\"" "$file"; then
        echo "  ✓ $file"
    else
        actual="$(grep -E '^  "version": ' "$file" | head -1)"
        echo "  ✗ $file — expected version \"${VERSION}\", got: $actual" >&2
        FAILURES=$((FAILURES + 1))
    fi
done

if [[ $FAILURES -gt 0 ]]; then
    echo
    echo "error: $FAILURES file(s) did not update as expected." >&2
    echo "Run 'git diff' to inspect, 'git checkout .' to back out." >&2
    exit 1
fi

echo
echo "Done. Next steps:"
echo "  cargo build    # refresh Cargo.lock with the new versions"
echo "  git diff       # inspect the ten-file bump"
echo "  git checkout . # or back out if it looks wrong"
