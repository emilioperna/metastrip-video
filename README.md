# MetaStrip Video — Video Metadata Remover

> See what privacy metadata a video contains, remove it locally without re-encoding,
> and verify the result. Offline, for Windows.
>
> **Scan → Clean → Verify** · MP4 · MOV · M4V · MKV · WebM · AVI

[![Latest release](https://img.shields.io/github/v/release/emilioperna/metastrip-video?label=latest&color=2ea043)](https://github.com/emilioperna/metastrip-video/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform: Windows](https://img.shields.io/badge/Platform-Windows%20x64-0078D4.svg)](#download)
[![Built with Tauri 2](https://img.shields.io/badge/Built%20with-Tauri%202-24C8DB.svg)](https://tauri.app)
[![Rust](https://img.shields.io/badge/Rust-stable-B7410E.svg)](https://www.rust-lang.org)
[![FFmpeg LGPL](https://img.shields.io/badge/FFmpeg-LGPL%20v3%2B-007808.svg)](THIRD-PARTY-NOTICES.md)

**[Download MetaStrip Video for Windows →](https://github.com/emilioperna/metastrip-video/releases/latest)**
Windows x86_64 · signed installer · FFmpeg bundled · no account

![MetaStrip Video ready to clean MP4, MOV, M4V, MKV, WebM and AVI files](docs/assets/metastrip-video.png)

<!-- Demo GIF goes here once recorded:
     ![Cleaning five videos with MetaStrip Video](docs/assets/demo.gif) -->

Drop up to 100 videos. MetaStrip shows what each one discloses, writes clean copies,
and checks every copy before calling it verified.

1. **Scan.** Each file is inspected with the bundled `ffprobe` as soon as it is added.
   What it finds is classified by fixed rules into categories — location, device,
   timestamp, software, creator identity, telemetry, identifier, copyright, unknown —
   each with a LOW, MEDIUM or HIGH severity. Container bookkeeping such as
   `major_brand` or `handler_name` is listed separately as technical metadata and is
   not counted as a privacy finding.
2. **Clean.** Metadata, chapters, data tracks and attachments are dropped while the
   video and audio are stream-copied (`-c copy`). Nothing is re-encoded, and there is
   no transcoding fallback: a file that cannot be stream-copied fails.
3. **Verify.** The output is inspected again and checked against the input: sensitive
   metadata gone, chapters and non-media tracks gone, video, audio and subtitle stream
   parameters unchanged, original unchanged. Only a file that passes every check is
   marked verified; a failed check is reported, never hidden.

- **Your videos never leave your computer.** No upload, no cloud, no server.
- **No account**, no sign-in.
- **No telemetry**, no analytics, no crash reporting.
- **No video or audio re-encoding.** The encoded streams are copied, not transcoded.
- **Your originals are never modified** or deleted.
- **Signed automatic updates**, verified against a key compiled into the app.
- **FFmpeg is bundled** — nothing else to install.

## Why MetaStrip?

Every video you export carries more than picture and sound. A phone stamps in the
camera model and the time. An editor leaves its name. A GoPro writes a whole GPS
telemetry track. Publish the file and you publish all of it.

Most tools that strip this re-encode the video to do it, which costs quality and time.
MetaStrip stream-copies the encoded video and audio and rebuilds the container without
the metadata. Because the streams are copied rather than transcoded, cleaning is
typically much faster than re-encoding, and the result looks exactly like the original.

It also shows its work: what a file disclosed before cleaning, and whether each of those
fields is gone afterwards.

It is deliberately small: one window, one button, no project files and no settings page.
And it is inspectable — the FFmpeg strategy is documented
[in this README](#how-it-works) and in
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md), and the source is here to read.

## Features

- Batch up to **100 videos** at a time
- **MP4, MOV, M4V, MKV, WebM and AVI**
- **Privacy Scan** before cleaning: deterministic categories and severity, with
  technical/container metadata shown separately from privacy findings
- Removes global metadata
- Removes per-stream metadata
- Removes chapters where the container exposes them
- Removes data and unknown metadata streams where the container exposes them
- Removes attachment tracks (embedded fonts and files in Matroska)
- Keeps subtitle tracks, and checks they survive
- **Verified Cleaning**: every output is re-inspected, with a before/after view per file
- **No video re-encoding**
- **No audio re-encoding**
- Original files are left untouched
- Atomic outputs: a failed run never leaves a partial file under a finished name
- **FFmpeg is bundled**, nothing to install separately
- **Signed automatic updates** through GitHub Releases

## Download

**[Download MetaStrip Video for Windows](https://github.com/emilioperna/metastrip-video/releases/latest)**

Grab the `x64-setup.exe` from the latest release and run it. That installer is the only
thing you need: **FFmpeg is bundled**, so there is nothing else to install and nothing to
configure.

- **Windows x86_64**
- Published on **GitHub Releases**, and **signed** — the app verifies the signature of
  every update it installs
- Updates itself from then on

Prefer to build it yourself? See [Building from source](#building-from-source).

## How to use

1. Install MetaStrip Video.
2. Choose an output folder, and a file-name prefix if you want one.
3. Drop your videos onto the window, or press **Select videos**. Each one is scanned
   straight away; open a row to see its privacy findings and technical metadata.
4. Press **Clean**.
5. The cleaned copies are in the folder you chose. Each row reports **Verified**,
   **Verification failed** (with the checks that failed) or **Not verified** (with the
   reason).

Every output is named `PREFIX_##########.<original-extension>`, where the ten digits
are a random ID that is never reused — for example `VIDEO_0917283645.mkv`. The output
keeps the input extension, and the originals keep their own names in their own folder,
unchanged.

## What gets removed?

MetaStrip removes supported metadata structures exposed by each container profile.
Not every container or input carries every structure listed below.

| Removed | Examples |
| --- | --- |
| Global metadata | title, artist, album, comment, copyright, description, creation time, encoder |
| Location tags | `location`, `com.apple.quicktime.location.ISO6709` |
| Device tags | `com.apple.quicktime.make`, `.model`, `.software` |
| Per-stream metadata | track titles, handler names, stream language |
| Chapters | chapter names and the text track carrying them |
| Data tracks | GoPro `gpmd` telemetry, iPhone `mebx`, other timed-metadata streams |
| Attachments | fonts and other files embedded in Matroska |

Data tracks are worth calling out: stripping the tags around such a track leaves the
track itself, payload and all. MetaStrip drops those tracks when the container exposes
them as data or unknown metadata streams.

## What stays untouched?

- **The video bitstream.** Stream-copied, not re-encoded.
- **The audio bitstream.** Same.
- **Subtitle tracks.** Stream-copied like video and audio.
- **Your original files.** Never modified, never deleted, never moved.
- **Picture quality.** There is no quality setting because nothing is re-compressed.

The container itself is rewritten by FFmpeg, so the output file is not a byte-for-byte
copy of the input. The muxer also writes some technical metadata of its own back —
`major_brand`, `handler_name` or `vendor_id`, and a bare `encoder` tag in Matroska and
WebM. The scan lists these as technical metadata, and the before/after view reports
them as still present rather than hiding them.

## What "verified" means

After cleaning, MetaStrip inspects the output with `ffprobe` and compares it with the
input. A file is marked **Verified** only if every check passes:

- the output exists and can be read back;
- no privacy finding from the input survives with its original value, and nothing at
  MEDIUM or above is present in the output;
- chapters are gone, where the input had any;
- data and attachment tracks are gone, where the input had any;
- the video, audio and subtitle streams are all still there, with matching stream
  parameters (codec, codec tag, profile, dimensions, pixel format, sample rate, channels
  and layout);
- the original is unchanged, the extension is preserved, and no temporary file is left.

If any check fails, the output is kept but the row reads **Verification failed** and
lists what failed. A file that cannot be inspected is still cleaned, but reported as
**Not verified**.

Verification compares stream parameters; it does not hash the encoded packets of every
file. That the packets themselves come through unchanged is a property of stream copy,
and it is enforced by regression tests that hash every stream's encoded packets across
all six containers — not re-proven at runtime for each video.

## Private by design

Video processing is entirely local. Your files are read from your disk, handed to a
bundled FFmpeg on your machine, and written back to a folder you chose.

- No upload, no cloud processing, no server
- No account, no sign-in
- No analytics, no telemetry, no crash reporting
- No database

The **only** network request the app makes is the signed update check against GitHub
Releases, described below. If you block it, everything else keeps working.

## Automatic updates

MetaStrip checks GitHub Releases for a newer version at startup, and once an hour
after that. A newer version downloads in the background and installs itself.

Updates are signed. The app carries the matching public key and refuses anything whose
signature does not verify, so a tampered or unsigned installer is never run.

An update never interrupts your work: if a batch is being processed the download still
happens, but the installer waits until the last video is done. If GitHub cannot be
reached, the check is skipped silently.

Your settings survive updates — the prefix, output folder and used-ID registry live in
`%APPDATA%\com.metastrip.video\` and no install touches them.

## Supported formats

| Format | Container strategy |
| --- | --- |
| MP4 | ISO-BMFF, stream copy |
| MOV | QuickTime/MOV, stream copy |
| M4V | ISO-BMFF M4V only, stream copy |
| MKV | Matroska, stream copy |
| WebM | WebM-compatible codecs, stream copy |
| AVI | AVI, stream copy |

Supported format does not mean arbitrary codecs can be remuxed into that container.
If stream-copy is incompatible, MetaStrip fails that file rather than re-encoding it.

The current build targets Windows 10/11 on x86_64.

## How it works

Files are inspected with the bundled `ffprobe` (`-show_format -show_streams
-show_chapters`, JSON output). The Privacy Scan inspects each file when it is added.
Immediately before cleaning, the input is inspected again to take a fresh baseline, in
case it changed while queued. After cleaning, the written output is inspected and
verified against that baseline. Nothing is parsed from FFmpeg's human-readable output.

For each file, MetaStrip invokes the bundled FFmpeg with this common stream-copy core:

```
ffmpeg -n -i INPUT \
  -map 0 -map -0:t? -c copy \
  -map_metadata -1 \
  -map_metadata:s -1 \
  -map_chapters -1 \
  -dn \
  -fflags +bitexact \
  ...container-specific options... \
  OUTPUT
```

- `-c copy` copies the encoded streams instead of re-encoding them. This is why nothing
  is re-compressed, and why cleaning is typically much faster than transcoding.
  Subtitle streams are selected by `-map 0` and copied the same way.
- `-map_metadata -1 -map_metadata:s -1 -map_chapters -1` drop file-level metadata,
  per-stream metadata and chapters.
- `-dn` drops data tracks. `-map 0` would otherwise copy them, and a data track is
  metadata in its own right.
- `-map -0:t?` drops attachment tracks: embedded subtitle fonts and similar files,
  which Matroska can carry. `-dn` does not reach them, and once metadata is
  stripped such a track lacks the `filename` tag its muxer requires, which would
  fail the whole file.
- Cover art that FFmpeg reports as a video stream (`attached_pic`) is not an
  attachment, so `-map -0:t?` never selects it and the output is what it would be
  without that argument. What that output keeps depends on the container: MP4 and
  M4V keep the image as cover art, Matroska keeps it as an ordinary one-frame video
  track, and MOV output does not keep it. A Matroska cover under a mimetype FFmpeg
  cannot map to an image codec is an attachment stream and is removed with the
  rest, which is the right outcome for an opaque embedded file.
- `-fflags +bitexact` keeps FFmpeg from stamping its own version into the output.

Container-specific behaviour is explicit:

- **MP4, MOV and M4V:** select the appropriate muxer, add `-movflags +faststart`, and
  retry once without faststart if that first stream-copy attempt fails.
- **MKV:** select the Matroska muxer; no MOV flags are passed.
- **WebM:** select the WebM muxer; no MOV flags are passed. A codec that WebM cannot
  stream-copy fails that file and is never transcoded.
- **AVI:** select the AVI muxer and add the unknown-stream handling needed when AVI
  exposes metadata streams that `-dn` cannot classify; no MOV flags are passed.
- **M4V validation:** only ISO-BMFF M4V files are accepted. Raw MPEG-4 elementary
  streams using the `.m4v` extension are rejected before processing.

FFmpeg writes to a temporary name, and the file is renamed into place only after it
exits successfully. An interrupted run can therefore leave a leftover temporary file,
but never a truncated video under a finished name. Leftovers are swept at the start of
the next batch.

More detail — the FFmpeg sidecars, the privacy classifier, the verifier, the ID
registry, stored state — is in
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Limitations

- **Windows x86_64 only** today. The bundled FFmpeg is a Windows binary.
- Supported containers are **MP4, MOV, M4V, MKV, WebM and AVI**.
- A supported container does not guarantee arbitrary codec compatibility.
- There is no transcoding fallback: incompatible stream-copy fails that file.
- Raw MPEG-4 elementary streams using the `.m4v` extension are rejected; M4V support
  is limited to ISO-BMFF files.
- MetaStrip is **not a forensic
  anonymisation tool**, and makes no claim that a cleaned file is unidentifiable:
  encoder characteristics, frame content and the container structure all remain.
- The output container is rewritten, so the file is not byte-identical to the input,
  and the muxer regenerates some technical metadata of its own.
- When processing succeeds, video and audio streams remain encoded as-is.
- The Privacy Scan classifies the metadata `ffprobe` exposes. It does not look inside
  the picture, the sound or opaque payloads.
- Verification checks stream parameters, not per-file packet hashes.

## Roadmap

Nothing is promised, but these are the realistic next steps:

- Additional formats only when they can meet the same regression standard
- Evaluate macOS and Linux builds, which need their own FFmpeg sidecars

## Building from source

```
git clone https://github.com/emilioperna/metastrip-video.git
cd metastrip-video
npm install
npm run setup:ffmpeg
npm run tauri dev
```

You need Node.js, npm and a Rust toolchain (MSVC, x86_64). `npm run setup:ffmpeg`
fetches the pinned FFmpeg binaries that get bundled into the app; they are not stored in
git. To build the installer:

```
npm run tauri build
```

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the architecture, and
[docs/RELEASING.md](docs/RELEASING.md) for how a release is cut.

## Contributing

Bug reports, ideas and pull requests are welcome. Start with
[CONTRIBUTING.md](CONTRIBUTING.md) — it covers the local setup, the tests, and the one
rule worth knowing up front: the FFmpeg pipeline does not change without a very good
reason.

Please do not attach private videos to a public issue.

## Security

Update signing is the security-sensitive part of this project. To report a
vulnerability, see [SECURITY.md](SECURITY.md) — please do not open a public issue for
one.

## FAQ

**Is it free?** Yes, and open source under the MIT License.

**Does it upload my videos?** No. Processing is local; the only network request is the
update check.

**Does it reduce quality?** No. Video and audio are copied, not re-encoded.

**Does it overwrite my originals?** No. Originals are never modified or deleted.

**What does "Verified" mean?** That the cleaned file was inspected again and passed
every check listed in [What "verified" means](#what-verified-means). It does not mean
the file is anonymous.

**Do I need FFmpeg installed?** No, it is bundled with the app.

**Which formats?** MP4, MOV, M4V, MKV, WebM and AVI.

**Does it update itself?** Yes, through signed updates from GitHub Releases.

---

If MetaStrip is useful to you, consider starring the repository — it helps other people
discover it.

## License

MetaStrip Video is open source under the [MIT License](LICENSE).

The bundled FFmpeg is licensed separately, under the LGPL v3+, and is redistributed
unmodified. See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) for versions,
checksums, source links and the reasoning; the FFmpeg licence text is also installed
alongside the application as `FFMPEG-LICENSE.txt`.
