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
/// them, so this must still be there afterwards.
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
    /// are kept by the cleaner, so a fixture carrying one proves preservation
    /// rather than removal.
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
