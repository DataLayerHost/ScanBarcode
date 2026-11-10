# ScanBarcode

ScanBarcode is a desktop barcode and QR-code scanner with a live camera preview, image loading, result history, clipboard/download actions, and configurable deeplink execution.

Read the [privacy policy](docs/privacy-policy.md) for details about camera access, local files, update checks, and external actions.

## Supported barcode formats

The decoder backend currently supports:

- QR Code
- Micro QR
- Data Matrix
- Aztec
- PDF417
- MaxiCode
- Codabar
- Code 39
- Code 93
- Code 128
- EAN-8
- EAN-13
- ITF
- UPC-A
- UPC-E
- RSS-14
- RSS Expanded
- Telepen
- UPC/EAN Extension

Multiple codes in one image are decoded into separate result lines.

## Scan actions

The latest scan gets an action button when its content is actionable:

- Wi-Fi QR payloads (`WIFI:`) request a Wi-Fi connection on Linux or open the system Wi-Fi settings on macOS and Windows.
- Wi-Fi Easy Connect (`DPP:`) is recognized as a Wi-Fi configuration action.
- vCards (`BEGIN:VCARD`) open a contact card for adding to the system contacts application.
- MECARD (`MECARD:`) and BIZCARD (`BIZCARD:`) are converted into contact cards.
- MATMSG and SMTP payloads open an email composer; SMSTO and MMSTO payloads open an SMS/MMS composer.
- EPC SEPA (`BCD`), Swiss QR-bill (`SPC`), and EMV TLV payloads are recognized as payment actions.
- iCalendar (`BEGIN:VCALENDAR`) and calendar events (`BEGIN:VEVENT`) open as calendar files.
- eSIM (`LPA:1$`), Matter (`MT:`), HomeKit (`X-HM://`), OpenPGP (`OPENPGP4FPR:`), and Blockchain UR (`UR:`) payloads are recognized and offered to the operating system.
- GS1 identifiers are recognized as product/inventory lookup data.
- Tone exchange-rate payloads (`BEGIN:RATES` … `END:RATES`) are recognized and counted for import.
- Tone application deeplinks such as `tone://rates/update`, `tone://token/...`, and `tone://app/...` remain available through the OS deeplink handler.
- CorePass deeplinks (`corepass:` and Tone user profiles) use the user action icon.
- BarcodeTx animated QR sequences are reconstructed with fountain decoding, SHA-256 validation, and transaction-batch parsing.
- URLs open in the default browser.
- Common deeplinks such as `mailto:`, `tel:`, `sms:`, `payto:`, `rss:`, and `feed:` are opened by the operating system.
- Magnet torrent links (`magnet:?xt=urn:btih:…`) are opened by the system's configured torrent application.
- Other URI schemes are also offered when the operating system has an application registered for them.

Generic deeplinks are not restricted to a built-in allowlist: any valid URI scheme, including custom schemes such as `my-custom-app://…`, remains available to an application registered by the operating system.

BarcodeTx frames can be read from the live camera. The progress bar below the results area shows the received animated sequence. Animated GIF uploads are decoded frame by frame as well, so a saved BarcodeTx animation can be loaded from an image file.

## Run from source

Install Rust, then run:

```sh
cargo run
```

The application opens as a native window. On macOS it requests camera permission the first time the camera is used.

## Build

```sh
cargo build --release
```

Release packages are produced automatically by GitHub Actions for macOS, Windows, and Linux when a non-prerelease GitHub Release is published. macOS has separate ARM64 and Intel ZIP downloads, each containing the native `ScanBarcode.app`; Windows ships a GUI `.exe` without a terminal window; Linux ships separate x86_64 (`ScanBarcode-VERSION-linux.tar.gz`) and ARM64 (`ScanBarcode-VERSION-linux-arm64.tar.gz`) desktop bundles, each containing a `.desktop` launcher and icon. Direct downloads use the same macOS ZIPs. See [macOS distribution details](docs/macos-app-store.md).

On Linux, the in-app updater chooses the archive for its CPU, verifies the release SHA-256 digest, and checks the extracted executable's ELF architecture before replacing the installed binary. The x86_64 archive keeps its existing filename so older installations continue to update. Snap Store builds receive updates through Snap instead.

Release signing uses the configured repository secrets and variables:

- macOS: `APPLE_CERTIFICATE_P12` is the base64 content of a Developer ID Application `.p12`; `APPLE_CERTIFICATE_PASSWORD` is its password. `MACOS_SIGNING_IDENTITY`, `APPLE_TEAM_ID`, and `APP_ID` are repository variables. The workflow uses `APPLE_API_KEY_P8` (PEM text or base64), `APPLE_API_KEY_ID`, and `APPLE_API_ISSUER_ID` for notarization. It signs each native app with the hardened runtime, staples the notarization ticket, and attaches both signed app ZIPs to the GitHub Release. Missing credentials fail the macOS release job.
- Windows: set `WINDOWS_CERTIFICATE_BASE64` (base64 of an exportable code-signing `.pfx`) and `WINDOWS_CERTIFICATE_PASSWORD`. The workflow signs and verifies the `.exe` before packaging. Without these secrets, the `.exe` remains unsigned. Certificates held in hardware or cloud signing services need a different signing integration.

Existing release assets must be rebuilt after configuring signing. Linux's tarball has no platform code-signing requirement.

### Homebrew

Create a public repository named `DataLayerHost/homebrew-tap` (on GitHub: **New repository**, owner **DataLayerHost**, name **homebrew-tap**). Generate a dedicated SSH key pair with `ssh-keygen -t ed25519 -f ~/.ssh/scanbarcode-homebrew-tap -C "ScanBarcode Homebrew publisher" -N ""`. Add `~/.ssh/scanbarcode-homebrew-tap.pub` to `homebrew-tap` under **Settings → Deploy keys → Add deploy key**, with **Allow write access** checked. Add the entire contents of the private key file (`~/.ssh/scanbarcode-homebrew-tap`) to ScanBarcode under **Settings → Secrets and variables → Actions** as `HOMEBREW_TAP_DEPLOY_KEY`. Keep the private key out of the repository and delete local copies after the secret is saved.

After the release workflow succeeds, `.github/workflows/publish-homebrew.yml` verifies the notarized macOS ZIPs and both Linux tarballs, calculates their SHA-256 hashes, and updates `Casks/scanbarcode.rb` in the tap. Homebrew installs `ScanBarcode.app` on Apple Silicon or Intel macOS and links the `scanbarcode` command on x86_64 or ARM64 Linux. The Linux build requires a graphical desktop and the runtime libraries used by the release binary. Install with the same command on either supported OS:

```sh
brew install --cask DataLayerHost/tap/scanbarcode
```

### App store publishing

For the first Mac App Store release, open `packaging/macos-xcode/ScanBarcode.xcodeproj`, build and archive the native Rust app in Xcode, then upload it manually through Organizer. The complete procedure and verification steps are in [macOS distribution details](docs/macos-app-store.md). This path does not require GitHub Actions.

After the release packaging workflow succeeds, `.github/workflows/publish-stores.yml` can submit store-specific builds. Store builds use the `store` Cargo feature, which disables the GitHub self-updater; users receive updates through their store. Tags use `MAJOR.MINOR.PATCH` without a prefix (for example, `0.1.3`) and must match `Cargo.toml`. Each store job is enabled separately by a repository variable, so unconfigured stores are skipped.

| Store | Repository variables | Repository secrets |
| --- | --- | --- |
| Mac App Store | `PUBLISH_MAC_APP_STORE=true`, `APPLE_TEAM_ID`, `APP_ID`, `APPLE_STORE_APP_IDENTITY`, `APPLE_STORE_INSTALLER_IDENTITY` | `APPLE_STORE_CERTIFICATE_BASE64`, `APPLE_STORE_CERTIFICATE_PASSWORD`, `APPLE_STORE_PROFILE_BASE64`, plus the existing `APPLE_API_KEY_P8`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID` |
| Microsoft Store | `PUBLISH_MICROSOFT_STORE=true`, `MS_PACKAGE_NAME`, `MS_PACKAGE_PUBLISHER`, `MS_PUBLISHER_DISPLAY_NAME`, `MS_PRODUCT_ID` | `MS_TENANT_ID`, `MS_SELLER_ID`, `MS_CLIENT_ID`, `MS_CLIENT_SECRET` |
| Snap Store | `PUBLISH_SNAP_STORE=true` | `SNAPCRAFT_STORE_CREDENTIALS` |

The Mac App Store certificate secret is a base64 `.p12` containing the Apple Distribution application identity and Mac Installer Distribution identity. The profile secret is a base64 Mac App Store Connect provisioning profile for `host.datalayer.scanbarcode`. The workflow builds a universal sandboxed `ScanBarcode.app`, creates a signed `.pkg` only as a temporary App Store upload container, deletes it after upload, and requests automatic release after App Review approval. The existing Developer ID certificate for GitHub downloads cannot be reused for this Store build.

For Microsoft Store, copy the package name and publisher identity exactly from Partner Center; the workflow builds a separate MSIX package and submits it with the [Microsoft Store Developer CLI](https://learn.microsoft.com/en-us/windows/apps/publish/msstore-dev-cli/overview). The first listing and first publication must be completed in Partner Center before CLI updates can run. The Microsoft Store re-signs submitted MSIX packages, so the Windows `.pfx` used for GitHub downloads is not required for this job.

For Snap Store, the package name is `dl-scanbarcode` and its displayed title is ScanBarcode. Wait until the Snap Store approves the name request, then create a `SNAPCRAFT_STORE_CREDENTIALS` secret scoped to `dl-scanbarcode` with `snapcraft export-login`. Set `PUBLISH_SNAP_STORE=true` to let the workflow build strictly confined amd64 and arm64 snaps on native Linux runners and release both to `stable` after Store review. Snap Store handles updates to installed snaps; the `store` Cargo feature disables the GitHub self-updater in these builds. The `camera` interface normally needs a user connection (`sudo snap connect dl-scanbarcode:camera`) unless the Store grants auto-connection. Test camera, image picker, Wi-Fi actions, and audio on installed store builds before enabling production publication.

## Repository layout

- `src/` — application source
- `assets/` — application artwork
- `packaging/` — platform metadata used by release builds
- `.github/` — issue templates and CI/release workflows

## License

ScanBarcode is released under the CORE License. See [LICENSE](LICENSE).
