# ScanBarcode

ScanBarcode is a desktop barcode and QR-code scanner with a live camera preview, image loading, result history, clipboard/download actions, and configurable deeplink execution.

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

Release packages are produced automatically by GitHub Actions for macOS, Windows, and Linux when a non-prerelease GitHub Release is published. The macOS package is a Finder-openable `.app`; Windows ships a GUI `.exe` without a terminal window; Linux ships a desktop bundle containing a `.desktop` launcher and icon.

## Repository layout

- `src/` — application source
- `assets/` — application artwork
- `packaging/` — platform metadata used by release builds
- `.github/` — issue templates and CI/release workflows

## License

ScanBarcode is released under the CORE License. See [LICENSE](LICENSE).
