#!/usr/bin/env bash
set -euo pipefail

binary="${1:?Pass the macOS executable to inspect}"
symbols="$(nm -u "$binary")"
if grep -Eq '(^|[[:space:]])_CGS(SetWindowBackgroundBlurRadius|MainConnectionID)([^[:alnum:]_]|$)' <<< "$symbols" ||
	strings -a "$binary" | grep -E 'CGS(SetWindowBackgroundBlurRadius|MainConnectionID)' >/dev/null; then
	echo 'Mac App Store executable references a private CoreGraphics window blur API' >&2
	exit 1
fi
echo 'Mac App Store private blur API check passed'
