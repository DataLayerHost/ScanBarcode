# ScanBarcode patch to winit 0.30.13

This directory is the crates.io source for `winit` 0.30.13 with one macOS change:
`WindowDelegate::set_blur` no longer calls the private
`CGSSetWindowBackgroundBlurRadius` and `CGSMainConnectionID` APIs. Their unused
FFI declarations were removed as well.

ScanBarcode never enables window blur. The patch retains the compatible stable
`winit` API used by `eframe` 0.37 while allowing the Mac App Store binary to pass
Apple's private API check. When the application can move to a stable `winit`
release with a public blur implementation, remove this patch and verify the
final universal archive with `packaging/macos/check-store-apis.sh`.
