//! Scoring found beats against labelled ones, as `musicable analyse` does:
//! labels from a `.beats` file beside the audio, and Live's auto-warp read
//! from the warp markers in a Live Set, so the two can be compared on the
//! same files.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::beats::f_measure;

/// Beats and downbeats, in seconds of a file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Labels {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
}

/// Reads labels in the usual `.beats` form: a line for each beat, its time
/// in seconds, then, if it has one, its place in the bar, where 1 starts a
/// bar. Without places, there are no downbeats. Spaces, tabs or a comma
/// part the two, as Sonic Visualiser exports them. Blank lines and lines
/// starting with `#` don't count.
pub fn read_labels(text: &str) -> Result<Labels, String> {
    let mut labels = Labels::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields =
            (line.split(|c: char| c.is_whitespace() || c == ',')).filter(|f| !f.is_empty());
        let time = fields.next().and_then(|t| t.parse::<f64>().ok());
        let Some(time) = time.filter(|t| t.is_finite()) else {
            return Err(format!("line {}: no time in \"{line}\"", n + 1));
        };
        labels.beats.push(time);
        // A place such as 1, 1.1 or "1": its first number.
        let place = (fields.next().map(|p| p.trim_matches('"')))
            .map(|p| p.split('.').next().unwrap_or(p).parse::<u32>());
        if let Some(Ok(1)) = place {
            labels.downbeats.push(time);
        }
    }
    labels.beats.sort_by(f64::total_cmp);
    labels.downbeats.sort_by(f64::total_cmp);
    Ok(labels)
}

/// How well found beats match labelled ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// The beats' F-measure, within ±70 ms.
    pub beats: f64,
    /// The downbeats', if the labels have any.
    pub downbeats: Option<f64>,
}

/// Scores `found` against `labels`, both in seconds.
pub fn score(found: &Labels, labels: &Labels) -> Score {
    let tolerance = crate::TOLERANCE;
    Score {
        beats: f_measure(&found.beats, &labels.beats, tolerance),
        downbeats: (!labels.downbeats.is_empty())
            .then(|| f_measure(&found.downbeats, &labels.downbeats, tolerance)),
    }
}

/// What Live's auto-warp made of each audio file in the Live Set at
/// `path`: the beats its warp markers put on the file, by the file's name
/// in lowercase, for every warped clip with two markers or more. The first
/// clip of a file counts.
pub fn live_beats(path: &Path) -> Result<HashMap<String, Labels>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut xml = String::new();
    flate2::read::GzDecoder::new(file)
        .read_to_string(&mut xml)
        .map_err(|e| format!("{}: not a Live Set: {e}", path.display()))?;
    Ok(live_beats_in(&xml))
}

/// [`live_beats`], from a Live Set's XML.
pub fn live_beats_in(xml: &str) -> HashMap<String, Labels> {
    let mut found = HashMap::new();
    for clip in blocks(xml, "AudioClip") {
        let warped = element(clip, "IsWarped").and_then(|e| attribute(e, "Value"));
        if warped.as_deref() != Some("true") {
            continue;
        }
        let file = (blocks(clip, "FileRef").next())
            .and_then(|r| element(r, "Path"))
            .and_then(|e| attribute(e, "Value"));
        let Some(file) = file else {
            continue;
        };
        let name = file
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&file)
            .to_lowercase();
        let markers: Vec<(f64, f64)> = (blocks(clip, "WarpMarkers").next())
            .map(|markers| {
                elements(markers, "WarpMarker")
                    .filter_map(|e| {
                        let seconds = attribute(e, "SecTime")?.parse().ok()?;
                        let beat = attribute(e, "BeatTime")?.parse().ok()?;
                        Some((seconds, beat))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let length = (element(clip, "DefaultDuration").and_then(|e| attribute(e, "Value")))
            .and_then(|frames| frames.parse::<f64>().ok())
            .zip(element(clip, "DefaultSampleRate").and_then(|e| attribute(e, "Value")))
            .and_then(|(frames, rate)| Some(frames / rate.parse::<f64>().ok()?));
        if let Some(labels) = beats_of(&markers, length) {
            found.entry(name).or_insert(labels);
        }
    }
    found
}

/// The beats warp markers put on a file `length` seconds long, or as far
/// as the last marker without a length: a beat at every whole beat, found
/// between the markers either side, or past the first or last at their
/// pair's tempo. Bars are four beats long from beat 0, Live's 1.1.1.
fn beats_of(markers: &[(f64, f64)], length: Option<f64>) -> Option<Labels> {
    let mut markers = markers.to_vec();
    markers.sort_by(|a, b| a.1.total_cmp(&b.1));
    markers.dedup_by(|a, b| a.1 == b.1);
    if markers.len() < 2 {
        return None;
    }
    // The seconds where `beat` plays, along the markers.
    let seconds_at = |beat: f64| {
        let i = markers
            .partition_point(|m| m.1 <= beat)
            .clamp(1, markers.len() - 1);
        let ((s0, b0), (s1, b1)) = (markers[i - 1], markers[i]);
        s0 + (beat - b0) * (s1 - s0) / (b1 - b0)
    };
    let beat_at = |seconds: f64| {
        let i = markers
            .partition_point(|m| m.0 <= seconds)
            .clamp(1, markers.len() - 1);
        let ((s0, b0), (s1, b1)) = (markers[i - 1], markers[i]);
        b0 + (seconds - s0) * (b1 - b0) / (s1 - s0)
    };
    let end = length.unwrap_or(markers[markers.len() - 1].0);
    let (first, last) = (beat_at(0.0).ceil() as i64, beat_at(end).floor() as i64);
    let mut labels = Labels::default();
    for beat in first..=last {
        let seconds = seconds_at(beat as f64);
        labels.beats.push(seconds);
        if beat.rem_euclid(4) == 0 {
            labels.downbeats.push(seconds);
        }
    }
    Some(labels)
}

/// The blocks `<tag ...>...</tag>` in `xml`, outermost first, each without
/// its closing tag.
fn blocks<'a>(xml: &'a str, tag: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = xml;
    std::iter::from_fn(move || {
        loop {
            let start = rest.find(&open)?;
            let after = &rest[start + open.len()..];
            // `<AudioClipX` isn't an `<AudioClip`.
            if !after.starts_with([' ', '>', '/', '\n', '\r', '\t']) {
                rest = after;
                continue;
            }
            let end = after.find(&close).map_or(after.len(), |e| e + close.len());
            let block = &rest[start..start + open.len() + end];
            rest = &after[end..];
            return Some(block);
        }
    })
}

/// The empty elements `<tag .../>` and opening tags `<tag ...>` in `xml`,
/// each up to its `>`.
fn elements<'a>(xml: &'a str, tag: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let open = format!("<{tag}");
    let mut rest = xml;
    std::iter::from_fn(move || {
        loop {
            let start = rest.find(&open)?;
            let after = &rest[start + open.len()..];
            let end = after.find('>').map_or(after.len(), |e| e + 1);
            let element = &after[..end];
            rest = &after[end..];
            if element.starts_with([' ', '>', '/', '\n', '\r', '\t']) {
                return Some(element);
            }
        }
    })
}

/// The first element `<tag .../>` in `xml`.
fn element<'a>(xml: &'a str, tag: &'a str) -> Option<&'a str> {
    elements(xml, tag).next()
}

/// The value of the attribute `name` in an element's text, unescaped.
fn attribute(element: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let start = element.find(&key)? + key.len();
    let end = element[start..].find('"')? + start;
    Some(
        element[start..end]
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_read_times_and_downbeats() {
        let labels =
            read_labels("# a comment\n0.5 1\n1.0\t2\n\n1.5,3\n2.0, 4\n2.5,\"1.1\"\n").unwrap();
        assert_eq!(labels.beats, [0.5, 1.0, 1.5, 2.0, 2.5]);
        assert_eq!(labels.downbeats, [0.5, 2.5]);
        let plain = read_labels("0.25\n0.75\n").unwrap();
        assert_eq!((plain.beats.len(), plain.downbeats.len()), (2, 0));
        assert!(read_labels("beat 1\n").unwrap_err().contains("line 1"));
    }

    #[test]
    fn scores_count_hits_within_70_ms() {
        let labels = read_labels("0.5 1\n1.0 2\n1.5 3\n2.0 4\n").unwrap();
        let found = Labels {
            beats: vec![0.52, 1.04, 1.5, 2.2],
            downbeats: vec![0.5],
        };
        let score = score(&found, &labels);
        assert_eq!(score.beats, 0.75);
        assert_eq!(score.downbeats, Some(1.0));
    }

    const SET: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton>
  <AudioClip Id="0" Time="0">
    <WarpMarkers>
      <WarpMarker Id="1" SecTime="0.1" BeatTime="0" />
      <WarpMarker Id="2" SecTime="2.1" BeatTime="4" />
      <WarpMarker Id="3" SecTime="4.6" BeatTime="8" />
    </WarpMarkers>
    <SampleRef>
      <FileRef>
        <RelativePath Value="Samples/Drums &amp; Bass.wav" />
        <Path Value="C:/Users/max/Music/Drums &amp; Bass.wav" />
      </FileRef>
      <DefaultDuration Value="220500" />
      <DefaultSampleRate Value="44100" />
    </SampleRef>
    <IsWarped Value="true" />
  </AudioClip>
  <AudioClip Id="1" Time="16">
    <WarpMarkers>
      <WarpMarker Id="1" SecTime="0" BeatTime="0" />
      <WarpMarker Id="2" SecTime="0.015625" BeatTime="0.03125" />
    </WarpMarkers>
    <SampleRef><FileRef><Path Value="/Users/max/Unwarped.aif" /></FileRef></SampleRef>
    <IsWarped Value="false" />
  </AudioClip>
</Ableton>"#;

    #[test]
    fn live_sets_give_the_beats_of_each_warped_file() {
        let found = live_beats_in(SET);
        assert_eq!(found.len(), 1);
        let labels = &found["drums & bass.wav"];
        // Half a second a beat for four beats, then 0.625, to the file's end
        // at 5 s, which is beat 8.64.
        let expected = [0.1, 0.6, 1.1, 1.6, 2.1, 2.725, 3.35, 3.975, 4.6];
        assert_eq!(labels.beats.len(), expected.len());
        for (got, want) in labels.beats.iter().zip(expected) {
            assert!((got - want).abs() < 1e-9, "{:?}", labels.beats);
        }
        assert_eq!(labels.downbeats, [0.1, 2.1, 4.6]);
    }
}
