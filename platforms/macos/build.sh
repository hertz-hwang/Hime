#!/bin/bash
# Build the Hime macOS input method (SPEC_MACOS §1, §3.6).
#
# 1. cargo build --release -p hime-ffi          -> target/release/libhime_ffi.a
# 2. swiftc sources + hime.h bridge header      -> Hime.app/Contents/MacOS/Hime
# 3. assemble bundle (Info.plist + dictionary + icons + localized names), codesign
# 4. optional: --install copies to ~/Library/Input Methods/ and (re)launches
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/../.." && pwd)"

APP_NAME="Hime"
BUILD_DIR="${SCRIPT_DIR}/build"
APP_BUNDLE="${BUILD_DIR}/${APP_NAME}.app"
CONTENTS="${APP_BUNDLE}/Contents"
BRIDGE_HEADER="${ROOT_DIR}/crates/hime-ffi/hime.h"
# The default input scheme: 琉璃v1 (data/xingma_sample.tsv is a test fixture only).
DICTIONARY="${ROOT_DIR}/data/schemes/liuli_v1/liuli_table.txt"

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
cp "${DICTIONARY}" "${CONTENTS}/Resources/liuli_table.txt"
printf 'APPL????' > "${CONTENTS}/PkgInfo"

echo "==> Rendering icons from assets/*.svg"
ICON_BUILD_DIR="${BUILD_DIR}/icons"
xcrun swift "${SCRIPT_DIR}/tools/make-icons.swift" "${SCRIPT_DIR}/assets" "${ICON_BUILD_DIR}"
iconutil -c icns "${ICON_BUILD_DIR}/AppIcon.iconset" -o "${ICON_BUILD_DIR}/AppIcon.icns"
cp "${ICON_BUILD_DIR}/AppIcon.icns" "${CONTENTS}/Resources/AppIcon.icns"
cp "${ICON_BUILD_DIR}/HimeMenuIcon.pdf" "${CONTENTS}/Resources/HimeMenuIcon.pdf"

echo "==> Copying localized display names"
cp -R "${SCRIPT_DIR}/resources/en.lproj" "${SCRIPT_DIR}/resources/zh-Hans.lproj" "${CONTENTS}/Resources/"

echo "==> Ad-hoc codesign"
codesign --force --sign - "${APP_BUNDLE}"

echo "==> Built ${APP_BUNDLE}"

if [[ "${1:-}" == "--install" ]]; then
    INSTALL_DIR="${HOME}/Library/Input Methods"
    INSTALL_BUNDLE="${INSTALL_DIR}/${APP_NAME}.app"
    mkdir -p "${INSTALL_DIR}"
    # Stop a running instance first so it releases the IMK connection.
    pkill -x "${APP_NAME}" 2>/dev/null || true
    rm -rf "${INSTALL_BUNDLE}"
    cp -R "${APP_BUNDLE}" "${INSTALL_DIR}/"
    codesign --force --sign - "${INSTALL_BUNDLE}"

    # Refresh LaunchServices so System Settings drops its cached bundle name and
    # icon instead of showing the previous build's labels.
    LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister"
    if [[ -x "${LSREGISTER}" ]]; then
        "${LSREGISTER}" -f "${INSTALL_BUNDLE}"
    fi

    echo "==> Registering and enabling the input source via TIS"
    xcrun swift - "${INSTALL_BUNDLE}" <<'SWIFT_EOF'
import Carbon
import Foundation

let bundlePath = CommandLine.arguments[1]
let bundleURL = URL(fileURLWithPath: bundlePath) as CFURL

let regStatus = TISRegisterInputSource(bundleURL)
print("TISRegisterInputSource(\(bundlePath)): \(regStatus == noErr ? "ok" : "failed, OSStatus \(regStatus)")")
if regStatus != noErr { exit(1) }

// The mode id is what System Settings lists; the app-level id may also
// appear. Enable whichever of the two TIS actually exposes.
let targets: Set<String> = [
    "com.hime.inputmethod.Hime",
    "com.hime.inputmethod.Hime.Hans",
]

guard let listRef = TISCreateInputSourceList(nil, true)?.takeRetainedValue() as? [TISInputSource]
else {
    print("TISCreateInputSourceList returned no list")
    exit(1)
}

func inputSourceID(_ source: TISInputSource) -> String? {
    guard let ptr = TISGetInputSourceProperty(source, kTISPropertyInputSourceID) else { return nil }
    return Unmanaged<CFString>.fromOpaque(ptr).takeUnretainedValue() as String
}

var enabled: Set<String> = []
var failed: Set<String> = []
for source in listRef {
    guard let id = inputSourceID(source), targets.contains(id) else { continue }
    let status = TISEnableInputSource(source)
    if status == noErr {
        print("TISEnableInputSource(\(id)): enabled")
        enabled.insert(id)
    } else {
        print("TISEnableInputSource(\(id)): failed, OSStatus \(status)")
        failed.insert(id)
    }
}
for id in targets.subtracting(enabled).subtracting(failed) {
    print("input source not registered in TIS list: \(id)")
}
if enabled.isEmpty { exit(1) }
SWIFT_EOF

    killall Hime 2>/dev/null || true
    # The input menu keeps its own cache of the mode list, so replacing the bundle
    # in place leaves the old label and icon there; the agent relaunches on demand.
    killall TextInputMenuAgent 2>/dev/null || true
    echo "==> Installed to ${INSTALL_DIR}"
fi
