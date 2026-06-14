# GitHub Actions iOS IPA Build

This repository includes a workflow for building an unsigned iOS IPA on a
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
   - `build_type`: `release`
6. Download the generated IPA from the workflow run artifacts.

The workflow also runs automatically for `codex/**` branches when iOS build
inputs change.

Internally the workflow still invokes `tauri ios build` so Tauri can prepare
its iOS Xcode build context, then it packages the resulting unsigned `.app`
bundle into an IPA artifact. The macOS runner temporarily disables Xcode code
signing for this artifact build only.

## Local Sideload Signing

The artifact is intentionally unsigned. iOS still requires every app to be
signed before it can run on a physical iPhone, but that signing can happen on
your own machine through a sideloading tool instead of inside GitHub Actions.

Use the downloaded unsigned IPA with a local re-signing installer such as
Sideloadly, AltStore, SideStore, or another tool that can sign an IPA using
your local Apple ID or device-specific signing setup.

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
- The IPA downloaded from Actions is not directly installable until a local
  sideloading tool signs it.
- Free Apple ID sideloading usually has app count and refresh limits enforced
  by Apple's services.
- If the local signer changes the bundle identifier, make sure it stays
  consistent with the app data you expect to reuse.
