#!/bin/bash
# Build the Hime macOS input method (SPEC_MACOS §1, §3.6).
#
# 1. cargo build --release -p hime-ffi          -> target/release/libhime_ffi.a
# 2. swiftc sources + hime.h bridge header      -> Hime.app/Contents/MacOS/Hime
# 3. assemble bundle (Info.plist + dictionary), ad-hoc codesign
# 4. optional: --install copies to ~/Library/Input Methods/ and (re)launches
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

APP_NAME="Hime"
BUILD_DIR="${SCRIPT_DIR}/build"
APP_BUNDLE="${BUILD_DIR}/${APP_NAME}.app"
CONTENTS="${APP_BUNDLE}/Contents"
BRIDGE_HEADER="${ROOT_DIR}/crates/hime-ffi/hime.h"
DICTIONARY="${ROOT_DIR}/data/xingma_sample.tsv"

echo "==> cargo build --release -p hime-ffi"
cargo build --release -p hime-ffi --manifest-path "${ROOT_DIR}/Cargo.toml"
# The workspace .cargo/config.toml may redirect the target dir; ask cargo.
TARGET_DIR="$(cargo metadata --no-deps --format-version 1 \
    --manifest-path "${ROOT_DIR}/Cargo.toml" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
RUST_LIB="${TARGET_DIR}/release/libhime_ffi.a"
test -f "${RUST_LIB}" || { echo "error: ${RUST_LIB} not found"; exit 1; }

echo "==> Assembling ${APP_BUNDLE}"
rm -rf "${APP_BUNDLE}"
mkdir -p "${CONTENTS}/MacOS" "${CONTENTS}/Resources"

echo "==> swiftc compile + link"
swiftc \
    -swift-version 5 \
    -O \
    -module-name "${APP_NAME}" \
    -import-objc-header "${BRIDGE_HEADER}" \
    -o "${CONTENTS}/MacOS/${APP_NAME}" \
    "${SCRIPT_DIR}/main.swift" \
    "${SCRIPT_DIR}/HimeInputController.swift" \
    -framework Cocoa \
    -framework InputMethodKit \
    "${RUST_LIB}"

cp "${SCRIPT_DIR}/Info.plist" "${CONTENTS}/Info.plist"
cp "${DICTIONARY}" "${CONTENTS}/Resources/xingma_sample.tsv"
printf 'APPL????' > "${CONTENTS}/PkgInfo"

echo "==> Ad-hoc codesign"
codesign --force --sign - "${APP_BUNDLE}"

echo "==> Built ${APP_BUNDLE}"

if [[ "${1:-}" == "--install" ]]; then
    INSTALL_DIR="${HOME}/Library/Input Methods"
    mkdir -p "${INSTALL_DIR}"
    pkill -x "${APP_NAME}" 2>/dev/null || true
    rm -rf "${INSTALL_DIR}/${APP_NAME}.app"
    cp -R "${APP_BUNDLE}" "${INSTALL_DIR}/"
    open "${INSTALL_DIR}/${APP_NAME}.app"
    echo "==> Installed to ${INSTALL_DIR} and launched"
fi
