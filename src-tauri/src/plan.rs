//! What a cleaned file must look like, stated before it is cleaned.
//!
//! Two things live here and they are kept apart on purpose.
//!
//! * [`CleaningOptions`] is what the user chose. It drives the FFmpeg argument
//!   list directly (`ffmpeg_args`), so a file that cannot be inspected is still
//!   cleaned exactly as asked, with no plan at all.
//! * [`CleaningPlan`] is the verifier's contract for one inspected file: which
//!   media streams must come out the other side, in which order and with which
//!   parameters, and whether subtitle removal has to be proven. It is derived
//!   from the fresh pre-clean report and the options for the run, and nothing
//!   reads it but `verify`.
//!
//! The privacy floor -- metadata, chapters, non-media tracks and detected cover
//! art -- is not an option and has no field here. The verifier checks it on
//! every file, whatever the plan says.

use serde::{Deserialize, Serialize};

use crate::inspect::{MetadataReport, StreamIdentity, StreamKind};

/// The one cleaning choice the user can make. Everything else is the floor.
///
/// `default` at the struct level so a stored value written by a later version
/// with more fields still yields this one rather than failing as a whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CleaningOptions {
    /// Also drop every subtitle stream. Off by default: subtitles are media the
    /// user usually wants, and removing them is destructive.
    pub remove_subtitles: bool,
}

/// One media stream the clean promises to keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedStream {
    /// Position in the input, for messages. Output indices shift whenever
    /// something before a stream is removed, so this is never matched against
    /// an output index.
    pub input_index: usize,
    pub kind: StreamKind,
    pub identity: StreamIdentity,
    /// Always `false` in practice, since detected cover art is removed, but
    /// compared anyway: a kept stream must not come out claiming to be one.
    pub attached_pic: bool,
}

#[derive(Clone, Debug)]
pub struct CleaningPlan {
    /// Whether the output must carry no subtitle stream at all.
    pub remove_subtitles: bool,
    /// Every media stream that must survive, in input order: the input's media
    /// minus detected cover art, minus subtitles when they are being removed.
    pub expected_media: Vec<ExpectedStream>,
}

/// The verifier contract for one inspected file and one set of options.
pub fn plan_for(report: &MetadataReport, options: CleaningOptions) -> CleaningPlan {
    let expected_media = report
        .media_streams()
        .filter(|stream| !stream.attached_pic)
        .filter(|stream| !(options.remove_subtitles && stream.kind == StreamKind::Subtitle))
        .map(|stream| ExpectedStream {
            input_index: stream.index,
            kind: stream.kind,
            identity: stream.identity.clone(),
            attached_pic: stream.attached_pic,
        })
        .collect();

    CleaningPlan {
        remove_subtitles: options.remove_subtitles,
        expected_media,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::parse_ffprobe_json;

    /// Video, a cover, audio, two subtitles and a data track, in an order that
    /// puts the cover between real streams.
    const MIXED: &str = r#"{"streams":[
        {"index":0,"codec_type":"video","codec_name":"h264","width":1920,"disposition":{"attached_pic":0}},
        {"index":1,"codec_type":"video","codec_name":"mjpeg","width":300,"disposition":{"attached_pic":1}},
        {"index":2,"codec_type":"audio","codec_name":"aac","sample_rate":"48000"},
        {"index":3,"codec_type":"subtitle","codec_name":"mov_text"},
        {"index":4,"codec_type":"subtitle","codec_name":"mov_text"},
        {"index":5,"codec_type":"data","codec_name":"bin_data"}
    ]}"#;

    fn indices(plan: &CleaningPlan) -> Vec<usize> {
        plan.expected_media.iter().map(|s| s.input_index).collect()
    }

    #[test]
    fn the_default_keeps_every_media_stream_but_detected_cover_art() {
        let report = parse_ffprobe_json("x.mp4", MIXED).unwrap();
        let plan = plan_for(&report, CleaningOptions::default());

        assert!(!plan.remove_subtitles);
        // Cover (1) and data (5) are not expected; order is the input's.
        assert_eq!(indices(&plan), [0, 2, 3, 4]);
        assert!(plan.expected_media.iter().all(|s| !s.attached_pic));
        assert_eq!(
            plan.expected_media[0].identity.codec_name.as_deref(),
            Some("h264")
        );
    }

    #[test]
    fn removing_subtitles_takes_them_out_of_the_expected_media() {
        let report = parse_ffprobe_json("x.mp4", MIXED).unwrap();
        let plan = plan_for(
            &report,
            CleaningOptions {
                remove_subtitles: true,
            },
        );

        assert!(plan.remove_subtitles);
        assert_eq!(indices(&plan), [0, 2]);
    }

    #[test]
    fn a_second_real_video_stream_is_expected_to_survive() {
        let two_angles = r#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264"},
            {"index":1,"codec_type":"video","codec_name":"h264"},
            {"index":2,"codec_type":"audio","codec_name":"aac"}
        ]}"#;
        let report = parse_ffprobe_json("x.mov", two_angles).unwrap();
        let plan = plan_for(&report, CleaningOptions::default());
        assert_eq!(indices(&plan), [0, 1, 2]);
    }

    #[test]
    fn options_serialise_in_camel_case_and_default_to_keeping_subtitles() {
        assert!(!CleaningOptions::default().remove_subtitles);
        let json = serde_json::to_string(&CleaningOptions {
            remove_subtitles: true,
        })
        .unwrap();
        assert_eq!(json, r#"{"removeSubtitles":true}"#);
        let parsed: CleaningOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, CleaningOptions::default());
    }
}
