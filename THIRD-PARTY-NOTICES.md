# Third-party notices

MetaStrip Video redistributes and reuses the components listed below.

---

## FFmpeg

`ffmpeg.exe` and `ffprobe.exe` are shipped inside the application folder and are
executed as separate processes, invoked with command-line arguments. Neither is
linked into `metastrip-video.exe` in any form — no headers, no libraries, no shared
address space.

- **Project:** FFmpeg — https://ffmpeg.org
- **Version shipped:** `n8.1.2-50-g1a748fe2cd` (x86_64, Windows)
- **License:** GNU Lesser General Public License, version 3 or later (LGPL v3+).
  Full text: `FFMPEG-LICENSE.txt`, installed next to the application.
- **Build used:** the prebuilt `win64-lgpl` binaries from the BtbN/FFmpeg-Builds
  project, pinned to an immutable release tag:
  https://github.com/BtbN/FFmpeg-Builds/releases/tag/autobuild-2026-08-31-13-27
  (`ffmpeg-n8.1.2-50-g1a748fe2cd-win64-lgpl-8.1.zip`)
- **SHA-256 of the pinned archive:**
  `f6274bbd9c247f9e90c1bbed066b03ed4a3907cece2fb91be6dd352393936365`
- **SHA-256 of the shipped `ffmpeg.exe`:**
  `9c60da6c0b083110d59084ea39f60ae149aa3e031c3b4bb4f573fafa1c1e7cea`
- **SHA-256 of the shipped `ffprobe.exe`:**
  `67176fa62f89f94c3bcd379fd05677a25651569a2eb8880ec2194e62c82be412`
  — all three verified by `scripts/setup-ffmpeg.ps1` on every fetch. Both executables
  come out of that one archive, so they are always the same FFmpeg build.
- **Modifications:** none. The executables are redistributed byte-for-byte as published
  upstream.
- **Source code:** https://github.com/FFmpeg/FFmpeg at the tag matching the version
  above, and https://ffmpeg.org/download.html

### Why the LGPL build

FFmpeg's default configuration is LGPL; passing `--enable-gpl` (which pulls in x264,
x265, xvid and friends) makes the resulting binary GPL. This build has `--enable-gpl`
off, so no GPL-only component is present.

The choice is about keeping compliance simple, not about avoiding a licence that would
otherwise be forced onto MetaStrip Video. FFmpeg runs here as a separate program
communicating over `exec` and command-line arguments, which the GNU licence FAQ
normally treats as separate programs rather than one combined work
(https://www.gnu.org/licenses/gpl-faq.html#MereAggregation). Shipping a GPL build
alongside the app would not by itself make the app GPL. The LGPL build simply avoids
having to make that argument at all, and costs nothing: the app only ever runs FFmpeg
with `-c copy`, so no encoder is needed.

The shipped executables are static builds — that describes how the FFmpeg project's
own `libav*` libraries are linked into its own command-line tools, both sides being
FFmpeg's LGPL code. It does not mean FFmpeg is statically linked into MetaStrip Video. The
practical obligation for redistribution is what is already satisfied above: state the
version, keep the binaries unmodified, ship the licence text, and point at the
corresponding sources.

*None of this is legal advice.*

---

## Simple-Metadata-Remover

The core of the FFmpeg argument pipeline used by this app (`-map 0 -c copy
-map_metadata -1 -map_chapters -1 -fflags +bitexact`) was taken as a technical
reference from:

- **Project:** Simple-Metadata-Remover — https://github.com/ozp3/Simple-Metadata-Remover
- **License:** MIT

```
MIT License

Copyright (c) 2026 ozp3

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

No source code from that project was copied; it is a Python/Tkinter application
and this one is Rust/React. MetaStrip Video additionally passes
`-map_metadata:s -1` (stream-level metadata), `-dn` (drop data tracks) and
`-movflags +faststart`.

---

## Application dependencies

MetaStrip Video's own direct dependencies are Tauri 2 with its dialog, opener and
updater plugins, `serde`, `serde_json` and `rand` on the Rust side, and React with Vite
on the front end.

A licence survey of the full dependency graph was run against `src-tauri/Cargo.lock` and
the npm production tree on 2026-08-25:

- **Rust:** 522 crates in the lock file, 463 of which resolved to a licence from the
  local registry. Every one is permissive — MIT, Apache-2.0 or a dual/multi licence
  including one of them — apart from five crates under **MPL-2.0** (`cssparser`,
  `cssparser-macros`, `dtoa-short`, `option-ext`, `selectors`), a file-level copyleft
  that is satisfied by using them unmodified. No GPL-licensed crate is present. The 59
  unresolved crates are platform-specific packages that are not compiled into the
  Windows build and were therefore not in the local registry cache.
- **npm (production):** 6 packages in total, all MIT or `Apache-2.0 OR MIT`.

This survey is a snapshot, not a generated manifest, and it does not reproduce each
licence text. To regenerate it:

```
cargo install cargo-license && cargo license --manifest-path src-tauri/Cargo.toml
npx license-checker-rspack --production --summary
```
