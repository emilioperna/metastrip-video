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
npm test                      # updater, format logic, privacy aggregation (vitest)
cd src-tauri && cargo test    # format matrix, cleaning, scan, classification, verification
```

The Rust tests build real synthetic, container-specific fixtures with the bundled
FFmpeg and run the actual cleaning path, so `npm run setup:ffmpeg` has to have run first.
They cover the things that would be expensive to get wrong: metadata and chapter
removal, encoded-stream identity, data-track removal, atomic output, ID reuse, and
legacy settings migration.

v0.5 adds classifier tests that need no FFmpeg at all, inspector tests that run
against both fixed JSON documents and the real `ffprobe`, and verification tests
that deliberately break one guarantee at a time — a missing output, an unreadable
output, a modified original, a leftover temp file, a transcoded output — to prove
the verifier reports `verified: false` rather than passing by default.

A fixture only plants metadata the target muxer actually keeps: the MOV muxer
writes `make`/`model`, the MP4 muxer silently drops them, and AVI carries neither
those nor a location. `expected_categories()` in `testkit.rs` is the per-container
truth, and the tests assert the canaries are present *before* cleaning so a
fixture that quietly loses one fails loudly instead of making the cleaning
assertions trivially true.

Benchmarks are `#[ignore]`d, because timings vary by machine and failing a build
on them would be noise:

```
cd src-tauri && cargo test --lib bench -- --ignored --nocapture --test-threads=1
```

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
  privacy.ts          scan types + aggregation, no Tauri imports, unit-tested
  cleaning.ts         cleaning option, warnings and cleaning copy, unit-tested
  updater.ts          update state machine, no Tauri imports, unit-tested
  useUpdater.ts       the hook that talks to the updater plugin
src-tauri/src/
  lib.rs              commands, settings, ID registry, cleaning pipeline
  sidecar.rs          bundled ffmpeg/ffprobe resolution
  inspect.rs          ffprobe JSON -> typed MetadataReport
  privacy.rs          deterministic privacy classification
  plan.rs             CleaningOptions + the clean-time CleaningPlan
  verify.rs           post-clean verification
  testkit.rs          synthetic fixtures shared by the test modules
scripts/              FFmpeg fetch, version consistency check
```

`lib.rs` was one file through v0.4, when the whole backend was a few hundred
lines. v0.5 splits the new pipeline into a module per stage, because those stages
are independently testable and a single file would no longer be navigable. The
cleaning pipeline, the ID registry and the settings stayed in `lib.rs`; sidecar
resolution moved out to `sidecar.rs`.

## Bundled FFmpeg

`ffmpeg.exe` and `ffprobe.exe` are declared as Tauri `externalBin` sidecars in
`src-tauri/tauri.conf.json`, so the bundler copies both next to the app executable in
the install directory.

The binaries themselves are **not committed** — `src-tauri/binaries/*.exe` is gitignored.
`scripts/setup-ffmpeg.ps1` fetches them from a pinned, immutable BtbN release tag and
verifies the SHA-256 of the archive and of each extracted executable, so every machine
and every release build gets byte-identical FFmpeg tools. Both come out of the same
archive, which is what keeps them from drifting to different FFmpeg builds. To move to
another version, update the constants and the `$Sidecars` table at the top of that
script.

Pin an **end-of-month** autobuild tag. BtbN keeps daily builds for roughly two weeks and
one build per month after that, so a mid-month tag stops resolving within weeks and every
clean clone and CI run then fails at `npm run setup:ffmpeg`.

At runtime `ffmpeg_program()` and `ffprobe_program()` in `src-tauri/src/sidecar.rs`
resolve each tool the same way, in this order (shown for `ffmpeg`; `ffprobe` is
identical):

1. `ffmpeg.exe` next to the running executable. This covers both the installed layout
   and `tauri dev`: `tauri_build::build()`, called from `build.rs`, copies every
   `externalBin` into the target directory, so `target/debug/ffmpeg.exe` exists after a
   plain `cargo check`.
2. `ffmpeg-x86_64-pc-windows-msvc.exe` next to the running executable — a fallback for
   Tauri CLI versions that keep the target-triple suffix in the target directory.
3. **Debug builds only:** plain `ffmpeg`, resolved through the system PATH.

A release build never falls back to the PATH: if its own sidecar is missing that means
a broken install, and the app says so rather than silently using some other FFmpeg.

`ffmpeg` runs the cleaner. `ffprobe` is invoked by `inspect.rs`, and both the privacy
scan and the verifier depend on it: without it a file cannot be scanned or verified.
See [`../THIRD-PARTY-NOTICES.md`](../THIRD-PARTY-NOTICES.md) for which build is used
and why.

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
classified as data, and `-map -0:t?` excludes attachment streams, which `-dn` does not
reach and which only Matroska can carry. M4V output explicitly selects the MP4 muxer because FFmpeg
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

## The privacy scan

v0.4 was `file → clean → output`. v0.5 puts an inspection in front of the cleaner
and a verification behind it:

```
media file → Inspector → MetadataReport → PrivacyClassifier → PrivacyFindings

CleaningOptions ──────────────→ ffmpeg_args → stream-copy cleaner → output
fresh MetadataReport + options → CleaningPlan ─→ Verifier → VerificationReport
```

Each stage is a module under `src-tauri/src/`, and each one is a plain function
over plain data — no traits, no registries, no injection. The cleaner in the
middle is the v0.4 stream-copy cleaner. v0.5 added `-map -0:t?`, which drops
attachment tracks; v0.5.1 added `-map -0:disp:attached_pic`, which drops detected
cover art, and `-sn` when the user asks for subtitles to go.

| Module | Job |
| --- | --- |
| `sidecar.rs` | Resolves the bundled `ffmpeg` / `ffprobe` |
| `inspect.rs` | ffprobe JSON → `MetadataReport` |
| `privacy.rs` | `MetadataReport` → `Vec<PrivacyFinding>` |
| `plan.rs` | The user's cleaning option, and what a cleaned file must look like |
| `verify.rs` | Output + original → `VerificationReport` |
| `testkit.rs` | Synthetic fixtures shared by every test module |

### Inspector

One `ffprobe -v error -print_format json -show_format -show_streams -show_chapters`
run per file, normalised into `MetadataReport`.

`ffprobe` was bundled specifically for this. The alternative that needs no new
binary, `ffmpeg -i in -f ffmetadata -`, returns container tags and chapters but
**no `[STREAM]` section at all**: per-stream metadata is invisible to it, and so
are the stream list, `codec_tag_string` (which is how `gpmd`/`mebx` telemetry
tracks are recognised) and the codec parameters the verifier compares. Parsing
`ffmpeg`'s human-readable stderr was rejected outright — it is not a stable
interface.

Nothing downstream sees raw ffprobe JSON. The UI receives `ScanView`, never a
`serde_json::Value`, so a schema change upstream is a change in one file.

Two things the parser deliberately tolerates, because ffprobe does them: numbers
arriving either quoted or bare (`"640"` and `640`), and fields simply being
absent. An unrecognised `codec_type` becomes `StreamKind::Unknown`, which still
counts as a non-media track, so the verifier expects it gone. Which argument
removes it depends on the kind: `-dn` covers data streams, `-map -0:t?` covers
attachments, and neither covers the other.

`StreamSummary.attached_pic` is `disposition.attached_pic` and nothing else: true
only when ffprobe reports a non-zero number there. Absent, `0` or not a number is
false. This one flag is what the whole app means by "detected cover art" -- the
cleaner removes by it, the verifier checks by it, the scan counts by it. There
are deliberately no heuristics on top (one-frame video, image codecs, AVI stills,
Matroska covers an earlier remux demoted to plain video): each of those can be
real footage, and removing footage is the worse mistake. The frontend never sees
the raw disposition, only the `coverImages` count in `ScanView`.

### Classifier

`RULES` in `privacy.rs` is the single table. Categories are Location, Device,
Timestamp, Software, CreatorIdentity, Telemetry, Identifier, Copyright,
Structural and Unknown; severity is LOW / MEDIUM / HIGH and comes from the rule
that matched, so every finding can be explained by pointing at one line.

**Structural is technical metadata, not a privacy finding.** Container
bookkeeping (`major_brand`, `minor_version`, `compatible_brands`, `handler_name`,
`language`, `vendor_id`, …) keeps its rule and stays in the finding list, but
`PrivacySummary` counts it only in `technical`: it never contributes to the
privacy `total` or to the HIGH / MEDIUM / LOW figures, per file or per batch.
The UI shows it in a separate, lower-emphasis "Technical metadata" section with a
neutral `TECHNICAL` tag instead of a severity, and the completion screen reports
privacy fields removed and technical fields removed separately. Unknown keeps
its current semantics and still counts as a privacy finding.

Matching is layered rather than substring, and the most specific match wins:

| Kind | Matches | Why |
| --- | --- | --- |
| `Exact` | the whole normalised key | `creation_time` |
| `Prefix` | a namespace | `com.apple.quicktime.location.*` |
| `Suffix` | the last dotted segment | `com.android.model` → `model` |
| `Contains` | anywhere | vendor keys with no stable shape |

Plain substring matching fails in both directions: `model` is inside
`modification_time`, and `com.apple.quicktime.make` never equals `make`. Both
cases are regression tests.

Two rules are not key-driven:

- A value shaped like ISO 6709 (`+45.4642+009.1900/`) is Location/HIGH whatever
  its key, which catches coordinates hiding in a vendor field.
- Non-media tracks are classified by codec tag and handler name: `gpmd`, `mebx`,
  `fdsc`, `rtmd` or a `GoPro MET`-style handler is Telemetry/HIGH; any other data
  track is Unknown/MEDIUM, because its payload cannot be described in advance.

An unrecognised key is Unknown/**LOW**, never HIGH — it is still removed, it is
just not announced as dangerous. Free-text keys (`title`, `comment`,
`description`, …) are Unknown/**MEDIUM** with their own explanation: they are
recognised, but their contents are not predictable.

Explanations are static strings per category. Nothing is generated at runtime and
no metadata value can leak into the copy.

#### Why `vendor_id` and `duration` are Structural

Both are written by the muxer on the way out and cannot be removed by
`-map_metadata -1`. `vendor_id` names the muxer (`FFMP`), not the camera;
`duration` is Matroska's per-stream tag. Classifying them as Device or Unknown
made verification fail on every MOV and MKV for no privacy gain.

### Cleaning options and the clean-time plan

Cleaning has a fixed privacy floor and one choice.

- **The floor** is not an option and has no field anywhere: global and per-stream
  metadata, chapters, data / attachment / unknown tracks and detected cover art
  always go.
- **`CleaningOptions { remove_subtitles }`** is the choice. It is `false` by
  default, and it drives `ffmpeg_args` directly: `-sn` when set, nothing when not.
  A file that cannot be inspected is therefore still cleaned exactly as asked.

`CleaningPlan` is the verifier's contract for one file and nothing else. It is
built at clean time from the **fresh** pre-clean report and the batch's options:

- `remove_subtitles` -- whether check 8 must prove no subtitle remains;
- `expected_media` -- every media stream that must survive, in input order: the
  input's video, audio and subtitle streams, minus detected cover art, minus
  subtitles when they are being removed. Each entry carries its kind, its
  `StreamIdentity` and its `attached_pic` flag.

The scan sends no plan: options can change after a file was scanned, and a plan
the scan computed would describe a run that is not going to happen. The v0.5
descriptive fields (`container_strategy`, `guarantees`, `preserved_streams`, the
expected counts) were read by nothing and are gone.

There is no preset enum. One boolean does not need one, and a stored preset name
whose meaning changed in an update would silently change what existing users'
files come out as.

### Verifier

After a file is cleaned it is inspected again and checked against the input.
**Every check must pass** before the word "verified" is used anywhere in the UI:

1. the output exists;
2. the output can be read back (a failure here stops the run rather than letting
   later checks pass vacuously);
3. no disclosure from the input survives;
4. nothing at MEDIUM or above is present in the output;
5. the output has no chapters -- whatever the input had;
6. the output has no data, attachment or unknown track -- whatever the input had;
7. "Detected cover art removed": no output stream is marked `attached_pic`;
8. "Subtitle tracks removed", only when the batch asked for it: no subtitle remains;
9. "Kept stream parameters match": the output's media streams are exactly
   `plan.expected_media` -- same count, same order, same kind, same
   `StreamIdentity`, same `attached_pic` (a parameter comparison, not packet
   identity);
10. the original is unchanged;
11. the extension is preserved;
12. no temporary file is left in the output folder.

Checks 5, 6 and 7 are the **floor** and never read the plan: an output that gained
chapters or a track is not clean just because the input had none to remove. Only
checks 8 and 9 read the plan.

Check 9 compares counts before it pairs streams, so a missing stream can never be
skipped by `zip` stopping at the shorter list, and it reports which kind moved: a
lost second camera angle reads "Video stream count changed: 2 expected, 1 found",
never as cover art that went.

Check 3 compares key, scope **and value**, on any stream. It deliberately ignores
the stream index: removing a stream renumbers everything after it, and a
disclosure that survived on input stream 2 turns up on output stream 1. Ignoring
the index can only find more survivors, never fewer. A muxer rewriting its own tag
is not a surviving disclosure: `encoder` goes from `Lavf62.12.102`, which pins the
exact build, to the bare `Lavf` that `-fflags +bitexact` produces. Check 4
independently rejects anything sensitive appearing under any value, so a partial
rewrite cannot slip between the two.

Nothing checks what a kept stream contains. The text of a subtitle, the picture
and the sound are copied, not inspected, and the UI says so; that is also why a
cover image -- which carries its own EXIF, GPS included, inside the picture -- is
removed rather than kept and trusted.

Structural findings are excluded from check 3 because a muxer has to write
`major_brand`, `handler_name` and `vendor_id` back. Matroska and WebM also
regenerate `encoder`, so those outputs legitimately keep one LOW Software
finding, and the before/after table reports it as still present rather than
hiding it.

A file that cannot be inspected is still cleaned — inspection is what makes
verification possible, not what makes cleaning safe — but it is reported as
cleaned-and-not-verified, never as verified.

#### Stream parameters at runtime, packet identity in tests

Not re-encoding is a property of the pipeline: every cleaning invocation uses
`-c copy`, and there is no transcoding path to fall back to. What is checked, and
where, is split on purpose.

- **Runtime**, in `verify.rs`, check "Kept stream parameters match": codec name,
  codec tag, profile, dimensions, pixel format, sample rate, channels and layout
  are compared between input and output for every stream the plan keeps. One
  ffprobe run, constant time in the file size. It catches a missing stream or one
  that came out as a different codec or format. It does **not** prove the packets
  are bit-for-bit the same, and on its own it cannot rule out a re-encode that
  kept every parameter. The UI therefore says only "stream copy", "no
  transcoding" and "matching codec parameters" — never bit-for-bit or
  byte-identical.
- **Regression**, in the tests only: `deep_regression_encoded_payloads_are_byte_identical`
  hashes every stream's encoded packets with FFmpeg's `md5` muxer and asserts
  byte equality across all six containers. This, together with
  `ffmpeg_options_are_container_specific_and_never_transcode`, is what backs the
  no-re-encoding claim.

Hashing at runtime was rejected on measured cost: it turns a millisecond-scale
check into a full re-read of every file in the batch, which is fine for a
two-second fixture and not fine for a 4 GB holiday video. `a_transcoded_output_fails_the_stream_parameters_check`
feeds the verifier a deliberately transcoded file (different codec) to prove the
parameter check bites.

"The original is unchanged" is checked by size and modification time, not a
content hash, for the same reason: the cleaner never opens the input for writing,
so this guards against a bug or an outside process, and hashing multi-gigabyte
inputs every run would cost more than the guard is worth. The tests compare full
bytes instead.

### Measured cost

`cargo test --lib bench -- --ignored --nocapture --test-threads=1`

On the development machine, with two-second synthetic fixtures:

| Batch | Scan | Clean only (v0.4) | Clean + verify (v0.5) | Verify overhead |
| --- | --- | --- | --- | --- |
| 1 file | 56 ms | 62 ms | 170 ms | 108 ms |
| 6 mixed | 316 ms | 363 ms | 998 ms | 635 ms |
| 20 mixed | 1.04 s | 1.21 s | 3.30 s | 2.09 s |

Everything is linear in file count: ~52 ms per scan, ~61 ms per clean, ~104 ms of
verification. One inspection costs ~51 ms and is dominated by process startup, so
the per-file cost is roughly fixed regardless of how large the video is. The
verification overhead is exactly two inspections: `clean_and_verify` re-inspects
the input, then inspects the output.

The input is re-inspected rather than reusing the scan because the file may have
changed between being queued and being cleaned, and verification has to compare
against what was actually on disk at the time. Caching that behind a size and
mtime check would remove about a third of the overhead and is the obvious next
optimisation if it ever matters.

### Failure handling

| Failure | Behaviour |
| --- | --- |
| A file cannot be inspected | Row shows `Scan failed`; the rest of the batch scans normally; the file can still be cleaned |
| `ffprobe` is missing | One clear error at startup, not one per file |
| Cleaning fails | Existing v0.4 behaviour: the file is an error, the batch continues |
| Cleaning succeeds, verification fails | The output is kept, the row reads `Verification failed`, and the failing checks are listed. The word "verified" is not used |
| Cleaning succeeds, no baseline to verify against | The row reads `Not verified`, with the reason |

### UI

The queue row stays a summary: severity chips and a total, plus a 2px edge tint
by worst finding. Values are only rendered for the one row the reader opens, which
is what keeps 100 files usable. After cleaning, the same panel shows the
before/after table instead of the findings list.

`src/privacy.ts` holds the types and the aggregation, imports nothing from Tauri
and is unit tested — that is what keeps the batch counters and the "verified"
wording honest without driving the whole app.

## Stored state

```
%APPDATA%\com.metastrip.video\settings.json    prefix + output folder + cleaning choice
%APPDATA%\com.metastrip.video\used-ids.txt     one ID per line, append-only
```

The folder comes from `app_config_dir()`, which Tauri derives from `identifier` in
`tauri.conf.json`. Nothing in the Rust code hardcodes a product name, so renaming or
forking the app moves this folder on its own.

Plain files, no database. A missing or corrupt `settings.json` falls back to defaults
instead of refusing to start. If the saved output folder has been deleted, the app says
so and disables cleaning until a new one is chosen.

`settings.json` is read leniently, because every v0.5.0 install already has one
without a `cleaning` key and an update must not cost anyone their prefix or folder.
`#[serde(default)]` on the struct fills in a missing key; the `lenient` deserialiser
on `cleaning` turns a value this version cannot read (`null`, a string, a number, an
object of the wrong shape) into the default options instead of failing the whole
document. Unknown keys are ignored. Only a document that is not JSON at all falls
back to defaults whole, as in v0.5.0.

Every save is read-modify-write through `update_settings`: `save_settings` changes
prefix and folder, `save_cleaning_options` changes the cleaning choice, and neither
can erase the other's value. Writes go to `settings.json.tmp`, are flushed to disk,
and are renamed over the real file, so a crash mid-write leaves the previous file
intact.

The stored cleaning choice is a preference, not what a batch runs with. The page
snapshots the checkbox at the click and sends it with `clean_videos(paths,
options)`; the backend uses that value for every file of the batch, never re-reads
`settings.json` for it, and echoes it back in `CleanSummary.options`. The checkbox
is disabled while the pipeline is busy (cleaning or installing an update). It stays
enabled while files are scanning: nothing the scan finds depends on it.

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
node scripts/check-version.mjs v0.5.0   # also check they match a tag
```

CI runs the second form on a release tag and refuses to build on a mismatch.
