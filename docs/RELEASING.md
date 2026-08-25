# Releasing

Cutting a release is pushing a tag. `.github/workflows/release.yml` does the rest.

## Steps

1. Bump the version in `package.json`, `src-tauri/Cargo.toml` and
   `src-tauri/tauri.conf.json`. All three must agree — `node scripts/check-version.mjs`
   checks this, and CI refuses to build on a mismatch.
2. Commit.
3. Tag it: `git tag v0.3.1`
4. Push the tag: `git push origin v0.3.1`
5. The workflow builds, signs, and opens a **draft** release with the installer, its
   `.sig` and `latest.json`. Publish the draft when it looks right.

Installed copies pick the update up from there. Until the draft is published, GitHub
does not treat it as the latest release, so the updater endpoint keeps returning the
previous version — which is the intended behaviour.

## What the workflow does

Windows x86_64 only: the app is Windows-only and the bundled FFmpeg sidecar is a
Windows binary. Adding another target means adding a sidecar for it first.

```
checkout → node → rust → cache → npm ci
  → check version matches the tag
  → npm run setup:ffmpeg      (fetches the pinned FFmpeg, verifies SHA-256)
  → verify the sidecar is in place
  → npm run build → npm test → cargo test
  → tauri-action                (build, sign, draft release, upload)
```

`tauri-action` is pinned to the full commit SHA of its v1.0.0 release rather than the
moving `v1` tag, because that step is handed the signing key.

## Release assets

- `MetaStrip Video_<VERSION>_x64-setup.exe`
- `MetaStrip Video_<VERSION>_x64-setup.exe.sig`
- `latest.json` — generated and uploaded by `tauri-action`, never written by hand

The app reads `latest.json` from
`https://github.com/emilioperna/metastrip-video/releases/latest/download/latest.json`.
Because that URL always resolves to the newest published release, a future release that
somehow shipped without updater artifacts would break the endpoint for everyone. Do not
publish a release built outside this workflow.

## Signing

Updates are signed with a minisign keypair. The public half is in
`src-tauri/tauri.conf.json` under `plugins.updater.pubkey`; the app refuses any update
whose signature does not verify against it.

The private half is **not in this repository and must never be**. It lives outside the
working tree, is password-protected, and is needed only by CI.

The workflow expects two repository secrets, under
**Settings → Secrets and variables → Actions**:

| Secret | Value |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | the contents of the updater private key file |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the password that key was generated with |

Keep an offline copy of both. Losing the private key means existing installations can no
longer be updated — every user would have to reinstall by hand. Rotating it has the same
effect for anyone who does not reinstall, so treat it as permanent.

To generate a keypair (only ever needed once, or after a compromise):

```
npm run tauri signer generate -- -w <path outside this repository>
```
