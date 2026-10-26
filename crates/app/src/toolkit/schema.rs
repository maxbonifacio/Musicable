//! Pieces of the JSON Schemas of what audio commands give back.

use serde_json::{Map, Value as Json, json};

use super::MOST;

/// An object with `properties`, each of them always there, and no others.
pub(super) fn object(about: &str, properties: &[(&str, Json)]) -> Json {
    let required: Vec<&str> = properties.iter().map(|(name, _)| *name).collect();
    let properties: Map<String, Json> = (properties.iter())
        .map(|(name, schema)| ((*name).to_owned(), schema.clone()))
        .collect();
    json!({
        "type": "object",
        "description": about,
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// `schema`, saying what it is.
fn described(mut schema: Json, about: &str) -> Json {
    schema["description"] = about.into();
    schema
}

pub(super) fn number(about: &str) -> Json {
    described(json!({"type": "number"}), about)
}

/// A number, or `null`: for silence, as levels are.
pub(super) fn level(about: &str) -> Json {
    described(json!({"type": ["number", "null"]}), about)
}

/// A whole number, 0 or more.
pub(super) fn count(about: &str) -> Json {
    described(json!({"type": "integer", "minimum": 0}), about)
}

pub(super) fn text(about: &str) -> Json {
    described(json!({"type": "string"}), about)
}

pub(super) fn boolean(about: &str) -> Json {
    described(json!({"type": "boolean"}), about)
}

/// One of `choices`.
pub(super) fn choice(choices: &[&str], about: &str) -> Json {
    described(json!({"type": "string", "enum": choices}), about)
}

/// A list of `item`s, 200 at most.
pub(super) fn list(item: Json, about: &str) -> Json {
    described(
        json!({"type": "array", "items": item, "maxItems": MOST}),
        about,
    )
}

/// `schema`, or `null`.
pub(super) fn or_null(schema: Json, about: &str) -> Json {
    json!({"description": about, "anyOf": [schema, {"type": "null"}]})
}

/// A position in a file, as [`super::position`] gives it.
pub(super) fn position(about: &str) -> Json {
    object(
        about,
        &[
            (
                "seconds",
                number("Its time from the start of the file, in seconds, to 6 places."),
            ),
            (
                "frame",
                count("Its frame, counting from 0 at the start of the file."),
            ),
        ],
    )
}

/// A file as `audio.info` describes it.
pub(super) fn file(about: &str) -> Json {
    object(
        about,
        &[
            ("file", text("Its whole path.")),
            ("format", choice(&["wav", "aiff", "flac"], "Its format.")),
            (
                "encoding",
                text(
                    "How it stores its samples: int8 to int32 for integers of that many bits, \
                     or float32 or float64.",
                ),
            ),
            ("channels", count("Its channels.")),
            ("sample_rate", count("Its sample rate, in hertz.")),
            (
                "frames",
                count("Its length in frames, a sample of each channel."),
            ),
            ("seconds", number("Its length in seconds, to 6 places.")),
            (
                "peak_dbfs",
                level(
                    "Its loudest sample, in decibels from full scale, to 2 places: null if it's \
                     silent.",
                ),
            ),
            (
                "acid_bpm",
                level(
                    "The tempo its ACID chunk gives, as loops from sample libraries have, in \
                     beats a minute: null without one.",
                ),
            ),
            (
                "audio_md5",
                text(
                    "The MD5 of its samples, as a FLAC file keeps it: files of the same \
                     samples hash the same, whatever their format.",
                ),
            ),
        ],
    )
}

/// What a command gives back for each of its files: `each`, or why the
/// file failed.
pub(super) fn files(each: Json, about: &str) -> Json {
    let failed = object(
        "A file the command couldn't do, and why.",
        &[
            ("file", text("Its whole path.")),
            ("error", text("Why it failed, in a sentence.")),
        ],
    );
    list(json!({"anyOf": [each, failed]}), about)
}
