// The three files that carry the app version have to agree: the installer name,
// the updater's "current version" and the tag a release is cut from all come
// from here. A mismatch means the updater compares against the wrong number.
//
//   node scripts/check-version.mjs            check the three agree
//   node scripts/check-version.mjs v0.3.1     also check they match a tag
//
// Exits non-zero on a mismatch, so CI can gate a release on it.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const sources = {
  "package.json": () => JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version,
  "src-tauri/tauri.conf.json": () =>
    JSON.parse(readFileSync(join(root, "src-tauri/tauri.conf.json"), "utf8")).version,
  // Cargo.toml is read with a regex rather than a TOML parser: it is the only
  // value needed, and it keeps this script dependency-free.
  "src-tauri/Cargo.toml": () => {
    const toml = readFileSync(join(root, "src-tauri/Cargo.toml"), "utf8");
    const match = toml.match(/^\s*version\s*=\s*"([^"]+)"/m);
    if (!match) throw new Error("no version key in Cargo.toml");
    return match[1];
  },
};

const found = Object.entries(sources).map(([file, read]) => [file, read()]);
const versions = new Set(found.map(([, v]) => v));

let failed = false;

if (versions.size !== 1) {
  console.error("Version mismatch:");
  for (const [file, version] of found) console.error(`  ${version}  ${file}`);
  failed = true;
} else {
  console.log(`version ${[...versions][0]} consistent across ${found.length} files`);
}

const tag = process.argv[2];
if (tag) {
  const expected = tag.replace(/^v/, "");
  for (const [file, version] of found) {
    if (version !== expected) {
      console.error(`Tag ${tag} does not match ${version} in ${file}`);
      failed = true;
    }
  }
  if (!failed) console.log(`tag ${tag} matches`);
}

process.exit(failed ? 1 : 0);
