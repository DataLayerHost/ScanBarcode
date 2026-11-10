#!/usr/bin/env python3
"""Check that an embedded Mac App Store profile matches the app being signed."""

import datetime
import plistlib
import sys


def main() -> None:
	if len(sys.argv) != 5:
		raise SystemExit("Usage: verify-profile.py PROFILE_PLIST TEAM_ID BUNDLE_ID CERTIFICATE_DER")
	with open(sys.argv[1], "rb") as profile_file:
		profile = plistlib.load(profile_file)
	team_id, bundle_id = sys.argv[2:4]
	if team_id not in profile.get("TeamIdentifier", []):
		raise SystemExit("Provisioning profile has the wrong team ID")
	entitlements = profile.get("Entitlements", {})
	identifier = entitlements.get("com.apple.application-identifier") or entitlements.get(
		"application-identifier"
	)
	if identifier != f"{team_id}.{bundle_id}":
		raise SystemExit("Provisioning profile has the wrong application identifier")
	with open(sys.argv[4], "rb") as certificate_file:
		certificate = certificate_file.read()
	if certificate not in profile.get("DeveloperCertificates", []):
		raise SystemExit("Signing certificate is not included in the provisioning profile")
	if profile.get("ProvisionsAllDevices") or profile.get("ProvisionedDevices"):
		raise SystemExit("Expected an App Store distribution profile")
	expires = profile.get("ExpirationDate")
	if expires is None:
		raise SystemExit("Provisioning profile has no expiration date")
	if expires.replace(tzinfo=datetime.timezone.utc) <= datetime.datetime.now(
		datetime.timezone.utc
	):
		raise SystemExit("Provisioning profile has expired")
	print("Provisioning profile team, app ID, certificate, and expiration verified")


if __name__ == "__main__":
	main()
