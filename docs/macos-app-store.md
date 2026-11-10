# macOS app distribution

`ScanBarcode.app` is the installed application for every macOS channel. It keeps the existing Rust/egui application and uses `host.datalayer.scanbarcode` as its bundle ID. `packaging/macos/build-app.sh` assembles the app with its icon, version, and camera usage message from `packaging/macos/Info.plist`. Use a plain version tag such as `0.1.3`, matching the version in `Cargo.toml`.

| Channel | Download or upload | Signing |
| --- | --- | --- |
| GitHub Releases and direct downloads | `ScanBarcode-<tag>-macos-arm64.zip` or `ScanBarcode-<tag>-macos-x86_64.zip`, each containing `ScanBarcode.app` | Developer ID Application, hardened runtime, notarization, stapled ticket |
| Homebrew Cask | Architecture-matched release ZIP; Cask installs `ScanBarcode.app` | Same verified Developer ID app |
| Mac App Store | Universal `ScanBarcode.app`; temporary signed `.pkg` used only for App Store Connect upload | Apple Distribution app identity, embedded distribution profile, App Sandbox and camera entitlement; Mac Installer Distribution signs the upload container |

No macOS installer is attached to a GitHub Release or retained as a workflow artifact. The App Store job creates its `.pkg` in `$RUNNER_TEMP`, submits it, and removes it on success or failure. Users install the app through the Mac App Store, which handles the container.

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

## Mac App Store

Set `PUBLISH_MAC_APP_STORE=true` to enable the Mac App Store jobs in `.github/workflows/publish-stores.yml`. They build native ARM64 and Intel `store` feature binaries, combine them into one universal `ScanBarcode.app`, embed the Mac App Store distribution provisioning profile, sign with `packaging/macos/AppStore.entitlements`, and verify the profile and signed entitlements. The `store` feature disables the GitHub self-updater, so store updates come through Apple.

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

Apple's [command-line packaging guidance](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution) uses a signed installer package to submit a standalone Mac app built outside Xcode. Xcode can upload an archive directly, but this project has no Xcode app target or archive workflow. The temporary `.pkg` is therefore only the App Store Connect upload container; it is not a build target or a user-facing installer. The workflow uses fastlane `deliver` to upload that package and request review.

## Local checks

Run `plutil -lint packaging/macos/Info.plist packaging/macos/AppStore.entitlements`, `cargo fmt --all -- --check`, and `cargo test --all-targets`. The bundle assembler can be tested with `RUNNER_TEMP="$(mktemp -d)" bash packaging/macos/build-app.sh <macOS-binary> 0.1.3 <output>/ScanBarcode.app` on macOS. Actual certificate, profile, notarization, and App Store upload checks run in GitHub Actions because they require the configured secrets and Apple services.
