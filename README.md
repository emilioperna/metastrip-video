# MetaStrip Video — Video Metadata Remover

Fast, offline batch video metadata remover for Windows.
Strip metadata from MP4 and MOV files without re-encoding.

- Batch process up to 100 videos
- No re-encoding
- Original audio/video quality preserved
- Local and offline
- Persistent output folder
- Automatic unique filenames
- FFmpeg bundled

Desktop app built with Tauri 2, React, TypeScript and Rust. An installed copy does not
need FFmpeg on the system PATH.

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
src-tauri/target/release/bundle/nsis/MetaStrip Video_0.2.1_x64-setup.exe
```

The unpacked app (`metastrip-video.exe` plus `ffmpeg.exe`) is in
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
   and `tauri dev`: `tauri_build::build()`, called from `build.rs`, copies every
   `externalBin` into the target directory, so `target/debug/ffmpeg.exe` exists after a
   plain `cargo check`;
2. `ffmpeg-x86_64-pc-windows-msvc.exe` next to the running executable — a fallback for
   Tauri CLI versions that keep the target-triple suffix in the target directory;
3. **debug builds only:** plain `ffmpeg`, resolved through the system PATH.

A release build never falls back to the PATH: if its own sidecar is missing that means
a broken install, and the app says so rather than silently using some other FFmpeg.

Only `ffmpeg` is bundled. `ffprobe` is not used by the app and is deliberately not
shipped. See `THIRD-PARTY-NOTICES.md` for which build is used and why.

## How it works

Pick an output folder and a file-name prefix once. From then on: drop up to 100
MP4/MOV files (or use **Select videos**), press **Clean N videos**, done. Files are
processed one at a time; a failure on one file does not stop the rest.

Every output is renamed to `<PREFIX>_<10 digits>.<original extension>`, for example
`VIDEO_0917283645.mp4`. Leading zeros are kept, so the numeric part is always exactly
ten characters. Originals are never modified or deleted.

Per file, the app runs:

```
ffmpeg -n -i INPUT \
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

### Atomic output

FFmpeg writes to `.video-cleaner-processing-<id>.<ext>` inside the output folder, and the file
is renamed to its final name only after FFmpeg exits successfully. A kill, a crash or a
power cut can therefore leave a `.video-cleaner-processing-*` file behind, but never a
truncated video under a name that looks finished. Those leftovers are swept at the
start of the next batch; nothing else in the folder is touched.

The temporary file is created in the output folder, not in `%TEMP%`, so the final step
is a same-volume rename and stays atomic.

`-n` rather than `-y`: the app already guarantees the target name is free, so an
existing file means something is wrong and must not be overwritten.

### Unique names

Each output gets a random 10-digit ID that is never reused. Before FFmpeg starts, the
ID is checked against a registry of every ID ever issued *and* against the output
folder, then appended to the registry and flushed to disk. Reserving before processing
means a crash can never hand the same number out twice; the cost is that a failed
conversion burns its ID, which is irrelevant at ten billion combinations.

### Stored state

```
%APPDATA%\<bundle identifier>\settings.json    prefix + output folder
%APPDATA%\<bundle identifier>\used-ids.txt     one ID per line, append-only
```

The folder comes from `app_config_dir()`, which Tauri derives from `identifier` in
`tauri.conf.json` — currently `com.metastrip.video`. Nothing in the Rust code
hardcodes a product name, so renaming or forking the app moves this folder on its own.

Installations from v0.2.0 and earlier stored the same two files under the previous
identifier, `com.aurevm.videocleaner`. On startup the app copies them across once if
the current folder does not already have them; the old folder is left untouched, and a
failed copy is ignored rather than blocking startup. `LEGACY_CONFIG_DIR` in
`src-tauri/src/lib.rs` exists only for that, and can be deleted once no pre-rebrand
install is left.

Plain files, no database. A missing or corrupt `settings.json` falls back to defaults
instead of refusing to start. If the saved output folder has been deleted, the app says
so and disables cleaning until a new one is chosen.

## Licenses and attribution

This app redistributes **FFmpeg** (LGPL v3+, unmodified upstream binary, run as a
separate process) and reuses the FFmpeg argument pipeline from
**Simple-Metadata-Remover** (MIT).

Full notices, versions, checksums and source links are in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md). The FFmpeg license text is also
installed alongside the application as `FFMPEG-LICENSE.txt`.
