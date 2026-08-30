# Development

How MetaStrip Video is put together, and how to work on it locally.

## Requirements

- Node.js + npm
- Rust toolchain (MSVC, x86_64)

## Running it

```
npm install
npm run setup:ffmpeg
npm run tauri dev
```

`npm run setup:ffmpeg` downloads the FFmpeg binary that gets bundled into the app. Run
it once after cloning; it is a no-op if the right binary is already in place.

## Tests

```
npm test                      # updater + frontend format logic (vitest)
cd src-tauri && cargo test    # format matrix, cleaning pipeline, ID registry, settings
```

The Rust tests build real synthetic, container-specific fixtures with the bundled
FFmpeg and run the actual cleaning path, so `npm run setup:ffmpeg` has to have run first.
They cover the things that would be expensive to get wrong: metadata and chapter
removal, encoded-stream identity, data-track removal, atomic output, ID reuse, and
legacy settings migration.

## Building the installer

```
npm run tauri build
```

Output:

```
src-tauri/target/release/bundle/nsis/MetaStrip Video_<VERSION>_x64-setup.exe
```

The unpacked app (`metastrip-video.exe` plus `ffmpeg.exe`) is in
`src-tauri/target/release/`. That setup exe is the only thing an end user needs; they
never have to know FFmpeg is involved.

## Layout

```
src/                  React UI
  App.tsx             the whole window
  updater.ts          update state machine, no Tauri imports, unit-tested
  useUpdater.ts       the hook that talks to the updater plugin
src-tauri/src/lib.rs  everything Rust: FFmpeg pipeline, ID registry, settings, commands
scripts/              FFmpeg fetch, version consistency check
```

`lib.rs` is deliberately one file. The whole backend is a few hundred lines and
splitting it would cost more in navigation than it saves.

## Bundled FFmpeg

`ffmpeg.exe` is declared as a Tauri `externalBin` sidecar in `src-tauri/tauri.conf.json`,
so the bundler copies it next to the app executable in the install directory.

The binary itself is **not committed** — `src-tauri/binaries/*.exe` is gitignored.
`scripts/setup-ffmpeg.ps1` fetches it from a pinned, immutable BtbN release tag and
verifies the SHA-256 of both the archive and the extracted executable, so every machine
and every release build gets byte-identical FFmpeg. To move to another version, update
the constants at the top of that script.

At runtime `ffmpeg_program()` in `src-tauri/src/lib.rs` resolves FFmpeg in this order:

1. `ffmpeg.exe` next to the running executable. This covers both the installed layout
   and `tauri dev`: `tauri_build::build()`, called from `build.rs`, copies every
   `externalBin` into the target directory, so `target/debug/ffmpeg.exe` exists after a
   plain `cargo check`.
2. `ffmpeg-x86_64-pc-windows-msvc.exe` next to the running executable — a fallback for
   Tauri CLI versions that keep the target-triple suffix in the target directory.
3. **Debug builds only:** plain `ffmpeg`, resolved through the system PATH.

A release build never falls back to the PATH: if its own sidecar is missing that means
a broken install, and the app says so rather than silently using some other FFmpeg.

Only `ffmpeg` is bundled. `ffprobe` is not used by the app and is deliberately not
shipped. See [`../THIRD-PARTY-NOTICES.md`](../THIRD-PARTY-NOTICES.md) for which build is
used and why.

## The cleaning pipeline

`FORMAT_PROFILES` is the support contract: each accepted extension maps to an explicit
container family and output muxer. `get_supported_formats()` exposes that same matrix
to the frontend, so drag/drop validation, picker filters and user-facing copy do not
carry a second extension list.

The common FFmpeg arguments are in `ffmpeg_args()` and are effectively frozen: changing
them changes what the product does, and every one of them is there for a reason a test
enforces. ISO-BMFF profiles add faststart on the first attempt and may retry once
without it. Other profiles never receive MOV options. AVI adds `-ignore_unknown`
because FFmpeg exposes AVI data tracks as unknown streams; `-dn` still handles streams
classified as data. M4V output explicitly selects the MP4 muxer because FFmpeg
otherwise maps the `.m4v` suffix to a raw MPEG-4 video muxer; a bounded ISO-BMFF box
check rejects raw M4V elementary streams before processing. No fallback ever replaces
`-c copy` with an encoder.

Arguments are passed straight to the process — no shell, no string concatenation.

### Atomic output

FFmpeg writes to `.video-cleaner-processing-<id>.<ext>` inside the output folder, and
the file is renamed to its final name only after FFmpeg exits successfully. A kill, a
crash or a power cut can therefore leave a `.video-cleaner-processing-*` file behind,
but never a truncated video under a name that looks finished. Those leftovers are swept
at the start of the next batch; nothing else in the folder is touched.

The temporary file is created in the output folder, not in `%TEMP%`, so the final step
is a same-volume rename and stays atomic.

`-n` rather than `-y`: the app already guarantees the target name is free, so an
existing file means something is wrong and must not be overwritten.

### Unique names

Each output gets a random 10-digit ID that is never reused. Before FFmpeg starts, the ID
is checked against a registry of every ID ever issued *and* against the output folder,
then appended to the registry and flushed to disk. Reserving before processing means a
crash can never hand the same number out twice; the cost is that a failed conversion
burns its ID, which is irrelevant at ten billion combinations.

## Stored state

```
%APPDATA%\com.metastrip.video\settings.json    prefix + output folder
%APPDATA%\com.metastrip.video\used-ids.txt     one ID per line, append-only
```

The folder comes from `app_config_dir()`, which Tauri derives from `identifier` in
`tauri.conf.json`. Nothing in the Rust code hardcodes a product name, so renaming or
forking the app moves this folder on its own.

Plain files, no database. A missing or corrupt `settings.json` falls back to defaults
instead of refusing to start. If the saved output folder has been deleted, the app says
so and disables cleaning until a new one is chosen.

Installations from v0.2.0 and earlier stored the same two files under the previous
identifier, `com.aurevm.videocleaner`. On startup the app copies them across once if the
current folder does not already have them; the old folder is left untouched, and a
failed copy is ignored rather than blocking startup. `LEGACY_CONFIG_DIR` in
`src-tauri/src/lib.rs` exists only for that, and can be deleted once no pre-rebrand
install is left.

## The updater

`src/updater.ts` holds the state machine and imports nothing from Tauri, which is what
makes its failure paths testable. `src/useUpdater.ts` is the only place that talks to
the plugin.

Checking, downloading and installing are deliberately separate steps. On Windows
`update.install()` hands the installer to the shell and calls `exit(0)`, so installing
during a batch would abandon the remaining videos. The download happens as soon as an
update exists; the installer waits for the batch to finish.

There is no `relaunch()` call: `installMode: "passive"` passes NSIS `/R`, so the
installer restarts the app itself, and nothing after `install()` would run anyway.

Every failure path — GitHub unreachable, malformed `latest.json`, a signature that does
not verify, a download that dies — ends on idle with the app untouched and nothing
shown to the user.

## Version consistency

`package.json`, `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json` all carry the
version and must agree; the updater compares against the one in `tauri.conf.json`.

```
node scripts/check-version.mjs          # check the three agree
node scripts/check-version.mjs v0.3.1   # also check they match a tag
```

CI runs the second form on a release tag and refuses to build on a mismatch.
