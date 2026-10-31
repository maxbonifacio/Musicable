//! The manual's page of the audio commands,
//! `manual/src/generated-audio/commands.md`, made from the commands: what
//! each does, its options, the fields it gives back, and its example, with
//! what the example prints, run on the files the tests make. The manual's
//! Audio commands page includes it. The test checks that it's what the code
//! makes now, as `crates/ui/src/manual.rs` checks the manual's other pages
//! made from the code.
//!
//! `MUSICABLE_BLESS=1 cargo test -p musicable-app --lib manual` makes it
//! again. CONTRIBUTING.md#the-manual has more on the manual.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use musicable_commands::Param;
use serde_json::Value as Json;

use super::tests::{Folder, fixtures, texts, without_folder};
use super::{AudioCommand, Job, cli, commands};

/// The page, in the manual's source.
const PAGE: &str = "generated-audio/commands.md";

/// What the page starts with.
const HEADER: &str = "<!-- Made from the code by crates/app/src/toolkit/manual.rs: don't edit \
                      it. `MUSICABLE_BLESS=1 cargo test -p musicable-app --lib manual` makes \
                      it again. -->\n\n";

/// The folder the examples run in, as the page shows it.
const FOLDER: &str = "/music";

/// The files the examples run on, which [`fixtures`] makes.
const FILES: &str = "The examples run in a folder, `/music`, of three files: `tone.wav`, 2 \
                     seconds of stereo 24-bit 44.1 kHz audio, half a second of silence and then \
                     a 441 Hz tone at -12 dBFS for a second and at -6 dBFS for half a second; \
                     `loop.flac`, a second of four noise hits, in stereo 16-bit 44.1 kHz; and \
                     `voice.aif`, 1.5 seconds of a 240 Hz tone that swells and fades, in mono \
                     32-bit floating point at 48 kHz.";

/// The manual's source.
fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manual/src")
}

fn blessing() -> bool {
    std::env::var_os("MUSICABLE_BLESS").is_some()
}

/// The page: a section for each command, in order of id.
fn page() -> String {
    let mut page = format!("{HEADER}{FILES}\n");
    for command in commands() {
        page.push('\n');
        section(&command, &mut page);
    }
    page
}

/// `command`'s section of the page.
fn section(command: &AudioCommand, page: &mut String) {
    let name = command.name();
    let files = cli::files(command);
    let _ = writeln!(page, "### {name}\n");
    let _ = writeln!(
        page,
        "**{}**: `musicable audio {name} {files} [options]`, or the tool `{}`.\n",
        command.title,
        command.tool()
    );
    let _ = writeln!(page, "{}\n", command.about);

    page.push_str("| Option | Argument | What it's for |\n|---|---|---|\n");
    // The files first, as the command line takes them.
    let first = |param: &&Param| command.positional.contains(&param.name.as_ref());
    let (mut first, rest): (Vec<_>, Vec<_>) = command.params.iter().partition(first);
    first.sort_by_key(|param| {
        command
            .positional
            .iter()
            .position(|name| *name == param.name)
    });
    for param in first.into_iter().chain(rest) {
        let option = match command.positional {
            [name] if *name == param.name => files.clone(),
            names if names.contains(&param.name.as_ref()) => param.name.to_uppercase(),
            _ => cli::option(param),
        };
        let about = match param.optional {
            true => param.about.to_string(),
            false => format!("{} Required.", param.about),
        };
        let _ = writeln!(
            page,
            "| `{}` | `{}` | {} |",
            cell(&option),
            param.name,
            cell(&about)
        );
    }

    page.push_str("\nIt gives back:\n\n| Field | What it is |\n|---|---|\n");
    let schema = Json::Object(command.output_schema());
    fields(&schema, "", command.id == "audio.info", page);

    let folder = Folder::new(&format!("manual-{name}"));
    fixtures(&folder.0);
    let args = match cli::parse(command, &texts(command.example)) {
        Ok(cli::Parsed::Args(args)) => args,
        other => panic!("{}'s example reads as {other:?}", command.id),
    };
    let result = (command.run(&args, &Job::quiet(Some(&folder.0))))
        .unwrap_or_else(|why| panic!("{}'s example: {why}", command.id));
    let result = without_folder(&result, &folder.0, FOLDER);
    let _ = writeln!(
        page,
        "\nFor example,\n\n```sh\nmusicable audio {name} {}\n```\n\nprints\n\n```text\n{}```",
        command.example.join(" "),
        cli::text(&result, &schema)
    );
}

/// What a field of a result holds.
enum Shape<'a> {
    /// A position in a file: its seconds and frame.
    Position,
    /// A file, as `info` describes it.
    File(&'a Json),
    /// An object, or `null`.
    Object(&'a Json),
    /// A list of objects, of which those for files that failed are a file
    /// and an error.
    List(&'a Json, bool),
    Value,
}

/// What `schema`, a field's, holds.
fn shape(schema: &Json) -> Shape<'_> {
    let is_object = |schema: &Json| schema["type"] == "object";
    if let Some(variants) = schema["anyOf"].as_array() {
        // Something, or null.
        return match variants.iter().find(|variant| is_object(variant)) {
            Some(object) => shape(object),
            None => Shape::Value,
        };
    }
    if schema["type"] == "array" {
        let items = &schema["items"];
        let objects: Vec<&Json> = match items["anyOf"].as_array() {
            Some(variants) => variants.iter().collect(),
            None => vec![items],
        };
        let failed = |object: &Json| object["properties"].get("error").is_some();
        return match objects
            .iter()
            .find(|object| is_object(object) && !failed(object))
        {
            Some(each) => Shape::List(each, objects.iter().any(|object| failed(object))),
            None => Shape::Value,
        };
    }
    if !is_object(schema) {
        return Shape::Value;
    }
    let properties = &schema["properties"];
    if properties.get("seconds").is_some() && properties.get("frame").is_some() {
        Shape::Position
    } else if properties.get("audio_md5").is_some() {
        Shape::File(schema)
    } else {
        Shape::Object(schema)
    }
}

/// Rows of a table of the fields of `schema`, an object's, in its order,
/// each named after `prefix`. The fields of files are `info`'s, which only
/// its own table lists, when `file_fields`.
fn fields(schema: &Json, prefix: &str, file_fields: bool, page: &mut String) {
    let names = schema["required"].as_array().into_iter().flatten();
    for name in names.filter_map(Json::as_str) {
        let field = &schema["properties"][name];
        let path = format!("{prefix}{name}");
        let about = field["description"].as_str().unwrap_or_default();
        let mut row = |about: &str| {
            let _ = writeln!(page, "| `{path}` | {} |", cell(about));
        };
        match shape(field) {
            Shape::Position => row(&format!("{about} Its `seconds` and `frame`.")),
            Shape::File(file) if file_fields => {
                row(about);
                fields(file, &format!("{path}."), file_fields, page);
            }
            Shape::File(_) => row(&format!("{about} Its fields are those `info` gives.")),
            Shape::Object(object) => {
                row(about);
                fields(object, &format!("{path}."), file_fields, page);
            }
            Shape::List(each, failed) => {
                match failed {
                    true => row(&format!(
                        "{about} One the command couldn't read or write has only its `file` \
                         and an `error`, which says why."
                    )),
                    false => row(about),
                }
                if file_fields || !matches!(shape(each), Shape::File(_)) {
                    fields(each, &format!("{path}[]."), file_fields, page);
                }
            }
            Shape::Value => row(about),
        }
    }
}

/// `text` for a cell of a table, where `|` would end the cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

#[test]
fn the_page_of_audio_commands_is_what_the_code_makes() {
    let path = source().join(PAGE);
    let page = page();
    if blessing() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &page).unwrap();
    } else {
        assert!(
            fs::read_to_string(&path).ok().as_ref() == Some(&page),
            "manual/src/{PAGE} isn't what the code makes. Run\n  \
             MUSICABLE_BLESS=1 cargo test -p musicable-app --lib manual\n\
             to make it again, and commit it."
        );
    }
}

#[test]
fn the_manual_s_audio_commands_page_includes_it() {
    let source = source();
    let summary = fs::read_to_string(source.join("SUMMARY.md")).unwrap();
    assert!(summary.contains("](reference/audio-commands.md)"));
    let page = fs::read_to_string(source.join("reference/audio-commands.md")).unwrap();
    assert!(page.contains(&format!("{{{{#include ../{PAGE}}}}}")));
}
