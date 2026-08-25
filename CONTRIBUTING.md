# Contributing

Thanks for looking. MetaStrip Video is a small tool that does one thing, and the goal is
to keep it that way — so the most useful contributions are usually small ones.

## Getting set up

You need Node.js + npm and a Rust toolchain (MSVC, x86_64).

```
git clone https://github.com/emilioperna/metastrip-video.git
cd metastrip-video
npm install
npm run setup:ffmpeg      # fetches the pinned FFmpeg sidecar; not stored in git
npm run tauri dev
```

## Before you open a pull request

```
npm run build
npm test
cd src-tauri && cargo check --all-targets && cargo test
```

All of it should be green. The Rust tests need `npm run setup:ffmpeg` to have run, since
they build real MP4 fixtures with the bundled FFmpeg.

## What makes a good pull request

- **Keep the scope small.** One change per PR, explained in a sentence or two.
- **Match the surrounding code.** Same naming, same comment density, same idiom. Rust is
  `cargo fmt`-clean; do not reformat files you are not otherwise touching.
- **Add a test when the change could regress silently.** Everything in the cleaning path
  has one.
- **Update the docs** if you changed behaviour a user or contributor would notice.

## The FFmpeg pipeline is effectively frozen

```
-map 0 -c copy -map_metadata -1 -map_metadata:s -1 -map_chapters -1 -dn
-fflags +bitexact -movflags +faststart
```

Every one of those arguments is there for a reason, and a test enforces it. Changing
them changes what the product does and what its output can be trusted to contain. If you
believe one should change, open an issue first and make the case — with a file that
demonstrates the problem, if you can share one safely.

The same goes for the ID registry, the atomic-output logic, and the batch sequencing.

## Never commit

- Binaries: `ffmpeg.exe`, installers, build output
- Signing keys, passwords, tokens, `.env` files
- Test videos, screenshots containing personal paths or usernames
- Anything under `target/`, `dist/` or `node_modules/`

If you are adding a screenshot to the docs, check it for your Windows username and your
folder names first.

## Reporting bugs

Use the issue templates. And please:

**Do not upload private or personal videos to a public issue.** If a specific file is
needed to reproduce something, say so and we will find another way.

## Security

Do not report vulnerabilities through public issues. See [SECURITY.md](SECURITY.md).
