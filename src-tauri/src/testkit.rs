//! Synthetic fixtures for the test suite.
//!
//! Every fixture is generated with the bundled FFmpeg at test time. No video is
//! committed: binaries in git age badly, and a generated fixture can be given
//! exactly the metadata a test needs to prove something about.
//!
//! Encoding happens only here, to manufacture inputs. The production cleaning
//! path is always a stream copy.

use std::path::{Path, PathBuf};

use crate::format_profile;
use crate::sidecar::ffmpeg;

/// A throwaway directory under the OS temp dir.
pub fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("video-cleaner-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Payload planted in a fixture's data track. Its absence from an output is the
/// proof that the track itself is gone, not merely its tags.
pub const DATA_CANARY: &[u8] = b"DATA_TRACK_CANARY";

/// Payloads planted in a fixture's attachment streams, for the same reason.
/// Two of them, because a real Matroska carries a set of fonts rather than one,
/// and dropping only the first would otherwise pass. Deliberately not prefixes
/// of one another, so `contains` cannot match one inside the other.
pub const ATTACHMENT_CANARY_ALPHA: &[u8] = b"ATTACHMENT_ALPHA_CANARY";
pub const ATTACHMENT_CANARY_BETA: &[u8] = b"ATTACHMENT_BETA_CANARY";

/// File names the attachments are planted under. Matroska stores the name in a
/// `filename` tag, so it is a second canary that costs nothing.
pub const ATTACHMENT_NAME_ALPHA: &str = "fixture-alpha.ttf";
pub const ATTACHMENT_NAME_BETA: &str = "fixture-beta.otf";

/// Body of the fixture subtitle track. Subtitles are media: the cleaner keeps
/// them by default, so this must still be there afterwards -- and must be gone
/// when subtitle removal was asked for.
pub const SUBTITLE_BODY: &str = "SUBTITLE_BODY_KEPT";

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A tiny real MP4, so the FFmpeg path is exercised for real.
pub fn sample_video(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let status = ffmpeg()
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=10:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=1",
            "-c:v",
            "mpeg4",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-metadata",
            "title=SECRET",
        ])
        .arg(&path)
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        status.status.success(),
        "could not build the sample video: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    path
}

/// A sample that really carries a data stream, which is what `-dn` has to drop.
///
/// The mp4 muxer refuses a bare `bin_data` track, so the only way to get one out
/// of a stock LGPL FFmpeg is to let `-map_chapters` build a chapter text track.
/// The demuxer hands those samples back only when the chapters stop short of the
/// media, hence a 7 s clip whose chapters end at 7000 ms: flush against the end
/// the track reads back empty and the fixture would prove nothing.
pub fn sample_video_with_data_track(dir: &Path, name: &str) -> PathBuf {
    let base = dir.join("base-for-data-track.mp4");
    let built = ffmpeg()
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=10:duration=7",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=7",
            "-c:v",
            "mpeg4",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ])
        .arg(&base)
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        built.status.success(),
        "could not build the base clip: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let chapters = dir.join("chapters.ffmetadata");
    let script = [
        ";FFMETADATA1",
        "[CHAPTER]",
        "TIMEBASE=1/1000",
        "START=0",
        "END=3000",
        "title=opening",
        "[CHAPTER]",
        "TIMEBASE=1/1000",
        "START=3000",
        "END=7000",
        "title=DATA_TRACK_CANARY",
    ]
    .join("\n");
    std::fs::write(&chapters, script).unwrap();

    let path = dir.join(name);
    let built = ffmpeg()
        .args(["-y", "-i"])
        .arg(&base)
        .arg("-i")
        .arg(&chapters)
        .args([
            "-map",
            "0",
            "-map_chapters",
            "1",
            "-c",
            "copy",
            "-metadata",
            "title=SECRET",
        ])
        .arg(&path)
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        built.status.success(),
        "could not build the fixture: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    path
}

/// The stream kinds FFmpeg reports for a file, in order, e.g. `["Video", "Audio"]`.
///
/// Reads FFmpeg's human-readable report on purpose: this is a test assertion
/// about what FFmpeg itself sees, deliberately independent of the ffprobe JSON
/// the production inspector parses. If both agreed only because they shared a
/// parser, the format tests would prove much less.
pub fn stream_kinds(path: &Path) -> Vec<String> {
    // No output file, so FFmpeg exits non-zero after printing the input report.
    let probe = ffmpeg()
        .arg("-i")
        .arg(path)
        .output()
        .expect("ffmpeg must be available for these tests");
    String::from_utf8_lossy(&probe.stderr)
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("Stream #0:"))
        .filter_map(|line| {
            [
                "Video",
                "Audio",
                "Data",
                "Unknown",
                "Subtitle",
                "Attachment",
            ]
            .into_iter()
            .find(|kind| line.contains(&format!(": {kind}: ")))
            .map(str::to_string)
        })
        .collect()
}

#[derive(Clone, Copy)]
pub struct FixtureCapabilities {
    pub video_codec: &'static str,
    pub audio_codec: &'static str,
    pub chapters: bool,
    /// The kind FFmpeg reports for the non-media stream this fixture plants, or
    /// `None` where the container cannot carry one. Named for what
    /// `non_media_streams` actually means -- Data, Attachment or Unknown --
    /// rather than for data alone, which is how an attachment-bearing MKV came
    /// to have no fixture at all.
    pub non_media_stream_kind: Option<&'static str>,
    /// Subtitle codec this container accepts, where it accepts one. Subtitles
    /// are kept by default, so a fixture carrying one proves preservation, and
    /// the same fixture proves removal when the option is on.
    pub subtitle_codec: Option<&'static str>,
}

impl FixtureCapabilities {
    /// Whether this fixture plants attachment streams, which only Matroska can
    /// carry: the ISO-BMFF and AVI muxers refuse them outright, and the WebM
    /// muxer accepts the argument and then silently writes no attachment, which
    /// would leave an assertion passing against a stream that is not there.
    pub fn attaches(self) -> bool {
        self.non_media_stream_kind == Some("Attachment")
    }
}

/// What each container can actually carry. Pretending a muxer supports metadata
/// it does not would make a test that passes for the wrong reason.
pub fn fixture_capabilities(extension: &str) -> FixtureCapabilities {
    match extension {
        "mp4" | "mov" | "m4v" => FixtureCapabilities {
            video_codec: "mpeg4",
            audio_codec: "aac",
            chapters: true,
            // FFmpeg represents ISO-BMFF chapters with a timed text/data track.
            non_media_stream_kind: Some("Data"),
            subtitle_codec: Some("mov_text"),
        },
        "mkv" => FixtureCapabilities {
            video_codec: "mpeg4",
            audio_codec: "aac",
            chapters: true,
            // The only supported container that can carry an attachment.
            non_media_stream_kind: Some("Attachment"),
            subtitle_codec: Some("srt"),
        },
        "webm" => FixtureCapabilities {
            video_codec: "libvpx",
            audio_codec: "libopus",
            chapters: true,
            // WebM has no attachments element and no data streams.
            non_media_stream_kind: None,
            subtitle_codec: Some("webvtt"),
        },
        "avi" => FixtureCapabilities {
            video_codec: "mpeg4",
            audio_codec: "libmp3lame",
            // FFmpeg's AVI muxer does not write chapters. It can carry a data
            // stream, but reports it as `Unknown: none` when demuxing, and it
            // takes no subtitles.
            chapters: false,
            non_media_stream_kind: Some("Unknown"),
            subtitle_codec: None,
        },
        other => panic!("missing fixture capabilities for {other}"),
    }
}

/// Privacy canaries planted in each container-specific fixture, beyond the
/// `title` and `comment` every muxer here accepts.
///
/// Per extension rather than per container family, because muxers in the same
/// family disagree: the MOV muxer writes `make`/`model`, the MP4 muxer silently
/// drops them, and AVI's INFO chunks carry neither those nor a location. Planting
/// a tag the muxer discards would leave a test asserting against something that
/// was never in the file.
fn privacy_canaries(extension: &str) -> Vec<(&'static str, &'static str)> {
    let creator = [
        ("artist", "CREATOR_SECRET"),
        ("copyright", "COPYRIGHT_SECRET"),
    ];
    let device = [("make", "CanaryPhone"), ("model", "CanaryModel X")];
    let location = [("location", "+45.4642+009.1900/")];

    let mut tags: Vec<(&'static str, &'static str)> = creator.to_vec();
    match extension {
        // The MP4 muxer keeps a location but drops make/model.
        "mp4" | "m4v" => tags.extend(location),
        "mov" | "mkv" | "webm" => {
            tags.extend(device);
            tags.extend(location);
        }
        // AVI carries only the RIFF INFO set: title, comment, artist, copyright.
        "avi" => {}
        other => panic!("missing canary set for {other}"),
    }
    tags
}

/// The privacy categories a fixture for this container must disclose before it
/// is cleaned. Asserted by the tests so a fixture that quietly loses a canary
/// fails loudly instead of making the cleaning assertions trivially true.
pub fn expected_categories(extension: &str) -> Vec<&'static str> {
    let mut categories = vec!["Creator", "Copyright"];
    match extension {
        "mp4" | "m4v" => categories.push("Location"),
        "mov" | "mkv" | "webm" => {
            categories.push("Device");
            categories.push("Location");
        }
        "avi" => {}
        other => panic!("missing expected categories for {other}"),
    }
    categories
}

/// Builds a real, container-appropriate fixture with global metadata, privacy
/// canaries and, wherever the muxer supports them, chapters.
pub fn sample_for_format(dir: &Path, extension: &str) -> PathBuf {
    let profile = format_profile(extension).expect("fixture requested for unsupported format");
    let capabilities = fixture_capabilities(extension);
    let metadata_path = dir.join(format!("fixture-{extension}.ffmetadata"));

    let mut metadata =
        String::from(";FFMETADATA1\ntitle=GLOBAL_SECRET\ncomment=SENSITIVE_COMMENT\n");
    for (key, value) in privacy_canaries(extension) {
        metadata.push_str(&format!("{key}={value}\n"));
    }
    if capabilities.chapters {
        metadata.push_str("[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=1200\ntitle=CHAPTER_SECRET\n");
    }
    std::fs::write(&metadata_path, metadata).unwrap();

    let data_path = dir.join("avi-data-canary.bin");
    if extension == "avi" {
        std::fs::write(&data_path, DATA_CANARY).unwrap();
    }

    let subtitle_path = dir.join(format!("fixture-{extension}.srt"));
    if capabilities.subtitle_codec.is_some() {
        std::fs::write(
            &subtitle_path,
            format!("1\n00:00:00,000 --> 00:00:01,500\n{SUBTITLE_BODY}\n\n"),
        )
        .unwrap();
    }

    let attachment_paths: Vec<PathBuf> = if capabilities.attaches() {
        [
            (ATTACHMENT_NAME_ALPHA, ATTACHMENT_CANARY_ALPHA),
            (ATTACHMENT_NAME_BETA, ATTACHMENT_CANARY_BETA),
        ]
        .iter()
        .map(|(name, payload)| {
            let attachment = dir.join(name);
            std::fs::write(&attachment, payload).unwrap();
            attachment
        })
        .collect()
    } else {
        Vec::new()
    };

    let path = dir.join(format!("fixture.{extension}"));
    let mut command = ffmpeg();
    command
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=10:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-f",
            "ffmetadata",
            "-i",
        ])
        .arg(&metadata_path);
    // Inputs after the metadata one. Subtitles and the AVI data track never
    // coexist, so both take index 3.
    let subtitle_input = capabilities.subtitle_codec.map(|_| {
        command.arg("-i").arg(&subtitle_path);
        3
    });
    let data_input = (extension == "avi").then(|| {
        command.args(["-f", "data", "-i"]).arg(&data_path);
        3
    });
    command.args(["-map", "0:v:0", "-map", "1:a:0", "-map_metadata", "2"]);
    if let Some(index) = subtitle_input {
        command.args(["-map", &format!("{index}:s:0")]);
    }
    if let Some(index) = data_input {
        command.args(["-map", &format!("{index}:0")]);
    }
    if capabilities.chapters {
        command.args(["-map_chapters", "2"]);
    } else {
        command.args(["-map_chapters", "-1"]);
    }
    command.args([
        "-metadata:s:v:0",
        "title=STREAM_SECRET",
        "-t",
        "2",
        "-c:v",
        capabilities.video_codec,
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        capabilities.audio_codec,
    ]);
    if let Some(codec) = capabilities.subtitle_codec {
        command.args(["-c:s", codec]);
    }
    if extension == "avi" {
        command.args(["-c:d", "copy"]);
    }
    if extension == "webm" {
        command.args(["-deadline", "realtime", "-cpu-used", "8"]);
    }
    for (index, attachment) in attachment_paths.iter().enumerate() {
        command.arg("-attach").arg(attachment);
        // Matroska refuses to write an attachment whose mimetype it cannot work
        // out, so without this the fixture itself would not build.
        command.args([
            format!("-metadata:s:t:{index}"),
            "mimetype=application/x-truetype-font".to_string(),
        ]);
    }
    let built = command
        .args(["-f", profile.output_muxer])
        .arg(&path)
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        built.status.success(),
        "could not build the {extension} fixture: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    path
}

pub fn ffmetadata(path: &Path) -> String {
    let probe = ffmpeg()
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "ffmetadata", "-"])
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        probe.status.success(),
        "could not inspect metadata in {path:?}: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
    String::from_utf8_lossy(&probe.stdout).to_lowercase()
}

pub fn assert_ffmpeg_can_read(path: &Path) {
    let probe = ffmpeg()
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map", "0:v:0?", "-map", "0:a:0?", "-c", "copy", "-f", "null", "-",
        ])
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        probe.status.success(),
        "FFmpeg could not read {path:?}: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
}

/// The md5 of one stream's encoded packet payload.
///
/// This is the packet-identity check behind the no-re-encoding claim: it reads
/// the whole file, which is exactly why the runtime verifier does not do it (it
/// compares stream parameters only) and the test suite does. See the module
/// comment in `verify.rs`.
pub fn stream_payload_hash(path: &Path, stream: &str) -> String {
    let hash = ffmpeg()
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", stream, "-c", "copy", "-f", "md5", "-"])
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        hash.status.success(),
        "could not hash {stream} in {path:?}: {}",
        String::from_utf8_lossy(&hash.stderr)
    );
    let value = String::from_utf8_lossy(&hash.stdout).trim().to_string();
    assert!(!value.is_empty(), "empty {stream} hash for {path:?}");
    value
}

// ------------------------------------------------------------- Cover art ---

/// Text a fixture cover image carries in its own EXIF block. A phone photo
/// used as a cover carries its camera and its location the same way, inside
/// the picture rather than in any container tag.
pub const EXIF_DESCRIPTION_CANARY: &str = "EXIF_DESCRIPTION_CANARY";
pub const EXIF_MAKE_CANARY: &str = "EXIF_MAKE_CANARY";

/// A TIFF-structured EXIF block, little-endian: IFD0 with an image
/// description, a camera make and a pointer to a GPS IFD holding a latitude
/// of 45 deg 27 min 51.23 sec N. Laid out by the EXIF 2.3 rules -- word-aligned
/// values, offsets from the TIFF header -- so FFmpeg's own EXIF reader parses it.
fn exif_block() -> Vec<u8> {
    fn entry(tag: u16, kind: u16, count: u32, value: u32) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(12);
        bytes.extend(tag.to_le_bytes());
        bytes.extend(kind.to_le_bytes());
        bytes.extend(count.to_le_bytes());
        bytes.extend(value.to_le_bytes());
        bytes
    }
    fn ascii(text: &str) -> Vec<u8> {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        if bytes.len() % 2 == 1 {
            bytes.push(0);
        }
        bytes
    }
    const ASCII: u16 = 2;
    const LONG: u16 = 4;
    const RATIONAL: u16 = 5;

    let description = ascii(EXIF_DESCRIPTION_CANARY);
    let make = ascii(EXIF_MAKE_CANARY);
    let ifd0 = 8u32;
    let description_at = ifd0 + 2 + 12 * 3 + 4;
    let make_at = description_at + description.len() as u32;
    let gps_at = make_at + make.len() as u32;
    let latitude_at = gps_at + 2 + 12 * 2 + 4;

    let mut tiff = b"II*\0".to_vec();
    tiff.extend(ifd0.to_le_bytes());
    tiff.extend(3u16.to_le_bytes());
    tiff.extend(entry(
        0x010E,
        ASCII,
        EXIF_DESCRIPTION_CANARY.len() as u32 + 1,
        description_at,
    ));
    tiff.extend(entry(
        0x010F,
        ASCII,
        EXIF_MAKE_CANARY.len() as u32 + 1,
        make_at,
    ));
    tiff.extend(entry(0x8825, LONG, 1, gps_at));
    tiff.extend(0u32.to_le_bytes());
    tiff.extend(description);
    tiff.extend(make);
    tiff.extend(2u16.to_le_bytes());
    // GPSLatitudeRef "N": two bytes, stored inline in the value field.
    tiff.extend(0x0001u16.to_le_bytes());
    tiff.extend(ASCII.to_le_bytes());
    tiff.extend(2u32.to_le_bytes());
    tiff.extend(*b"N\0\0\0");
    tiff.extend(entry(0x0002, RATIONAL, 3, latitude_at));
    tiff.extend(0u32.to_le_bytes());
    for value in [45u32, 1, 27, 1, 5123, 100] {
        tiff.extend(value.to_le_bytes());
    }
    tiff
}

/// A real JPEG made by FFmpeg, optionally carrying an EXIF block in the APP1
/// segment right after SOI, which is where cameras put it. Nothing else in the
/// file changes, and FFmpeg decodes it and reads its EXIF back (asserted by the
/// tests that use it), so this is a valid photo rather than bytes a lenient
/// parser happens to accept.
pub fn cover_jpeg(dir: &Path, with_exif: bool) -> PathBuf {
    let plain = dir.join("cover-plain.jpg");
    let built = ffmpeg()
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=orange:s=64x64",
            "-frames:v",
            "1",
            "-c:v",
            "mjpeg",
            "-pix_fmt",
            "yuvj420p",
            "-f",
            "image2",
        ])
        .arg(&plain)
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        built.status.success(),
        "could not build the cover image: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    if !with_exif {
        return plain;
    }

    let jpeg = std::fs::read(&plain).unwrap();
    assert_eq!(jpeg[..2], [0xFF, 0xD8], "FFmpeg did not write a JPEG");
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend(exif_block());
    let length = u16::try_from(payload.len() + 2).unwrap();
    let mut photo = jpeg[..2].to_vec();
    photo.extend([0xFF, 0xE1]);
    photo.extend(length.to_be_bytes());
    photo.extend(payload);
    photo.extend(&jpeg[2..]);

    let path = dir.join("cover-exif.jpg");
    std::fs::write(&path, photo).unwrap();
    path
}

/// The EXIF FFmpeg itself reads out of an image, one `key=value` per line.
/// Empty when it finds none.
pub fn exif_seen_by_ffprobe(image: &Path) -> String {
    let probe = crate::sidecar::ffprobe()
        .args([
            "-v",
            "error",
            "-show_frames",
            "-show_entries",
            "frame_tags",
            "-of",
            "flat",
        ])
        .arg(image)
        .output()
        .expect("ffprobe must be available for these tests");
    assert!(
        probe.status.success() && probe.stderr.is_empty(),
        "ffprobe could not read {image:?} cleanly: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
    String::from_utf8_lossy(&probe.stdout).into_owned()
}

/// The container-specific fixture with `cover` attached as cover art, the way
/// each container exposes one to FFmpeg as `disposition.attached_pic`:
///
/// * MP4 and M4V: an iTunes-style `covr` image.
/// * MOV: the same ISO-BMFF `covr` layout under a `.mov` name. FFmpeg's `mov`
///   muxer cannot write a cover, so this is the MOV case users actually meet:
///   a file from another tool that FFmpeg reads a cover out of.
/// * MKV: an image attachment with an image mimetype, which FFmpeg demuxes as
///   an attached picture rather than as an attachment.
///
/// Everything else the base fixture carries -- metadata, chapters, subtitles,
/// fonts -- comes along, so the cover is removed from a realistic file.
pub fn sample_with_cover(dir: &Path, extension: &str, cover: &Path) -> PathBuf {
    let path = dir.join(format!("fixture-cover.{extension}"));
    let mut command = ffmpeg();
    command.arg("-y");
    match extension {
        "mp4" | "m4v" | "mov" => {
            let base = sample_for_format(dir, if extension == "m4v" { "m4v" } else { "mp4" });
            // The chapter text track is regenerated from the chapters, and the
            // muxer refuses a bare copy of it, hence `-dn`.
            command.arg("-i").arg(&base).arg("-i").arg(cover).args([
                "-map",
                "0",
                "-dn",
                "-map",
                "1",
                "-c",
                "copy",
                "-disposition:v:1",
                "attached_pic",
                "-f",
                "mp4",
            ]);
        }
        "mkv" => {
            let base = sample_for_format(dir, "mkv");
            // The base fixture already carries two font attachments, so the
            // cover is the third attachment stream.
            command
                .arg("-i")
                .arg(&base)
                .args(["-map", "0", "-c", "copy", "-attach"])
                .arg(cover)
                .args([
                    "-metadata:s:t:2",
                    "mimetype=image/jpeg",
                    "-metadata:s:t:2",
                    "filename=cover.jpg",
                    "-f",
                    "matroska",
                ]);
        }
        other => panic!("no cover art fixture for {other}"),
    }
    let built = command
        .arg(&path)
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        built.status.success(),
        "could not build the {extension} cover fixture: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    path
}

/// Two genuine camera angles and one audio track: the second video stream is
/// footage, not a cover, and nothing may treat it as removable.
pub fn sample_with_two_videos(dir: &Path) -> PathBuf {
    let path = dir.join("two-angles.mp4");
    let built = ffmpeg()
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=10:duration=2",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x64:rate=10:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-map",
            "0:v",
            "-map",
            "1:v",
            "-map",
            "2:a",
            "-c:v",
            "mpeg4",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-metadata",
            "title=SECRET",
        ])
        .arg(&path)
        .output()
        .expect("ffmpeg must be available for these tests");
    assert!(
        built.status.success(),
        "could not build the two-angle fixture: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    path
}
