#!/bin/sh
# kommand0 installer — downloads the prebuilt binaries for your platform from the
# latest GitHub release, verifies the checksum, and installs `kommand0` + `kmd`.
#
#   curl -fsSL https://github.com/cstuncsik/kommand0/releases/latest/download/install.sh | sh
#
# Environment overrides:
#   KOMMAND0_VERSION      tag to install (e.g. v0.1.5); default: the latest release
#   KOMMAND0_INSTALL_DIR  install directory; default: /usr/local/bin (sudo if needed)
set -eu

REPO="https://github.com/cstuncsik/kommand0"
API="https://api.github.com/repos/cstuncsik/kommand0"
INSTALL_DIR="${KOMMAND0_INSTALL_DIR:-/usr/local/bin}"

info() { echo "kommand0-install: $*"; }
err() {
	echo "kommand0-install: error: $*" >&2
	exit 1
}

# --- downloader (curl or wget) ----------------------------------------------
if command -v curl >/dev/null 2>&1; then
	fetch() { curl -fsSL "$1" -o "$2"; }
	fetch_stdout() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
	fetch() { wget -qO "$2" "$1"; }
	fetch_stdout() { wget -qO- "$1"; }
else
	err "need curl or wget on PATH"
fi

# --- checksum verifier (sha256sum or shasum) --------------------------------
if command -v sha256sum >/dev/null 2>&1; then
	verify() { sha256sum -c "$1.sha256" >/dev/null; }
elif command -v shasum >/dev/null 2>&1; then
	verify() { shasum -a 256 -c "$1.sha256" >/dev/null; }
else
	err "need sha256sum or shasum to verify the download"
fi

# --- resolve version --------------------------------------------------------
if [ -n "${KOMMAND0_VERSION:-}" ]; then
	version="$KOMMAND0_VERSION"
else
	# Parse tag_name from the releases API (no jq dependency).
	version=$(fetch_stdout "$API/releases/latest" |
		grep '"tag_name"' | head -n1 |
		sed 's/.*"tag_name"[^"]*"\([^"]*\)".*/\1/')
	[ -n "$version" ] || err "could not resolve the latest version (set KOMMAND0_VERSION)"
fi

# --- detect platform --------------------------------------------------------
os=$(uname -s)
arch=$(uname -m)
case "$os" in
Darwin) platform="macos-universal" ;; # universal binary covers arm64 + x86_64
Linux)
	case "$arch" in
	x86_64 | amd64) platform="linux-x86_64" ;;
	*) err "no prebuilt Linux binary for $arch — build from source: $REPO" ;;
	esac
	;;
*) err "unsupported OS '$os' — build from source: $REPO" ;;
esac

# --- download + verify ------------------------------------------------------
archive="kommand0-${version}-${platform}.tar.gz"
base="$REPO/releases/download/${version}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

info "downloading $archive ..."
fetch "$base/$archive" "$tmp/$archive" || err "download failed: $base/$archive"
fetch "$base/$archive.sha256" "$tmp/$archive.sha256" || err "checksum download failed"

info "verifying checksum ..."
(cd "$tmp" && verify "$archive") || err "checksum verification FAILED — refusing to install"

# --- extract + install ------------------------------------------------------
tar -xzf "$tmp/$archive" -C "$tmp" || err "extracting $archive failed"

# Decide whether the install dir needs sudo (create it if missing + writable).
sudo_prefix=""
if ! mkdir -p "$INSTALL_DIR" 2>/dev/null || [ ! -w "$INSTALL_DIR" ]; then
	command -v sudo >/dev/null 2>&1 ||
		err "cannot write to $INSTALL_DIR and sudo is unavailable (set KOMMAND0_INSTALL_DIR to a writable dir)"
	info "installing to $INSTALL_DIR (using sudo) ..."
	sudo_prefix="sudo"
fi
# shellcheck disable=SC2086 # sudo_prefix is intentionally an unquoted command prefix
$sudo_prefix mkdir -p "$INSTALL_DIR"
for bin in kommand0 kmd; do
	[ -f "$tmp/$bin" ] || err "archive is missing '$bin'"
	chmod +x "$tmp/$bin"
	# shellcheck disable=SC2086
	$sudo_prefix mv "$tmp/$bin" "$INSTALL_DIR/$bin"
done

info "installed kommand0 + kmd ($version) to $INSTALL_DIR"
case ":$PATH:" in
*":$INSTALL_DIR:"*) ;;
*) info "note: $INSTALL_DIR is not on your PATH — add it to use 'kommand0' and 'kmd'" ;;
esac
