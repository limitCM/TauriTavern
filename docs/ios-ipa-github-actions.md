# GitHub Actions iOS IPA Build

This repository includes a manual workflow for building an iOS IPA on a
GitHub-hosted macOS runner:

```text
.github/workflows/ios-ipa.yml
```

## Manual Run

1. Open the fork on GitHub.
2. Go to **Actions**.
3. Select **iOS IPA Build**.
4. Click **Run workflow**.
5. Keep the defaults for a first test:
   - `runner`: `macos-15`
   - `export_method`: `debugging`
   - `build_type`: `release`
6. Download the generated IPA from the workflow run artifacts.

## Signing Secrets

Tauri iOS builds still need Apple signing. Use either automatic signing through
App Store Connect API credentials or manual signing assets.

For automatic signing, add these repository secrets:

```text
APPLE_API_ISSUER
APPLE_API_KEY
APPLE_API_KEY_P8
```

`APPLE_API_KEY_P8` is the full text content of the downloaded
`AuthKey_<key id>.p8` file. The workflow writes it to a temporary file and
exports `APPLE_API_KEY_PATH` for the Tauri CLI.

For manual signing, add these repository secrets instead:

```text
IOS_CERTIFICATE
IOS_CERTIFICATE_PASSWORD
IOS_MOBILE_PROVISION
```

`IOS_CERTIFICATE` and `IOS_MOBILE_PROVISION` should be base64-encoded before
being stored as GitHub Secrets. When these three secrets are present, the
workflow imports the certificate into a temporary keychain and installs the
provisioning profile for the build.

## Export Method

Use `debugging` for the first side-load or development build. Use
`app-store-connect` when preparing a TestFlight/App Store style IPA. Use
`release-testing` when the provisioning profile includes the target test
device UDIDs.

The project already sets the bundle identifier and Apple development team in:

```text
src-tauri/tauri.conf.json
src-tauri/gen/apple/project.yml
```

If the fork uses a different Apple developer account or bundle identifier,
update those files before running the workflow.

## Common Failure Points

- The local checkout still points at `Darkatse/TauriTavern`; push these workflow
  files to `limitCM/TauriTavern` before running Actions on the fork.
- Missing or mismatched Apple signing credentials usually fail at the Xcode
  archive/export step.
- A `debugging` or `release-testing` IPA only installs on devices covered by
  the matching provisioning profile.
- `app-store-connect` builds require a valid App Store Connect app record and
  matching bundle identifier.
