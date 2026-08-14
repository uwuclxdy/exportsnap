#!/usr/bin/env bash
# install.sh — install a released Rust binary. Prefers `cargo` when present;
# otherwise downloads the platform asset from GitHub Releases and verifies it
# against the release's sha256sums.txt BEFORE installing. Fails closed on any
# download or checksum error. Writes nothing to your shell profile.
#
# Pairs with .github/workflows/release.yml — the OS/arch → asset-name mapping below
# MUST match the asset names that workflow publishes.
set -Eeuo pipefail

REPO="uwuclxdy/exportsnap"  # owner/name, e.g. you/yourtool
BINARY="exportsnap"         # binary name AND crates.io crate name (edit the cargo line if they differ)
NOCARGO=0

# Guard against running the template unmodified.
{ [[ "$REPO" != *__*__* ]] && [[ "$BINARY" != *__*__* ]]; } \
    || { echo "ERROR: edit __GH_REPO__ and __BIN_NAME__ before running install.sh" >&2; exit 1; }

for arg in "$@"; do
    case "${arg}" in
        --nocargo) NOCARGO=1 ;;
        *) echo "Unknown argument: ${arg}" >&2; exit 1 ;;
    esac
done

# Prefer cargo when available (unless --nocargo).
if [[ "${NOCARGO}" -eq 0 ]] && command -v cargo &>/dev/null; then
    echo "cargo detected — installing via cargo..."
    cargo install "${BINARY}"
    echo ""
    echo "To uninstall, run: cargo uninstall ${BINARY}"
    exit 0
fi

# Detect OS/arch → release asset name (keep in sync with release.yml).
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
EXT=""

case "${OS}" in
    linux)
        case "${ARCH}" in
            x86_64) ASSET="${BINARY}-linux-x86_64" ;;
            *) echo "Unsupported architecture: ${ARCH}" >&2; exit 1 ;;
        esac
        ;;
    darwin)
        case "${ARCH}" in
            x86_64)        echo "No prebuilt for Intel macOS (this release publishes aarch64 only) — install via cargo: cargo install ${BINARY}" >&2; exit 1 ;;
            arm64|aarch64) ASSET="${BINARY}-macos-aarch64" ;;
            *)             echo "Unsupported architecture: ${ARCH}" >&2; exit 1 ;;
        esac
        ;;
    *mingw*|*msys*|*cygwin*)
        ASSET="${BINARY}-windows-x86_64.exe"
        EXT=".exe"
        OS="windows"
        ;;
    *)
        echo "Unsupported OS: ${OS} — install via cargo: cargo install ${BINARY}" >&2
        exit 1
        ;;
esac

URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
SUMS_URL="https://github.com/${REPO}/releases/latest/download/sha256sums.txt"

TMP=$(mktemp)
TMP_SUMS=$(mktemp)
trap 'rm -f "${TMP}" "${TMP_SUMS}"' EXIT

# dl <url> <out> — curl or wget, fail closed.
dl() {
    if command -v curl &>/dev/null; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget &>/dev/null; then
        wget -q "$1" -O "$2"
    else
        echo "Error: curl or wget is required" >&2; exit 1
    fi
}

echo "Downloading ${ASSET}..."
dl "${URL}" "${TMP}"

# Verify integrity against sha256sums.txt from the same release. A download or
# checksum failure aborts — no partial / unverified install.
echo "Verifying checksum..."
dl "${SUMS_URL}" "${TMP_SUMS}" \
    || { echo "Error: failed to download sha256sums.txt — aborting install" >&2; exit 1; }

if command -v sha256sum &>/dev/null; then
    ACTUAL_HEX=$(sha256sum "${TMP}" | awk '{print $1}')
elif command -v shasum &>/dev/null; then
    ACTUAL_HEX=$(shasum -a 256 "${TMP}" | awk '{print $1}')
else
    echo "Error: sha256sum or shasum is required for integrity verification" >&2; exit 1
fi

# Lines are "<64-hex>  <asset-name>".
EXPECTED_HEX=$(grep -E "^[0-9a-f]{64}  ${ASSET}$" "${TMP_SUMS}" | awk '{print $1}')
[[ -n "${EXPECTED_HEX}" ]] \
    || { echo "Error: ${ASSET} not found in sha256sums.txt — aborting install" >&2; exit 1; }

if [[ "${ACTUAL_HEX}" != "${EXPECTED_HEX}" ]]; then
    echo "Error: checksum mismatch for ${ASSET}" >&2
    printf '  expected: %s\n  got:      %s\n' "${EXPECTED_HEX}" "${ACTUAL_HEX}" >&2
    exit 1
fi
echo "Checksum verified."
chmod +x "${TMP}"

# Install dir: /usr/local/bin when writable, else ~/.local/bin.
if [[ "${OS}" != "windows" && -w /usr/local/bin ]]; then
    INSTALL_DIR="/usr/local/bin"
else
    INSTALL_DIR="${HOME}/.local/bin"
fi

mkdir -p "${INSTALL_DIR}"
mv "${TMP}" "${INSTALL_DIR}/${BINARY}${EXT}"
echo "Installed to ${INSTALL_DIR}/${BINARY}${EXT}"

if ! printf '%s' "${PATH}" | grep -q "${INSTALL_DIR}"; then
    echo ""
    echo "Note: ${INSTALL_DIR} is not in your PATH. Add this to your shell profile:"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
fi

echo ""
echo "To uninstall, run: rm ${INSTALL_DIR}/${BINARY}${EXT}"
