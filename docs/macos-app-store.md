# macOS app distribution

`ScanBarcode.app` is the installed application for every macOS channel. It keeps the existing Rust/egui application and uses `host.datalayer.scanbarcode` as its bundle ID. `packaging/macos/build-app.sh` assembles the app with its icon, version, and camera usage message from `packaging/macos/Info.plist`. Use a plain version tag such as `0.1.3`, matching the version in `Cargo.toml`.

| Channel | Download or upload | Signing |
| --- | --- | --- |
| GitHub Releases and direct downloads | `ScanBarcode-<tag>-macos-arm64.zip` or `ScanBarcode-<tag>-macos-x86_64.zip`, each containing `ScanBarcode.app` | Developer ID Application, hardened runtime, notarization, stapled ticket |
| Homebrew Cask | Architecture-matched release ZIP; Cask installs `ScanBarcode.app` | Same verified Developer ID app |
| Mac App Store | Universal `ScanBarcode.app` archived in Xcode and uploaded through Organizer | Xcode re-signs with the Mac App Distribution identity and App Store profile during distribution |

No macOS installer is attached to a GitHub Release or retained as a workflow artifact. Xcode may create a temporary signed package as its App Store Connect upload container; users install `ScanBarcode.app` through the Mac App Store.

## GitHub Releases

Publishing a non-prerelease GitHub Release runs `.github/workflows/release.yml`. Native ARM64 and Intel jobs build `ScanBarcode.app`, sign it with Developer ID, verify the signature and team ID, submit a ZIP to Apple's notarization service, staple and validate the ticket, and then attach the final ZIP. The in-app updater selects the ZIP matching the current Mac architecture. The Homebrew workflow verifies both archives before updating the tap.

Repository configuration:

| Name | Kind | Purpose |
| --- | --- | --- |
| `APPLE_CERTIFICATE_P12` | Secret | Base64-encoded Developer ID Application certificate and private key in `.p12` form |
| `APPLE_CERTIFICATE_PASSWORD` | Secret | Password for that `.p12` |
| `MACOS_SIGNING_IDENTITY` | Variable | Developer ID Application identity name |
| `APPLE_TEAM_ID` | Variable | `XNG2XKLCQG` |
| `APP_ID` | Variable | `host.datalayer.scanbarcode` |
| `APPLE_API_KEY_P8` | Secret | App Store Connect API private key, PEM or base64 |
| `APPLE_API_KEY_ID` | Secret | API key ID |
| `APPLE_API_ISSUER_ID` | Secret | API issuer ID |

## First Mac App Store release with Xcode Organizer

The checked-in project is `packaging/macos-xcode/ScanBarcode.xcodeproj`. It requires Xcode 27.2 or later for its JSON project format. The `ScanBarcode` target is macOS-only. Its build phase compiles the existing Rust/egui executable with Cargo's `store` feature, selects the architecture requested by Xcode, and places the real Rust executable at `ScanBarcode.app/Contents/MacOS/ScanBarcode` **before Xcode signs the app**. No SwiftUI launcher is included. Debug builds compile for the active Mac; Release archives compile ARM64 and Intel and combine them into a universal app. Cargo handles incremental compilation, while Xcode runs the build phase each time so Rust source edits are not missed.

The bundle ID is `host.datalayer.scanbarcode`; the team is `XNG2XKLCQG`. `packaging/macos/Info.plist` supplies the app name, camera usage text, icon name, copyright `(CORE) DataLayer`, version `0.1.3`, category, and macOS 12 minimum. Xcode's **Current Project Version** supplies build number `1`. `packaging/macos/AppStore.entitlements` enables App Sandbox, camera, and read/write access to files users choose in the Open or Save dialogs. The store build disables the GitHub self-updater. The icon comes from `assets/scanbarcode-icon.png` through the shared bundle assembler.

### Prepare Xcode

1. Install Xcode 27.2 or later, open it once to accept the license, and select it with `xcode-select` if needed.
2. Install Rust and both macOS targets: `rustup target add aarch64-apple-darwin x86_64-apple-darwin`.
3. Add your Apple Developer account to **Xcode → Settings → Accounts** and confirm that team **XNG2XKLCQG** can manage signing. Install the **Mac App Distribution** certificate and its private key in Keychain. Xcode can create or download the matching Mac App Store distribution profile when distributing the archive; use an explicit App ID for `host.datalayer.scanbarcode`.
4. Open `packaging/macos-xcode/ScanBarcode.xcodeproj` and select the **ScanBarcode** scheme. The target's **Signing & Capabilities** page should show **Automatically manage signing**, team **XNG2XKLCQG**, and the App Sandbox capabilities. Set the macOS run destination to **My Mac**.

### Build, run, archive, and upload

1. Choose **Product → Build**. Xcode invokes Cargo, then signs `ScanBarcode.app`. Choose **Product → Run** to launch the native Rust app; test live camera scanning and **Load image…**. The run build may be development-signed.
2. Increase **Current Project Version** in the target's Build Settings for each new App Store upload. Keep **Marketing Version**, `Cargo.toml`, and the release version in `packaging/macos/Info.plist` aligned. For the first upload these are marketing version `0.1.3` and build number `1`.
3. Select **Any Mac** as the destination and choose **Product → Archive**. The Xcode build enables full DWARF information for the Rust release executable, generates a universal `ScanBarcode.app.dSYM` from the final executable before signing, and checks both architecture UUIDs. Open **Window → Organizer → Archives** and select the ScanBarcode archive. Confirm its `dSYMs` directory contains `ScanBarcode.app.dSYM` before uploading.
4. Click **Distribute App → App Store Connect → Upload**. Leave automatic signing enabled so Xcode uses the Mac App Distribution identity and a matching Mac App Store profile, then review Xcode's validation and upload screens. This is the first-release upload path; GitHub Actions are not required.
5. Wait for App Store Connect to process the build, then select the uploaded build for version **0.1.3** and complete the listing, compliance, and App Review fields. If an earlier build was already uploaded without symbols, increase **Current Project Version**, create a fresh archive, and upload that build; its executable UUIDs will differ from the older archive.

### Validate the archive

An archive created by Xcode may initially carry an Apple Development signature. **Organizer re-signs it for App Store distribution.** Verify the archive's bundle before upload, then use Organizer's validation screen to confirm distribution signing and provisioning. For a local export, Xcode's `DistributionSummary.plist` records the Mac App Distribution certificate, provisioning profile, architectures, and app version. Inspect the exported app or Xcode's temporary upload package before treating it as distributable.

```sh
APP="/path/to/ScanBarcode.xcarchive/Products/Applications/ScanBarcode.app"
DSYM="/path/to/ScanBarcode.xcarchive/dSYMs/ScanBarcode.app.dSYM"
plutil -lint "$APP/Contents/Info.plist"
lipo -archs "$APP/Contents/MacOS/ScanBarcode" # x86_64 arm64
xcrun dwarfdump --uuid "$APP/Contents/MacOS/ScanBarcode"
xcrun dwarfdump --uuid "$DSYM/Contents/Resources/DWARF/ScanBarcode"
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -d --entitlements :- "$APP" 2>/dev/null
```

The two `dwarfdump` commands must print identical UUIDs for both `x86_64` and `arm64`. Check that `CFBundleExecutable` is `ScanBarcode`, the bundle ID is `host.datalayer.scanbarcode`, the copyright is `(CORE) DataLayer`, and the camera usage description is present. A final App Store export must show the **Mac App Distribution** identity, team `XNG2XKLCQG`, a valid App Store profile for the bundle ID, App Sandbox, camera, and user-selected file entitlements. The signed upload container is temporary and must not be distributed to users.

## Optional GitHub Actions Mac App Store publication

For later automated releases, set `PUBLISH_MAC_APP_STORE=true` to enable the Mac App Store jobs in `.github/workflows/publish-stores.yml`. They build native ARM64 and Intel `store` feature binaries, combine them into one universal `ScanBarcode.app`, embed the Mac App Store distribution provisioning profile, sign with `packaging/macos/AppStore.entitlements`, and verify the profile and signed entitlements. The `store` feature disables the GitHub self-updater, so store updates come through Apple.

Additional repository variables:

| Name | Value |
| --- | --- |
| `APPLE_STORE_APP_IDENTITY` | Apple Distribution application identity name |
| `APPLE_STORE_INSTALLER_IDENTITY` | Mac Installer Distribution identity name |

Additional repository secrets:

| Name | Value |
| --- | --- |
| `APPLE_STORE_CERTIFICATE_BASE64` | Base64 `.p12` containing the Apple Distribution app identity and the Mac Installer Distribution identity with their private keys |
| `APPLE_STORE_CERTIFICATE_PASSWORD` | Password for the store `.p12` |
| `APPLE_STORE_PROFILE_BASE64` | Base64 Mac App Store distribution provisioning profile for `host.datalayer.scanbarcode` and team `XNG2XKLCQG` |

The existing `APPLE_API_KEY_P8`, `APPLE_API_KEY_ID`, and `APPLE_API_ISSUER_ID` are reused for App Store Connect submission. That API key needs access to upload builds and submit the app for review. The profile is checked for the team, app ID, expiration, and signing certificate before signing. The workflow also checks the app's signature, team ID, sandbox entitlement, camera entitlement, and upload container signature. The profile and credentials are written only to the ephemeral runner and are removed after the job. GitHub Actions must never enable shell tracing for these steps.

Apple's [command-line packaging guidance](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution) uses a signed installer package to submit a standalone Mac app built outside Xcode. The optional workflow uses fastlane `deliver` with a temporary `.pkg` upload container in `$RUNNER_TEMP`; it is never a build target or a user-facing installer. The first release can instead go through Xcode Organizer as described above.

## Local checks

Run `plutil -lint packaging/macos/Info.plist packaging/macos/AppStore.entitlements`, `cargo fmt --all -- --check`, and `cargo test --all-targets`. To inspect the Xcode target from the command line without relying on a fixed project directory depth, use `PROJECT="$(find packaging -name '*.xcodeproj' -type d -print -quit)"; xcodebuild -list -project "$PROJECT"`. The bundle assembler can be tested with `RUNNER_TEMP="$(mktemp -d)" bash packaging/macos/build-app.sh <macOS-binary> 0.1.3 <output>/ScanBarcode.app` on macOS. Local signed builds and App Store exports can verify certificates and profiles when Xcode has access to the team account; notarization and the optional automated App Store upload use GitHub Actions credentials.
