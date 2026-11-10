# ScanBarcode privacy policy

**Effective date:** November 10, 2026
**Publisher:** DataLayer

ScanBarcode is a desktop app for scanning barcodes and QR codes with a camera or an image you choose. This policy describes how the app handles information on macOS, Windows, and Linux, including builds distributed through app stores and direct downloads.

## Camera, images, and scan results

ScanBarcode uses the camera only to show a live preview and decode codes on your device. You can turn the camera off, and your operating system controls camera permission. Images you choose through the file picker are read and decoded on your device. The app does not upload camera frames, selected images, or decoded scan results to DataLayer.

Scan results remain in the app's memory while it is open. The app does not automatically save scan history. You can clear results in the app. When you close the app, unsaved results are discarded.

If you choose **Save results**, ScanBarcode writes a text file to the location you select. If you choose **Copy results**, it puts the results on your system clipboard. Those copies remain under your control and may persist after you close the app; delete the file or clear the clipboard through your operating system when you no longer want them.

## Settings and temporary files

ScanBarcode saves local preferences, including camera selection, camera and preview settings, sound settings, and the automatic update setting. It does not store scanned content in those preferences. You can remove the app's locally stored preferences through your operating system.

If you choose an action that opens a scanned contact or calendar event, the app may create a temporary `.vcf` or `.ics` file on your device and pass it to the relevant system app. The operating system or that app may retain the file or imported information. You can remove temporary files and imported items using your operating system or the receiving app.

## Network access and other apps

Direct-download builds can automatically request release information from GitHub to check for updates. You can turn automatic checks off in the app. If you choose to download an update, the app downloads the release from GitHub to a temporary location and checks its SHA-256 digest. Store builds do not use this in-app updater; updates come through the store. GitHub may receive ordinary connection information, such as your IP address, when the app contacts its services. See [GitHub's privacy statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).

ScanBarcode does not automatically open scanned links or send scanned content to a remote scanning service. If you choose an action for a scanned URL, email address, payment link, Wi-Fi configuration, contact, calendar event, or other recognized content, ScanBarcode passes the relevant content to your browser, operating system, or another app. That destination's privacy practices then apply. Check a code's content before opening it.

## Data collection and sharing

ScanBarcode does not require an account and has no in-app analytics, advertising, or tracking. DataLayer does not collect scan content through the app. The app does not sell personal data.

If you voluntarily send a bug report or feature request through GitHub, the information you include is handled by GitHub and is visible according to the repository's issue settings. Please do not include private scan contents in a public issue. App stores and operating systems may separately process installation, update, purchase, or crash information under their own policies.

## Retention and your choices

DataLayer does not retain camera images, selected images, or scan results on a ScanBarcode server because the app does not send them there. You can stop camera access in the app or revoke its permission in your operating system's settings. You can clear scan results, delete files you saved, clear clipboard contents, remove temporary files, and uninstall the app and its local preferences. For information you chose to share with an external app, service, or store, use that provider's privacy controls.

## Changes and contact

We may update this policy if the app's data practices change. The effective date above will reflect the latest version. For privacy questions, contact DataLayer through the [ScanBarcode GitHub issue tracker](https://github.com/DataLayerHost/ScanBarcode/issues/new/choose). Avoid posting sensitive information in a public issue.
