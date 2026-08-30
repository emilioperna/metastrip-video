# MetaStrip Video — Video Metadata Remover

> Fast, offline video metadata remover for Windows.
> Strip metadata, chapters and data tracks without re-encoding.
>
> **Supported:** MP4 · MOV · M4V · MKV · WebM · AVI

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

Drop up to 100 videos, press one button, get clean copies.

- **Your videos never leave your computer.** No upload, no cloud, no server.
- **No account**, no sign-in.
- **No telemetry**, no analytics, no crash reporting.
- **No video or audio re-encoding.** The streams are copied, bit for bit.
- **Your originals are never modified** or deleted.
- **Signed automatic updates**, verified against a key compiled into the app.
- **FFmpeg is bundled** — nothing else to install.

## Why MetaStrip?

Every video you export carries more than picture and sound. A phone stamps in the
camera model and the time. An editor leaves its name. A GoPro writes a whole GPS
telemetry track. Publish the file and you publish all of it.

Most tools that strip this re-encode the video to do it, which costs quality and time.
MetaStrip copies the media streams across bit for bit and rebuilds the container without
the metadata. Because the streams are copied rather than transcoded, cleaning is
typically much faster than re-encoding, and the result looks exactly like the original.

It is deliberately small: one window, one button, no project files and no settings page.
And it is inspectable — the FFmpeg strategy is documented
[in this README](#how-it-works) and in
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md), and the source is here to read.

## Features

- Batch up to **100 videos** at a time
- **MP4, MOV, M4V, MKV, WebM and AVI**
- Removes global metadata
- Removes per-stream metadata
- Removes chapters where the container exposes them
- Removes data and unknown metadata streams where the container exposes them
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
3. Drop your videos onto the window, or press **Select videos**.
4. Press **Clean**.
5. The cleaned copies are in the folder you chose.

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

Data tracks are worth calling out: stripping the tags around such a track leaves the
track itself, payload and all. MetaStrip drops those tracks when the container exposes
them as data or unknown metadata streams.

## What stays untouched?

- **The video bitstream.** Copied, not re-encoded — byte-identical to the source.
- **The audio bitstream.** Same.
- **Your original files.** Never modified, never deleted, never moved.
- **Picture quality.** There is no quality setting because nothing is re-compressed.

The container itself is rewritten by FFmpeg, so the output file is not a byte-for-byte
copy of the input — the media inside it is.

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

For each file, MetaStrip invokes the bundled FFmpeg with this common stream-copy core:

```
ffmpeg -n -i INPUT \
  -map 0 -c copy \
  -map_metadata -1 \
  -map_metadata:s -1 \
  -map_chapters -1 \
  -dn \
  -fflags +bitexact \
  ...container-specific options... \
  OUTPUT
```

- `-c copy` copies the encoded streams instead of re-encoding them. This is why the
  media is bit-identical, and why cleaning is typically much faster than transcoding.
- `-map_metadata -1 -map_metadata:s -1 -map_chapters -1` drop file-level metadata,
  per-stream metadata and chapters.
- `-dn` drops data tracks. `-map 0` would otherwise copy them, and a data track is
  metadata in its own right.
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

More detail — the FFmpeg sidecar, the ID registry, stored state — is in
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
- The output container is rewritten, so the file is not byte-identical to the input.
- When processing succeeds, video and audio streams remain encoded as-is.

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
fetches the pinned FFmpeg binary that gets bundled into the app; it is not stored in
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
