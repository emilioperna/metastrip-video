# Third-party notices

Aurevm Video Cleaner redistributes and reuses the components listed below.

---

## FFmpeg

`ffmpeg.exe` is shipped inside the application folder and is executed as a separate
process, invoked with command-line arguments. It is not linked into
`aurevm-video-cleaner.exe` in any form — no headers, no libraries, no shared address
space.

- **Project:** FFmpeg — https://ffmpeg.org
- **Version shipped:** `n8.1.2-44-g7c533d0f86` (x86_64, Windows)
- **License:** GNU Lesser General Public License, version 3 or later (LGPL v3+).
  Full text: `FFMPEG-LICENSE.txt`, installed next to the application.
- **Build used:** the prebuilt `win64-lgpl` binary from the BtbN/FFmpeg-Builds
  project, pinned to an immutable release tag:
  https://github.com/BtbN/FFmpeg-Builds/releases/tag/autobuild-2026-08-24-13-10
  (`ffmpeg-n8.1.2-44-g7c533d0f86-win64-lgpl-8.1.zip`)
- **SHA-256 of the shipped `ffmpeg.exe`:**
  `5346a1daac36a23b4797e33e5c15e0d477e88cbd24b947f288c8607df89cb850`
  — verified by `scripts/setup-ffmpeg.ps1` on every fetch.
- **Modifications:** none. The executable is redistributed byte-for-byte as published
  upstream.
- **Source code:** https://github.com/FFmpeg/FFmpeg at the tag matching the version
  above, and https://ffmpeg.org/download.html

### Why the LGPL build

FFmpeg's default configuration is LGPL; passing `--enable-gpl` (which pulls in x264,
x265, xvid and friends) makes the resulting binary GPL. This build has `--enable-gpl`
off, so no GPL-only component is present.

The choice is about keeping compliance simple, not about avoiding a licence that would
otherwise be forced onto Aurevm Video Cleaner. FFmpeg runs here as a separate program
communicating over `exec` and command-line arguments, which the GNU licence FAQ
normally treats as separate programs rather than one combined work
(https://www.gnu.org/licenses/gpl-faq.html#MereAggregation). Shipping a GPL build
alongside the app would not by itself make the app GPL. The LGPL build simply avoids
having to make that argument at all, and costs nothing: the app only ever runs FFmpeg
with `-c copy`, so no encoder is needed.

The shipped `ffmpeg.exe` is a static build — that describes how the FFmpeg project's
own `libav*` libraries are linked into its own command-line tool, both sides being
FFmpeg's LGPL code. It does not mean FFmpeg is statically linked into Aurevm. The
practical obligation for redistribution is what is already satisfied above: state the
version, keep the binary unmodified, ship the licence text, and point at the
corresponding sources.

*None of this is legal advice.*

---

## Simple-Metadata-Remover

The FFmpeg argument pipeline used by this app (`-map 0 -c copy -map_metadata -1
-map_chapters -1 -fflags +bitexact`) was taken as a technical reference from:

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
and this one is Rust/React. Aurevm Video Cleaner additionally passes
`-map_metadata:s -1` (stream-level metadata) and `-movflags +faststart`.

---

## Application dependencies

A full transitive licence inventory has **not** been generated yet, so no claim is
made here about what every dependency ships under. Before making this repository or
a binary public, produce one and check it:

```
cargo install cargo-license && cargo license --manifest-path src-tauri/Cargo.toml
npx license-checker-rspack --production --summary
```

The direct dependencies are Tauri 2, its dialog and opener plugins, `serde`,
`serde_json`, `rand`, React and Vite.
