#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$SRCROOT"
while [ ! -f "$REPO_ROOT/Cargo.toml" ]; do
	PARENT="$(dirname "$REPO_ROOT")"
	if [ "$PARENT" = "$REPO_ROOT" ]; then
		echo 'Could not locate the ScanBarcode Cargo.toml above the Xcode project' >&2
		exit 1
	fi
	REPO_ROOT="$PARENT"
done
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
export RUNNER_TEMP="${DERIVED_FILE_DIR:?Xcode did not provide DERIVED_FILE_DIR}"
mkdir -p "$RUNNER_TEMP"
cd "$REPO_ROOT"

if [ "${PLATFORM_NAME:-}" != macosx ]; then
	echo 'ScanBarcode supports the macOS Xcode destination only' >&2
	exit 1
fi

case "${CONFIGURATION:-}" in
	Debug)
		CARGO_PROFILE=debug
		;;
	Release)
		CARGO_PROFILE=release
		# Xcode owns this release build's symbols; keep other release channels unchanged.
		export CARGO_PROFILE_RELEASE_DEBUG=2
		export CARGO_PROFILE_RELEASE_STRIP=none
		;;
	*)
		echo "Unsupported Xcode configuration: ${CONFIGURATION:-unset}" >&2
		exit 1
		;;
esac

ARCH_LIST="${ARCHS:-${CURRENT_ARCH:-${NATIVE_ARCH_ACTUAL:-}}}"
if [ -z "$ARCH_LIST" ]; then
	echo 'Xcode did not provide a build architecture' >&2
	exit 1
fi

BINARIES=()
for arch in $ARCH_LIST; do
	case "$arch" in
		arm64) rust_target=aarch64-apple-darwin ;;
		x86_64) rust_target=x86_64-apple-darwin ;;
		*) echo "Unsupported macOS architecture: $arch" >&2; exit 1 ;;
	esac
	if ! rustup target list --installed | grep -Fxq "$rust_target"; then
		echo "Install the Rust target with: rustup target add $rust_target" >&2
		exit 1
	fi
	if [ "$CARGO_PROFILE" = release ]; then
		cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" --features store --target "$rust_target"
	else
		cargo build --manifest-path "$REPO_ROOT/Cargo.toml" --features store --target "$rust_target"
	fi
	BINARIES+=("$REPO_ROOT/target/$rust_target/$CARGO_PROFILE/ScanBarcode")
done

if [ "${#BINARIES[@]}" -eq 1 ]; then
	BINARY="${BINARIES[0]}"
else
	BINARY="$DERIVED_FILE_DIR/ScanBarcode-universal"
	lipo -create "${BINARIES[@]}" -output "$BINARY"
fi

APP="${TARGET_BUILD_DIR:?Xcode did not provide TARGET_BUILD_DIR}/${FULL_PRODUCT_NAME:?Xcode did not provide FULL_PRODUCT_NAME}"
bash "$REPO_ROOT/packaging/macos/build-app.sh" "$BINARY" "$MARKETING_VERSION" "$APP" "$CURRENT_PROJECT_VERSION"

for arch in $ARCH_LIST; do
	lipo -verify_arch "$arch" "$APP/Contents/MacOS/ScanBarcode"
done
test "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist")" = ScanBarcode
bash "$REPO_ROOT/packaging/macos/check-store-apis.sh" "$APP/Contents/MacOS/ScanBarcode"

if [ "$CARGO_PROFILE" = release ]; then
	EXECUTABLE="$APP/Contents/MacOS/ScanBarcode"
	DSYM="${DWARF_DSYM_FOLDER_PATH:?Xcode did not provide DWARF_DSYM_FOLDER_PATH}/${DWARF_DSYM_FILE_NAME:?Xcode did not provide DWARF_DSYM_FILE_NAME}"
	rm -rf "$DSYM"
	mkdir -p "$(dirname "$DSYM")"
	xcrun dsymutil "$EXECUTABLE" -o "$DSYM"
	DWARF_FILE="$DSYM/Contents/Resources/DWARF/ScanBarcode"
	test -s "$DWARF_FILE"
	for arch in $ARCH_LIST; do
		executable_uuid="$(xcrun dwarfdump --uuid "$EXECUTABLE" | awk -v arch="$arch" '$3 == "(" arch ")" { print $2 }')"
		dsym_uuid="$(xcrun dwarfdump --uuid "$DWARF_FILE" | awk -v arch="$arch" '$3 == "(" arch ")" { print $2 }')"
		if [ -z "$executable_uuid" ] || [ "$executable_uuid" != "$dsym_uuid" ]; then
			echo "Missing or mismatched $arch dSYM UUID for ScanBarcode.app" >&2
			exit 1
		fi
		echo "Verified $arch dSYM UUID: $dsym_uuid"
	done
fi
