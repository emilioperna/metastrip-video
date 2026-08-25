# Security policy

## Reporting a vulnerability

**Please do not open a public issue for a security vulnerability.**

Report it privately through GitHub: go to the
[Security tab](https://github.com/emilioperna/metastrip-video/security) of this
repository and use **Report a vulnerability**. That opens a private advisory visible
only to the maintainers.

Useful things to include:

- What the problem is, and what an attacker could do with it
- The MetaStrip Video version and your Windows version
- Steps to reproduce
- A proof of concept, if you have one

You will get an acknowledgement as soon as the report is read. Since this is a small
project maintained by one person, please allow a reasonable window for a fix before
disclosing publicly.

When you report, do not include private videos, personal file paths, credentials or
tokens — a description is almost always enough, and anything you attach becomes part of
the advisory.

## Supported versions

Only the latest released version is supported. MetaStrip updates itself, so fixes reach
users through a new release rather than through patches to older versions.

## What is security-sensitive here

**Update signing.** MetaStrip installs updates automatically. Each update is signed with
a minisign keypair; the public half is compiled into the app, and an update whose
signature does not verify is never run. The private half is not in this repository and
never has been.

If you believe the signing key has been compromised, or that the update mechanism can be
made to install something unsigned, that is the highest-severity report this project can
receive — please use the private route above.

**The bundled FFmpeg.** The app ships a pinned FFmpeg binary, fetched and SHA-256
verified by `scripts/setup-ffmpeg.ps1`. Vulnerabilities in FFmpeg itself should go to
[the FFmpeg project](https://ffmpeg.org/security.html); tell us as well if a shipped
version needs to be bumped.

**What the app touches.** MetaStrip reads the videos you give it, writes to the folder
you choose, and stores two plain files under `%APPDATA%\com.metastrip.video\`. The only
network request it makes is the update check against GitHub Releases. Anything that
contradicts that description is worth reporting.
