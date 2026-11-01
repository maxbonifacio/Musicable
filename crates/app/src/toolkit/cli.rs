//! `musicable audio <command>`: the audio commands on the command line. A
//! command takes its file or files first, then its arguments as options,
//! `--name value` with hyphens for underscores, such as `--fade-ms 10`, and
//! `--name` or `--no-name` for those that are true or false. It prints
//! what it gives back as lines of text, or as JSON with `--json`. Relative
//! paths are in the working folder.

use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;

use musicable_commands::{Args, Param, Type, Value};
use serde_json::{Value as Json, json};

use super::{AudioCommand, Job, commands};

/// Runs `musicable audio` with the arguments after `audio`.
pub fn run(args: impl Iterator<Item = OsString>) -> ExitCode {
    let mut texts = Vec::new();
    for arg in args {
        match arg.into_string() {
            Ok(text) => texts.push(text),
            Err(arg) => {
                eprintln!("{} isn't text Musicable can read.", arg.to_string_lossy());
                return ExitCode::FAILURE;
            }
        }
    }
    let commands = commands();
    let Some((name, rest)) = texts.split_first() else {
        eprint!("{}", usage(&commands));
        return ExitCode::FAILURE;
    };
    if matches!(name.as_str(), "-h" | "--help" | "help") {
        print!("{}", usage(&commands));
        return ExitCode::SUCCESS;
    }
    let Some(command) = commands.iter().find(|command| command.name() == name) else {
        eprint!(
            "There's no audio command called {name}.\n\n{}",
            usage(&commands)
        );
        return ExitCode::FAILURE;
    };
    let json = rest
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json");
    let parsed = match parse(command, rest) {
        Ok(Parsed::Help) => {
            print!("{}", help(command));
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Args(args)) => args,
        Err(why) => return failed(&why, json),
    };
    let folder = match std::env::current_dir() {
        Ok(folder) => folder,
        Err(error) => return failed(&format!("The working folder isn't there: {error}."), json),
    };
    match command.run(&parsed, &Job::quiet(Some(&folder))) {
        Ok(result) => {
            let printed = match json {
                true => serde_json::to_string_pretty(&result).unwrap_or_default() + "\n",
                false => text(&result, &Json::Object(command.output_schema())),
            };
            // A reader that stops early, such as `head`, isn't a failure.
            let _ = std::io::stdout().lock().write_all(printed.as_bytes());
            match some_failed(&result) {
                true => ExitCode::FAILURE,
                false => ExitCode::SUCCESS,
            }
        }
        Err(why) => failed(&why, json),
    }
}

/// Says why a command failed: as JSON with `--json`, on the standard
/// output, where the result would have been.
fn failed(why: &str, json: bool) -> ExitCode {
    match json {
        true => println!("{}", json!({ "error": why })),
        false => eprintln!("{why}"),
    }
    ExitCode::FAILURE
}

/// Whether one of the files a command gave back failed.
fn some_failed(result: &Json) -> bool {
    (result.get("files").and_then(Json::as_array))
        .is_some_and(|files| files.iter().any(|file| file.get("error").is_some()))
}

/// What a command's arguments on the command line ask for.
#[derive(Debug, PartialEq)]
pub(super) enum Parsed {
    Args(Args),
    Help,
}

/// The arguments for `command` in `texts`, the command line after its
/// name.
pub(super) fn parse(command: &AudioCommand, texts: &[String]) -> Result<Parsed, String> {
    let mut args = Args::new();
    let mut positional = Vec::new();
    let mut options_end = false;
    let mut texts = texts.iter();
    while let Some(text) = texts.next() {
        if options_end || !text.starts_with('-') || text == "-" {
            positional.push(Value::Text(text.clone()));
            continue;
        }
        match text.as_str() {
            "--" => {
                options_end = true;
                continue;
            }
            "-h" | "--help" => return Ok(Parsed::Help),
            "--json" => continue,
            _ => {}
        }
        let option = text.trim_start_matches('-');
        let (name, inline) = match option.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (option, None),
        };
        let name = name.replace('-', "_");
        let unknown = || {
            format!(
                "{text} isn't an option of musicable audio {0}: musicable audio {0} --help \
                 lists them.",
                command.name()
            )
        };
        let (param, negated) = match find(command, &name) {
            Some(param) => (param, false),
            None => match name
                .strip_prefix("no_")
                .and_then(|name| find(command, name))
            {
                Some(param) if param.ty == Type::Bool => (param, true),
                _ => return Err(unknown()),
            },
        };
        let option = format!("--{}", param.name.replace('_', "-"));
        if param.ty == Type::Bool {
            let value = match inline {
                None | Some("true") => true,
                Some("false") => false,
                Some(other) => {
                    return Err(format!("{option} is true or false, not \"{other}\"."));
                }
            };
            args.set(&param.name, value != negated);
            continue;
        }
        if negated {
            return Err(unknown());
        }
        let value = match inline {
            Some(value) => value.to_owned(),
            None => (texts.next().cloned())
                .ok_or_else(|| format!("{option} needs {} after it.", placeholder(param.ty)))?,
        };
        match param.ty {
            Type::List(item) => {
                let item = read(*item, &option, &value)?;
                let mut items = match args.value(&param.name) {
                    Some(Value::List(items)) => items.clone(),
                    _ => Vec::new(),
                };
                items.push(item);
                args.set(&param.name, Value::List(items));
            }
            ty => {
                if args.value(&param.name).is_some() {
                    return Err(format!("{option} is given twice."));
                }
                args.set(&param.name, read(ty, &option, &value)?);
            }
        }
    }
    if !positional.is_empty() {
        let params: Vec<&Param> = (command.positional.iter())
            .map(|name| find(command, name).expect("a command has its positional ones"))
            .collect();
        // A list last takes the rest; the others take one each.
        let (singles, list) = match params.split_last() {
            Some((last, rest)) if matches!(last.ty, Type::List(_)) => (rest, Some(*last)),
            _ => (params.as_slice(), None),
        };
        let given = singles
            .iter()
            .any(|param| args.value(&param.name).is_some());
        let counts = match list {
            Some(_) => positional.len() > singles.len(),
            None => positional.len() == singles.len(),
        };
        if given || !counts {
            return Err(format!(
                "musicable audio {} takes {} before its options.",
                command.name(),
                before_options(&params)
            ));
        }
        let mut positional = positional.into_iter();
        for (param, value) in singles.iter().zip(positional.by_ref()) {
            args.set(&param.name, value);
        }
        if let Some(param) = list {
            let rest: Vec<Value> = positional.collect();
            let value = match args.value(&param.name) {
                Some(Value::List(given)) => Value::List([given.clone(), rest].concat()),
                _ => Value::List(rest),
            };
            args.set(&param.name, value);
        }
    }
    Ok(Parsed::Args(args))
}

fn find<'a>(command: &'a AudioCommand, name: &str) -> Option<&'a Param> {
    command.params.iter().find(|param| param.name == name)
}

/// `text`, given for `option`, as a value of type `ty`.
fn read(ty: Type, option: &str, text: &str) -> Result<Value, String> {
    let wrong = || format!("{option} takes {}, not \"{text}\".", placeholder(ty));
    match ty {
        Type::Int => text.parse::<i64>().map(Value::Int).map_err(|_| wrong()),
        Type::Number => match text.parse::<f64>() {
            Ok(number) if number.is_finite() => Ok(Value::Number(number)),
            _ => Err(wrong()),
        },
        _ => Ok(Value::Text(text.to_owned())),
    }
}

/// What an option of type `ty` takes, as `--help` and errors say it.
fn placeholder(ty: Type) -> String {
    match ty {
        Type::Int => "a whole number".to_owned(),
        Type::Number => "a number".to_owned(),
        Type::Choice(choices) => choices.join("|"),
        Type::List(item) => placeholder(*item),
        _ => "a path".to_owned(),
    }
}

/// What `musicable audio` alone says.
fn usage(commands: &[AudioCommand]) -> String {
    let mut text = "\
usage: musicable audio <command> [FILE...] [options] [--json]

Cuts, converts and measures audio files, WAV, AIFF and FLAC, and says what
it did, as JSON with --json. Each command is a tool of musicable mcp too.
musicable audio <command> --help says what a command takes.

"
    .to_owned();
    for command in commands {
        text += &format!("  {:<10} {}\n", command.name(), command.title);
    }
    text
}

/// What `musicable audio <command> --help` says.
pub(super) fn help(command: &AudioCommand) -> String {
    let files = files(command);
    let mut text = format!(
        "usage: musicable audio {} {files} [options] [--json]\n\n{}\n\n",
        command.name(),
        wrap(command.about, 0, 76)
    );
    let mut line = |left: String, about: &str| match left.len() {
        // Long options have what they're for on the lines below.
        ..26 => text += &format!("  {left:<26}{}\n", wrap(about, 28, 76).trim_start()),
        _ => text += &format!("  {left}\n{}\n", wrap(about, 28, 76)),
    };
    for name in command.positional {
        let param = find(command, name).expect("a command has its positional ones");
        let left = match command.positional {
            [_] => files.clone(),
            _ => first(command, name),
        };
        line(left, &param.about);
    }
    for param in &command.params {
        if command.positional.contains(&param.name.as_ref()) {
            continue;
        }
        match param.optional {
            true => line(option(param), &param.about),
            false => line(option(param), &format!("{} Required.", param.about)),
        }
    }
    line("--json".to_owned(), "Prints the result as JSON.");
    text += "\nOptions that are true or false take --name for true, and --no-name for false.\n";
    text += &format!(
        "\nexample: musicable audio {} {}\n",
        command.name(),
        command.example.join(" ")
    );
    text
}

/// What the command line calls `command`'s files, which it takes first:
/// `FILE`, `FILE...` for a command that takes any number, or their names,
/// such as `A B`, for one that takes files for different things.
pub(super) fn files(command: &AudioCommand) -> String {
    match command.positional {
        [name] => match find(command, name).map(|param| param.ty) {
            Some(Type::List(_)) => "FILE...".to_owned(),
            _ => "FILE".to_owned(),
        },
        names => (names.iter())
            .map(|name| first(command, name))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// What the command line calls the parameter `name` a command takes
/// first, among others: its name in capitals, and `...` after a list.
pub(super) fn first(command: &AudioCommand, name: &str) -> String {
    match find(command, name).map(|param| param.ty) {
        Some(Type::List(_)) => format!("{}...", name.to_uppercase()),
        _ => name.to_uppercase(),
    }
}

/// What `params`, the ones a command takes first, are, in an error.
fn before_options(params: &[&Param]) -> String {
    let names: Vec<&str> = params.iter().map(|param| param.name.as_ref()).collect();
    match params {
        [param] => format!("one {}", param.name),
        [.., last] if matches!(last.ty, Type::List(_)) => {
            format!(
                "its {}, then its {}",
                names[..names.len() - 1].join(", "),
                last.name
            )
        }
        params => format!("{} files, its {}", params.len(), names.join(" and ")),
    }
}

/// The option that gives `param`, as `--help` writes it: `--fade-ms <n>`.
pub(super) fn option(param: &Param) -> String {
    let option = format!("--{}", param.name.replace('_', "-"));
    match param.ty {
        Type::Bool => option,
        Type::Choice(choices) => format!("{option} <{}>", choices.join("|")),
        Type::Int | Type::Number => format!("{option} <n>"),
        Type::List(Type::Int | Type::Number) => format!("{option} <n>"),
        _ => format!("{option} <path>"),
    }
}

/// `text` in lines at most `width` long, indented by `indent` spaces.
fn wrap(text: &str, indent: usize, width: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut line = " ".repeat(indent);
    for word in text.split_whitespace() {
        if !line.trim_start().is_empty() && line.len() + 1 + word.len() > width {
            lines.push(line);
            line = " ".repeat(indent);
        }
        if !line.trim_start().is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines.join("\n")
}

/// What a command gave back, as lines of text for people: each field on a
/// line of its own, in the order its output schema lists them, and what's
/// in an object or a list under its name.
pub(super) fn text(result: &Json, schema: &Json) -> String {
    let mut out = String::new();
    fields(result, Some(schema), 0, &mut out);
    out
}

/// The fields of `object`, of `schema`, at `indent`.
fn fields(object: &Json, schema: Option<&Json>, indent: usize, out: &mut String) {
    let Json::Object(object) = object else {
        return;
    };
    let schema = schema.and_then(|schema| variant(schema, object));
    // In the order the schema has them, the others after them.
    let listed = (schema.and_then(|schema| schema.get("required")))
        .and_then(Json::as_array)
        .map_or(Vec::new(), |names| {
            names.iter().filter_map(Json::as_str).collect()
        });
    let mut names: Vec<&str> = listed
        .into_iter()
        .filter(|name| object.contains_key(*name))
        .collect();
    let others: Vec<&str> = (object.keys().map(String::as_str))
        .filter(|name| !names.contains(name))
        .collect();
    names.extend(others);
    for name in names {
        let property = schema.and_then(|schema| schema.get("properties")?.get(name));
        field(name, &object[name], property, indent, out);
    }
}

/// The schema `object` fits: `schema`, or the variant of its `anyOf` whose
/// fields it has.
fn variant<'a>(schema: &'a Json, object: &serde_json::Map<String, Json>) -> Option<&'a Json> {
    let Some(variants) = schema.get("anyOf").and_then(Json::as_array) else {
        return Some(schema);
    };
    variants.iter().find(|variant| {
        (variant.get("required").and_then(Json::as_array)).is_some_and(|names| {
            names.len() == object.len()
                && (names.iter())
                    .all(|name| name.as_str().is_some_and(|name| object.contains_key(name)))
        })
    })
}

fn field(name: &str, value: &Json, schema: Option<&Json>, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match value {
        Json::Object(object) => match (object.get("seconds"), object.get("frame")) {
            (Some(seconds), Some(frame)) if object.len() == 2 => {
                *out += &format!("{pad}{name}: {seconds} s, frame {frame}\n");
            }
            _ => {
                *out += &format!("{pad}{name}:\n");
                fields(value, schema, indent + 2, out);
            }
        },
        Json::Array(items) if items.iter().any(Json::is_object) => {
            *out += &format!("{pad}{name}:\n");
            let schema = schema.and_then(|schema| schema.get("items"));
            for item in items {
                let mut lines = String::new();
                match item {
                    Json::Object(_) => fields(item, schema, indent + 4, &mut lines),
                    item => lines = format!("{pad}    {}\n", scalar(item)),
                }
                // The first line starts with a dash.
                if lines.len() >= indent + 4 {
                    lines.replace_range(indent + 2..indent + 4, "- ");
                }
                *out += &lines;
            }
        }
        Json::Array(items) if items.is_empty() => *out += &format!("{pad}{name}: none\n"),
        Json::Array(items) => {
            let items: Vec<String> = items.iter().map(scalar).collect();
            *out += &format!("{pad}{name}: {}\n", items.join(", "));
        }
        value => *out += &format!("{pad}{name}: {}\n", scalar(value)),
    }
}

/// A value that's neither an object nor a list, for people.
fn scalar(value: &Json) -> String {
    match value {
        Json::Null => "none".to_owned(),
        Json::String(text) => text.clone(),
        value => value.to_string(),
    }
}
