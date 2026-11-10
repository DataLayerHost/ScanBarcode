#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
	echo 'Usage: build-app.sh BINARY VERSION OUTPUT_APP [BUILD_NUMBER]' >&2
	exit 2
fi

BINARY="$1"
VERSION="$2"
APP="$3"
BUILD_NUMBER="${4:-$VERSION}"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
	echo 'Version must have the form MAJOR.MINOR.PATCH' >&2
	exit 1
fi
if [[ ! "$BUILD_NUMBER" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
	echo 'Build number must contain one to three dot-separated numbers' >&2
	exit 1
fi
PACKAGE_VERSION="$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "ScanBarcode") | .version')"
if [ "$VERSION" != "$PACKAGE_VERSION" ]; then
	echo "Release version $VERSION does not match Cargo.toml version $PACKAGE_VERSION" >&2
	exit 1
fi
if [ ! -f "$BINARY" ]; then
	echo "Missing macOS binary: $BINARY" >&2
	exit 1
fi

mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/ScanBarcode"
chmod +x "$APP/Contents/MacOS/ScanBarcode"
cp assets/scanbarcode-icon.png "$APP/Contents/Resources/ScanBarcode.png"
ICONSET="$(mktemp -d "$RUNNER_TEMP/scanbarcode-icon.XXXXXX")/ScanBarcode.iconset"
mkdir -p "$ICONSET"
trap 'rm -rf "${ICONSET%/ScanBarcode.iconset}"' EXIT
for size in 16 32 128 256 512; do
	sips -z "$size" "$size" assets/scanbarcode-icon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
	retina=$((size * 2))
	sips -z "$retina" "$retina" assets/scanbarcode-icon.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/ScanBarcode.icns"
cp packaging/macos/Info.plist "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VERSION" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $BUILD_NUMBER" "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist")" = 'host.datalayer.scanbarcode'
test "$(/usr/libexec/PlistBuddy -c 'Print :NSCameraUsageDescription' "$APP/Contents/Info.plist")" = 'ScanBarcode uses your camera to scan barcodes and QR codes.'
test "$(/usr/libexec/PlistBuddy -c 'Print :NSHumanReadableCopyright' "$APP/Contents/Info.plist")" = '(CORE) DataLayer'
