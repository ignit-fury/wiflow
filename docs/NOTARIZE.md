# Notarizing Wiflow (macOS)

Wiflow ships signed with an **ad-hoc** signature (`codesign --force --deep --sign -`,
run by `packaging/build-app.sh`). Ad-hoc signing is enough for local runs and for
the DMG to mount and launch on the build machine. macOS Gatekeeper may still show
a "downloaded from the Internet" warning on other machines; the user can bypass it
once via right-click → Open, or the app can be notarized as described below.

## Why ad-hoc suffices locally

- `codesign -s -` signs the bundle with a local identity — no certificate, no
  account, no cost. The binary is verified as unmodified since signing.
- The app has no privileged components (no installer packages, no kernel
  extensions, no hardened-runtime-only features), so ad-hoc meets the
  minimum bar for the app to run where it was built.
- Notarization is an Apple-server stamp of approval for distribution OUTSIDE
  the build machine. It is a distribution concern, not a local-run concern.

## Notarization prerequisites (paid)

Notarization requires a **paid Apple Developer account ($99/yr)**:

1. Enroll at <https://developer.apple.com> (individual or organization).
2. Create an app-specific password for your Apple ID at
   <https://appleid.apple.com> — used with `notarytool` instead of your
   real password.
3. (Recommended) Create a Developer ID Application certificate in
   Keychain Access, so the release binary is signed with a real identity
   before notarization. Ad-hoc-signed binaries are rejected by notarytool.

## Notarize + staple (command shapes)

Never put credentials in scripts, plists, or this repo. Pass them via
environment variables or the macOS keychain:

```sh
# 1. Sign the release binary with a Developer ID identity (see prerequisites).
codesign --force --deep --sign "Developer ID Application: Your Name (TEAMID)" \
  --options runtime --timestamp target/Wiflow.app

# 2. Submit for notarization (app-specific password via env, never hardcoded).
xcrun notarytool submit target/Wiflow-0.1.0-arm64.dmg \
  --apple-id "$APPLE_ID" \
  --password "$APP_PASSWORD" \
  --team-id "$TEAM_ID" \
  --wait

# 3. Staple the notarization ticket to the DMG so it verifies offline.
xcrun staple staple target/Wiflow-0.1.0-arm64.dmg
```

`$APPLE_ID` = your Apple ID email. `$APP_PASSWORD` = the app-specific password
from step 2. `$TEAM_ID` = the 10-character Team ID shown in your Apple
Developer account. All three are read from the environment — nothing is
written to disk.

## Team ID note for Info.plist

`packaging/Info.plist` intentionally omits `CFBundleTeamID`: ad-hoc signing has
no Team ID, and the key is only meaningful for Developer ID signatures. When
you move to paid distribution, add the Team ID to the notarytool invocation
above (as `--team-id`) rather than baking it into the plist.

## Status

v1 ships ad-hoc-signed only. Notarization is a manual, credential-gated step
for a future release — documented here so the path is known, not automated
here because this project holds no paid Apple Developer account.
