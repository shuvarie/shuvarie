#!/bin/sh
# install.sh - download and install the prebuilt Shuvarie binary.
#
# Supported platforms (matching the release workflow's tarballs):
#   Linux  - x86_64 / aarch64, glibc (default) or musl
#   macOS  - x86_64 / aarch64 (Apple Silicon)
#
# Install location:
#   root        -> /usr/local/bin
#   normal user -> ~/.local/bin
# Both can be overridden with --dir or SHUVARIE_INSTALL_DIR.
#
# Usage:
#   ./scripts/install.sh [options]
#   curl -fsSL https://shuvarie.org/install.sh | sh
#
# Options:
#   -h, --help          Show this help and exit
#   -v, --version VER   Install a specific version (e.g. 0.6.7 or v0.6.7)
#   -d, --dir DIR       Install the binary into DIR
#       --libc LIBC     Linux only: gnu (default) or musl (fully static)
#       --no-verify     Skip SHA-256 checksum verification
#
# Environment:
#   SHUVARIE_VERSION, SHUVARIE_INSTALL_DIR, SHUVARIE_LIBC (same as the flags),
#   GITHUB_TOKEN (optional, lifts the API rate limit when resolving "latest").

set -eu

REPO="shuvarie/shuvarie"
BIN="shuvarie"

VERSION="${SHUVARIE_VERSION:-}"
INSTALL_DIR="${SHUVARIE_INSTALL_DIR:-}"
LIBC="${SHUVARIE_LIBC:-}"
VERIFY=1

usage() {
    cat <<'EOF'
install.sh - download and install the prebuilt Shuvarie binary.

Supported platforms:
  Linux   x86_64 / aarch64, glibc (default) or musl (fully static)
  macOS   x86_64 / aarch64 (Apple Silicon)

Install location:
  root         /usr/local/bin
  normal user  ~/.local/bin

Options:
  -h, --help          Show this help and exit
  -v, --version VER   Install a specific version (e.g. 0.6.7 or v0.6.7)
  -d, --dir DIR       Install the binary into DIR
      --libc LIBC     Linux only: gnu (default) or musl
      --no-verify     Skip SHA-256 checksum verification

Environment:
  SHUVARIE_VERSION, SHUVARIE_INSTALL_DIR, SHUVARIE_LIBC
  GITHUB_TOKEN (optional, lifts the API rate limit on the version lookup)
EOF
}

info() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}
have() { command -v "$1" >/dev/null 2>&1; }

# download <url> <dest>: non-zero on failure, quiet on success.
download() {
    if have curl; then
        curl -fsSL --retry 3 --connect-timeout 15 -o "$2" "$1"
    elif have wget; then
        wget -q -O "$2" "$1"
    else
        die "curl or wget is required"
    fi
}

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------
while [ $# -gt 0 ]; do
    case "$1" in
        -h | --help)
            usage
            exit 0
            ;;
        -v | --version)
            [ $# -ge 2 ] || die "$1 needs a value"
            VERSION="$2"
            shift 2
            ;;
        -v=* | --version=*)
            VERSION="${1#*=}"
            shift
            ;;
        -d | --dir)
            [ $# -ge 2 ] || die "$1 needs a value"
            INSTALL_DIR="$2"
            shift 2
            ;;
        -d=* | --dir=*)
            INSTALL_DIR="${1#*=}"
            shift
            ;;
        --libc)
            [ $# -ge 2 ] || die "$1 needs a value"
            LIBC="$2"
            shift 2
            ;;
        --libc=*)
            LIBC="${1#*=}"
            shift
            ;;
        --no-verify)
            VERIFY=0
            shift
            ;;
        *)
            die "unknown argument: $1 (try --help)"
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------
OS="$(uname -s 2>/dev/null || echo unknown)"
MACHINE="$(uname -m 2>/dev/null || echo unknown)"

case "$OS" in
    Linux | Darwin) ;;
    *)
        die "unsupported operating system: $OS
       this script installs on Linux and macOS only; on Windows use the .zip
       or the NSIS installer from https://github.com/${REPO}/releases"
        ;;
esac

case "$MACHINE" in
    x86_64 | amd64 | x64) ARCH="x86_64" ;;
    arm64 | aarch64) ARCH="aarch64" ;;
    i386 | i486 | i586 | i686)
        die "unsupported architecture: $MACHINE (32-bit x86 builds are not published)"
        ;;
    *)
        die "unsupported architecture: $MACHINE"
        ;;
esac

if [ "$OS" = "Linux" ]; then
    # The glibc tarball is built against glibc 2.17, so it runs on anything
    # from RHEL 7 onwards; musl is offered for a fully static binary.
    if [ -z "$LIBC" ]; then
        LIBC="gnu"
    fi
    case "$LIBC" in
        gnu | glibc) LIBC="gnu" ;;
        musl) LIBC="musl" ;;
        *) die "unsupported --libc value: $LIBC (use gnu or musl)" ;;
    esac
    TARGET="${ARCH}-unknown-linux-${LIBC}"
else
    if [ -n "$LIBC" ] && [ "$LIBC" != "gnu" ]; then
        die "--libc only applies to Linux"
    fi
    LIBC=""
    TARGET="${ARCH}-apple-darwin"
fi

# ---------------------------------------------------------------------------
# Scratch space
# ---------------------------------------------------------------------------
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/shuvarie-install.XXXXXX")" ||
    die "could not create a temporary directory"
cleanup() { rm -rf "$WORKDIR"; }
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

# ---------------------------------------------------------------------------
# Release lookup
# ---------------------------------------------------------------------------
# api_get <url> <dest>: fetch a JSON document (kept apart from `download` so
# that a missing release is reported as such rather than as a network error).
api_get() {
    if have curl; then
        if [ -n "${GITHUB_TOKEN:-}" ]; then
            curl -fsSL --connect-timeout 15 \
                --header "Authorization: Bearer ${GITHUB_TOKEN}" -o "$2" "$1"
        else
            curl -fsSL --connect-timeout 15 -o "$2" "$1"
        fi
    elif have wget; then
        wget -q -O "$2" "$1"
    else
        die "curl or wget is required"
    fi
}

if [ -z "$VERSION" ]; then
    API="https://api.github.com/repos/${REPO}/releases/latest"
    TAG_JSON="${WORKDIR}/latest.json"
    api_get "$API" "$TAG_JSON" ||
        die "could not resolve the latest release from ${API}
       pass --version <ver> to skip the lookup"
    VERSION="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$TAG_JSON" | head -n 1)"
    [ -n "$VERSION" ] || die "could not parse the latest release tag (pass --version explicitly)"
fi

VERSION="${VERSION#v}"
case "$VERSION" in
    "" | *[!0-9A-Za-z.+~-]*) die "invalid version: $VERSION" ;;
esac

ARCHIVE="${BIN}-${VERSION}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/v${VERSION}"

# ---------------------------------------------------------------------------
# Download
# ---------------------------------------------------------------------------
info "shuvarie ${VERSION} for ${OS} ${ARCH}${LIBC:+ (${LIBC})}"
info "downloading ${ARCHIVE}"

TARBALL="${WORKDIR}/${ARCHIVE}"
download "${BASE_URL}/${ARCHIVE}" "$TARBALL" ||
    die "download failed: ${BASE_URL}/${ARCHIVE}
       (a 404 here usually means this platform/version has no published build)"

# ---------------------------------------------------------------------------
# Checksum
# ---------------------------------------------------------------------------
if [ "$VERIFY" -eq 1 ]; then
    SUMS="${WORKDIR}/SHA256SUMS"
    if download "${BASE_URL}/SHA256SUMS" "$SUMS" 2>/dev/null; then
        EXPECTED="$(awk -v name="$ARCHIVE" '
            { f = $NF; sub(/^\*/, "", f); if (f == name) { print $1; exit } }
        ' "$SUMS" | tr 'A-F' 'a-f')"
        if [ -z "$EXPECTED" ]; then
            warn "no checksum for ${ARCHIVE} in SHA256SUMS; skipping verification"
        else
            ACTUAL=""
            if have sha256sum; then
                ACTUAL="$(sha256sum "$TARBALL" | awk '{ print $1 }')"
            elif have shasum; then
                ACTUAL="$(shasum -a 256 "$TARBALL" | awk '{ print $1 }')"
            elif have openssl; then
                ACTUAL="$(openssl dgst -sha256 -r "$TARBALL" | awk '{ print $1 }')"
            else
                warn "no sha256sum/shasum/openssl found; skipping verification"
            fi
            if [ -n "$ACTUAL" ]; then
                [ "$ACTUAL" = "$EXPECTED" ] ||
                    die "checksum mismatch for ${ARCHIVE}
       expected: ${EXPECTED}
       actual:   ${ACTUAL}"
                info "checksum verified"
            fi
        fi
    else
        warn "SHA256SUMS not published for this release; skipping verification"
    fi
fi

# ---------------------------------------------------------------------------
# Extract
# ---------------------------------------------------------------------------
tar -xzf "$TARBALL" -C "$WORKDIR" || die "could not extract ${ARCHIVE}"

SOURCE="${WORKDIR}/${BIN}-${VERSION}/${BIN}"
if [ ! -f "$SOURCE" ]; then
    SOURCE="$(find "$WORKDIR" -type f -name "$BIN" | head -n 1)"
fi
[ -n "$SOURCE" ] && [ -f "$SOURCE" ] ||
    die "the archive did not contain a ${BIN} binary"

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------
if [ -z "$INSTALL_DIR" ]; then
    if [ "$(id -u)" -eq 0 ]; then
        INSTALL_DIR="/usr/local/bin"
    else
        [ -n "${HOME:-}" ] || die "HOME is not set; pass --dir <path>"
        INSTALL_DIR="${HOME}/.local/bin"
    fi
fi
case "$INSTALL_DIR" in
    /*) ;;
    *) INSTALL_DIR="$(pwd)/${INSTALL_DIR}" ;;
esac

mkdir -p "$INSTALL_DIR" || die "could not create ${INSTALL_DIR}"
DEST="${INSTALL_DIR}/${BIN}"

if have install; then
    install -m 0755 "$SOURCE" "$DEST" || die "could not install to ${DEST}"
else
    cp "$SOURCE" "$DEST" && chmod 0755 "$DEST" || die "could not install to ${DEST}"
fi

info "installed ${DEST}"

if "$DEST" --version >/dev/null 2>&1; then
    info "version: $("$DEST" --version 2>/dev/null | head -n 1)"
else
    warn "installed ${DEST}, but '${BIN} --version' did not run - check your system libraries"
fi

case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        # Spell $HOME back as a literal in the hint, so the suggested line
        # stays copy-pasteable no matter which shell the reader is in.
        PATH_ENTRY="$INSTALL_DIR"
        case "$PATH_ENTRY" in
            "${HOME:-/nonexistent}"/*) PATH_ENTRY="\$HOME${PATH_ENTRY#"${HOME:-}"}" ;;
        esac
        info ""
        warn "${INSTALL_DIR} is not in your PATH"
        info "       add this line to your shell profile (~/.profile, ~/.bashrc,"
        info "       ~/.zshrc, or ~/.config/fish/config.fish) and restart the shell:"
        info "           export PATH=\"${PATH_ENTRY}:\$PATH\""
        ;;
esac
