#!/usr/bin/env bash
# Bump the workspace version and roll the CHANGELOG for a release.
#
# Usage: scripts/cut-release.sh <patch|minor|major|X.Y.Z>
#
# Mutates Cargo.toml (workspace version) and CHANGELOG.md (a new dated section
# under [Unreleased], plus compare links). Prints the new version to stdout.
# Does NOT touch git — the caller commits/tags. Safe to run locally and inspect
# the diff (then `git checkout -- Cargo.toml CHANGELOG.md` to revert). Every
# edit asserts it matched, so a drifted CHANGELOG fails loudly instead of
# producing a malformed release.
set -euo pipefail

arg="${1:?usage: cut-release.sh <patch|minor|major|X.Y.Z>}"
repo_url="https://github.com/cstuncsik/kommand0"

cur=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')

case "$arg" in
  patch | minor | major)
    IFS=. read -r ma mi pa <<<"$cur"
    case "$arg" in
      major) new="$((ma + 1)).0.0" ;;
      minor) new="${ma}.$((mi + 1)).0" ;;
      patch) new="${ma}.${mi}.$((pa + 1))" ;;
    esac
    ;;
  *) new="$arg" ;;
esac

# Strict semver-ish, no leading zeros.
if ! [[ "$new" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "invalid version: '$new' (expected X.Y.Z)" >&2
  exit 1
fi

# Refuse a non-increasing version (catches re-issuing or downgrading).
if [ "$new" = "$cur" ] || [ "$(printf '%s\n%s\n' "$cur" "$new" | sort -V | tail -1)" != "$new" ]; then
  echo "new version '$new' must be greater than current '$cur'" >&2
  exit 1
fi

today=$(date +%F)

# Each substitution must change exactly what we expect; abort if it matched
# nothing (e.g. a hand-edited CHANGELOG) so we never tag a malformed release.
sub() { # <file> <perl-expr> <description>
  perl -0pi -e "BEGIN{\$n=0} \$n += $2; END{exit 1 unless \$n}" "$1" \
    || { echo "cut-release: $3 — pattern not found in $1; aborting" >&2; exit 1; }
}

# 1) Workspace version (the only line-anchored `version = "..."` in root Cargo.toml).
sub Cargo.toml "s/^version = \"\\Q$cur\\E\"/version = \"$new\"/m" "version bump"

# 2) CHANGELOG: open a new dated section directly under [Unreleased].
sub CHANGELOG.md "s/## \\[Unreleased\\]\\n/## [Unreleased]\\n\\n## [$new] - $today\\n/" "changelog section"

# 3) CHANGELOG compare links: point [Unreleased] at the new tag and add a
#    prev...new link for the cut version.
sub CHANGELOG.md \
  "s{^\\[Unreleased\\]: \\Q$repo_url\\E/compare/v\\Q$cur\\E\\.\\.\\.HEAD\$}{[Unreleased]: $repo_url/compare/v$new...HEAD\\n[$new]: $repo_url/compare/v$cur...v$new}m" \
  "changelog compare links"

echo "$new"
