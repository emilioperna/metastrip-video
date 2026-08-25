# Aurevm Video Cleaner

Desktop app (Tauri 2 + React + TypeScript + Rust) that strips metadata from MP4 and MOV
files with FFmpeg, without re-encoding.

FFmpeg is **bundled with the app** — an installed copy does not need FFmpeg on the
system PATH.

## Build requirements

- Node.js + npm
- Rust toolchain (MSVC, x86_64)

## Run

```
git clone <repo>
npm install
npm run setup:ffmpeg
npm run tauri dev
```

`npm run setup:ffmpeg` downloads the FFmpeg binary that gets bundled into the app.
It is not stored in git — see [Bundled FFmpeg](#bundled-ffmpeg). Run it once after
cloning; it is a no-op if the right binary is already in place.

## Build the Windows installer

```
npm run tauri build
```

Output:

```
src-tauri/target/release/bundle/nsis/Aurevm Video Cleaner_0.1.0_x64-setup.exe
```

The unpacked app (`aurevm-video-cleaner.exe` plus `ffmpeg.exe`) is in
`src-tauri/target/release/`. That setup exe is the only thing an end user needs;
they never have to know FFmpeg is involved.

## Bundled FFmpeg

`ffmpeg.exe` is declared as a Tauri `externalBin` sidecar in
`src-tauri/tauri.conf.json`, so the bundler copies it next to the app executable in
the install directory.

The binary itself is **not committed** — `src-tauri/binaries/*.exe` is gitignored.
`scripts/setup-ffmpeg.ps1` fetches it from a pinned, immutable BtbN release tag and
verifies the SHA-256 of both the archive and the extracted executable, so every
machine and every release build gets byte-identical FFmpeg. To move to another
version, update the constants at the top of that script.

At runtime `ffmpeg_program()` in `src-tauri/src/lib.rs` resolves FFmpeg in this order:

1. `ffmpeg.exe` next to the running executable. This covers both the installed layout
   and `tauri dev`, because the build script also copies the sidecars into
   `target/debug`;
2. `ffmpeg-x86_64-pc-windows-msvc.exe` next to the running executable — a fallback for
   Tauri CLI versions that keep the target-triple suffix in the target directory;
3. **debug builds only:** plain `ffmpeg`, resolved through the system PATH. This is what
   you get from a bare `cargo run` with no sidecars present.

A release build never falls back to the PATH: if its own sidecar is missing that means
a broken install, and the app says so rather than silently using some other FFmpeg.

Only `ffmpeg` is bundled. `ffprobe` is not used by the app and is deliberately not
shipped. See `THIRD-PARTY-NOTICES.md` for which build is used and why.

## How it works

Drop up to 100 MP4/MOV files (or use **Select videos**), then press **Clean N videos**.
Files are processed one at a time; a failure on one file does not stop the rest.

Output goes to a `cleaned` folder created next to the **first** video. Names are
`original_cleaned.ext`, falling back to `original_cleaned_2.ext`, `_3`, … when taken.
Originals are never modified or deleted.

Per file, the app runs:

```
ffmpeg -y -i INPUT \
  -map 0 -c copy \
  -map_metadata -1 -map_metadata:s -1 -map_chapters -1 \
  -fflags +bitexact \
  -movflags +faststart \
  OUTPUT
```

`-c copy` means the audio and video bitstreams are byte-identical to the source.
If the container rejects `+faststart`, the file is retried once without it rather
than being reported as an error. Arguments are passed straight to the process — no
shell, no string concatenation.

## Licenses and attribution

This app redistributes **FFmpeg** (LGPL v3+, unmodified upstream binary, run as a
separate process) and reuses the FFmpeg argument pipeline from
**Simple-Metadata-Remover** (MIT).

Full notices, versions, checksums and source links are in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md). The FFmpeg license text is also
installed alongside the application as `FFMPEG-LICENSE.txt`.
