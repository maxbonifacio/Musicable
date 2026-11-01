//! Commands' arguments and results as JSON, and their parameters as JSON
//! Schema, for scripts and agents, which speak JSON.
//!
//! An argument is written as JSON writes its [`Type`]: `true` or `false`, a
//! number, a string, or an array. Positions and lengths are numbers of
//! beats, such as `1.5`, ids are whole numbers, and a parameter is a string
//! as [`param_text`] writes it, such as `"track 3 volume"`. A command's
//! result reads the same way.
//!
//! Callers that speak JSON can't answer the questions the app asks the user,
//! so they must give the parameters the app would [ask](Param::asked) for.

use std::collections::BTreeMap;

use serde_json::{Map, Number, Value as Json, json};

use crate::{Args, Command, CommandError, CommandId, Param, Registry, Type, Value, param_text};

/// `value` as JSON. A number that isn't finite is `null`, and a whole
/// number of beats is written as a whole number.
pub fn to_json(value: &Value) -> Json {
    let number = |n: f64| Number::from_f64(n).map_or(Json::Null, Json::Number);
    match value {
        Value::None => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::from(*i),
        Value::Number(n) => number(*n),
        Value::Text(text) => Json::String(text.clone()),
        Value::Beats(beats) => match beats.to_f64() {
            whole if whole.fract() == 0.0 && whole.abs() < 9_007_199_254_740_992.0 => {
                Json::from(whole as i64)
            }
            beats => number(beats),
        },
        Value::Track(id) => Json::from(id.0),
        Value::Clip(id) => Json::from(id.0),
        Value::Note(id) => Json::from(id.0),
        Value::Locator(id) => Json::from(id.0),
        Value::Device(id) => Json::from(id.0),
        Value::Take(id) => Json::from(id.0),
        Value::Modulator(id) => Json::from(id.0),
        Value::Routing(id) => Json::from(id.0),
        Value::Wire(id) => Json::from(id.0),
        Value::Param(param) => Json::String(param_text(*param)),
        Value::List(values) => Json::Array(values.iter().map(to_json).collect()),
    }
}

/// `json` as an argument of type `ty`, if it can be read as one. A number
/// without a fraction reads as a whole number, which stands for a float, a
/// position or an id as well.
pub fn from_json(ty: Type, json: &Json) -> Option<Value> {
    let value = match (ty, json) {
        (Type::Bool, Json::Bool(b)) => Value::Bool(*b),
        (Type::Text | Type::Param | Type::Choice(_), Json::String(text)) => {
            Value::Text(text.clone())
        }
        (Type::List(item), Json::Array(items)) => {
            let items: Option<Vec<Value>> = items.iter().map(|j| from_json(*item, j)).collect();
            Value::List(items?)
        }
        (_, Json::Number(number)) => match number.as_i64() {
            Some(i) => Value::Int(i),
            None => {
                let n = number.as_f64()?;
                // Whole numbers that fit exactly.
                if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
                    Value::Int(n as i64)
                } else {
                    Value::Number(n)
                }
            }
        },
        _ => return None,
    };
    ty.accepts(&value).then_some(value)
}

/// The arguments in `json` for a command with `params`, each read as its
/// parameter's type. `null` leaves an optional one out. Besides those a
/// command needs, those the app would ask the user for must be there.
pub fn args_from_json(params: &[Param], json: &Map<String, Json>) -> Result<Args, CommandError> {
    let mut args = Args::new();
    for (name, json) in json {
        let Some(param) = params.iter().find(|param| param.name == name.as_str()) else {
            return Err(CommandError::UnknownArgument(name.clone()));
        };
        if json.is_null() && param.optional {
            continue;
        }
        let value = from_json(param.ty, json).ok_or_else(|| CommandError::WrongType {
            name: name.clone(),
            expected: param.ty,
        })?;
        args.set(name, value);
    }
    match params
        .iter()
        .find(|param| (!param.optional || param.asked) && args.value(&param.name).is_none())
    {
        Some(param) => Err(CommandError::MissingArgument(param.name.to_string())),
        None => Ok(args),
    }
}

/// The command called `id` in `registry`, and the arguments in `json` for
/// it, as an agent runs it: the command must be one [for
/// agents](Command::agents), and the arguments are read by
/// [`args_from_json`].
pub fn agent_call(
    registry: &Registry,
    id: &str,
    json: &Map<String, Json>,
) -> Result<(CommandId, Args), CommandError> {
    let id = CommandId::from(id);
    let Some(command) = registry.get(&id) else {
        return Err(CommandError::Unknown(id));
    };
    let args = agent_args(command, json)?;
    Ok((id, args))
}

/// The arguments in `json` for `command`, as an agent runs it: the command
/// must be one [for agents](Command::agents), and the arguments are read
/// by [`args_from_json`].
pub fn agent_args(command: &Command, json: &Map<String, Json>) -> Result<Args, CommandError> {
    if !command.agents {
        return Err(CommandError::NotForAgents(command.id.clone()));
    }
    args_from_json(&command.params, json)
}

/// `args` as JSON, as a script would write them.
pub fn args_to_json(args: &Args) -> Map<String, Json> {
    (args.iter())
        .map(|(name, value)| (name.to_owned(), to_json(value)))
        .collect()
}

/// The JSON Schema of the arguments a command with `params` takes: an
/// object with a property for each parameter, saying what it's for and its
/// type. Those the command needs, and those the app would ask the user for,
/// must be there.
pub fn schema(params: &[Param]) -> Map<String, Json> {
    let properties: Map<String, Json> = params
        .iter()
        .map(|param| {
            let mut schema = type_schema(param.ty);
            let about = format!("{} ({})", param.about, type_text(param.ty));
            schema.insert("description".into(), Json::String(about));
            (param.name.to_string(), Json::Object(schema))
        })
        .collect();
    let required: Vec<Json> = params
        .iter()
        .filter(|param| !param.optional || param.asked)
        .map(|param| Json::String(param.name.to_string()))
        .collect();
    let mut schema = Map::new();
    schema.insert("type".into(), "object".into());
    schema.insert("properties".into(), Json::Object(properties));
    if !required.is_empty() {
        schema.insert("required".into(), Json::Array(required));
    }
    schema.insert("additionalProperties".into(), false.into());
    schema
}

/// The JSON Schema of an argument of type `ty`.
pub fn type_schema(ty: Type) -> Map<String, Json> {
    let id = || json!({"type": "integer", "minimum": 0, "maximum": u32::MAX});
    let schema = match ty {
        Type::Bool => json!({"type": "boolean"}),
        Type::Int => json!({"type": "integer"}),
        Type::Number | Type::Beats => json!({"type": "number"}),
        Type::Text | Type::Param => json!({"type": "string"}),
        Type::Track
        | Type::Clip
        | Type::Note
        | Type::Locator
        | Type::Device
        | Type::Take
        | Type::Modulator
        | Type::Routing
        | Type::Wire => id(),
        Type::List(item) => json!({"type": "array", "items": type_schema(*item)}),
        Type::Choice(choices) => json!({"type": "string", "enum": choices}),
    };
    match schema {
        Json::Object(schema) => schema,
        _ => unreachable!("each schema is an object"),
    }
}

/// What an argument of type `ty` is, as JSON writes it.
pub fn type_text(ty: Type) -> String {
    match ty {
        Type::Bool => "true or false".into(),
        Type::Int => "a whole number".into(),
        Type::Number => "a number".into(),
        Type::Text => "text".into(),
        Type::Beats => "a number of beats".into(),
        Type::Track => "a track's id".into(),
        Type::Clip => "a clip's id".into(),
        Type::Note => "a note's id in its clip".into(),
        Type::Locator => "a locator's id".into(),
        Type::Device => "a device's id".into(),
        Type::Take => "a take's id".into(),
        Type::Modulator => "a modulator's id".into(),
        Type::Routing => "a routing's id".into(),
        Type::Wire => "a wire's id".into(),
        Type::Param | Type::Choice(_) => ty.to_string(),
        Type::List(item) => format!("a list, each {}", type_text(*item)),
    }
}

/// What earlier commands gave back, by the names they were given, for the
/// arguments of those after them: see [`link`].
pub type Results = BTreeMap<String, Json>;

/// `args` with each reference to an earlier command's result replaced by
/// that result, or by a part of it. A reference is a string that is
/// nothing but `$` and a name, such as `"$comp"`, which [`Results`] has,
/// optionally followed by a path into it: `"$notes.0"` is the first item
/// of a list, and `"$made.device"` a field of an object. References can be
/// anywhere in the arguments, in lists too. A string that starts with `$$`
/// stands for itself with one `$` less, and other strings, such as
/// `"$5 off"`, are left as they are.
///
/// Fails, saying why in a phrase, when a reference names no result, or a
/// part the result doesn't have.
pub fn link(args: &Map<String, Json>, results: &Results) -> Result<Map<String, Json>, String> {
    (args.iter())
        .map(|(name, json)| Ok((name.clone(), link_value(json, results)?)))
        .collect()
}

/// The names of the earlier commands' results that `args` refer to, as
/// [`link`] reads them, each once, in order.
pub fn references(args: &Map<String, Json>) -> Vec<&str> {
    fn find<'a>(json: &'a Json, names: &mut Vec<&'a str>) {
        match json {
            Json::String(text) => {
                if let Some((name, _)) = reference(text)
                    && !names.contains(&name)
                {
                    names.push(name);
                }
            }
            Json::Array(items) => items.iter().for_each(|item| find(item, names)),
            Json::Object(fields) => fields.values().for_each(|field| find(field, names)),
            _ => {}
        }
    }
    let mut names = Vec::new();
    args.values().for_each(|json| find(json, &mut names));
    names
}

fn link_value(json: &Json, results: &Results) -> Result<Json, String> {
    Ok(match json {
        Json::String(text) => match reference(text) {
            Some((name, path)) => {
                let Some(mut found) = results.get(name) else {
                    return Err(match results.is_empty() {
                        true => format!("${name} names no earlier command's result"),
                        false => format!(
                            "${name} names no earlier command's result: those named are {}",
                            (results.keys().map(|name| format!("${name}")))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    });
                };
                for part in path {
                    let next = match found {
                        Json::Array(items) => part.parse::<usize>().ok().and_then(|i| items.get(i)),
                        Json::Object(fields) => fields.get(part),
                        _ => None,
                    };
                    found =
                        next.ok_or_else(|| format!("{text} isn't in ${name}, which is {found}"))?;
                }
                found.clone()
            }
            None => match text.strip_prefix("$$") {
                Some(rest) => Json::String(format!("${rest}")),
                None => json.clone(),
            },
        },
        Json::Array(items) => Json::Array(
            (items.iter())
                .map(|item| link_value(item, results))
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(fields) => Json::Object(link(fields, results)?),
        other => other.clone(),
    })
}

/// The name a reference such as `"$comp"` or `"$notes.0"` refers to, and
/// the path after it, if `text` is one.
fn reference(text: &str) -> Option<(&str, Vec<&str>)> {
    let mut parts = text.strip_prefix('$')?.split('.');
    let name = parts.next()?;
    let word = |part: &str| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let starts = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    let path: Vec<&str> = parts.collect();
    (starts && word(name) && path.iter().all(|part| word(part))).then_some((name, path))
}

#[cfg(test)]
mod tests {
    use musicable_model::clip::{ClipId, NoteId};
    use musicable_model::modulation::{ModulatorId, RoutingId};
    use musicable_model::param::ParamAddress;
    use musicable_model::routing::WireId;
    use musicable_model::time::Beats;
    use musicable_model::{Chain, DeviceId, LocatorId, TakeId, TrackId};

    use super::*;

    #[test]
    fn values_go_to_json_and_back_as_their_types() {
        let values = [
            (Type::Bool, Value::Bool(true), json!(true)),
            (Type::Int, Value::Int(-3), json!(-3)),
            (Type::Number, Value::Number(0.25), json!(0.25)),
            (Type::Text, Value::Text("Bass".into()), json!("Bass")),
            (
                Type::Beats,
                Value::Beats(Beats::from_ratio(3, 2)),
                json!(1.5),
            ),
            (Type::Track, Value::Track(TrackId(3)), json!(3)),
            (Type::Clip, Value::Clip(ClipId(4)), json!(4)),
            (Type::Note, Value::Note(NoteId(5)), json!(5)),
            (Type::Locator, Value::Locator(LocatorId(6)), json!(6)),
            (Type::Device, Value::Device(DeviceId(7)), json!(7)),
            (Type::Take, Value::Take(TakeId(8)), json!(8)),
            (Type::Modulator, Value::Modulator(ModulatorId(9)), json!(9)),
            (Type::Routing, Value::Routing(RoutingId(10)), json!(10)),
            (Type::Wire, Value::Wire(WireId(11)), json!(11)),
            (
                Type::Param,
                Value::Param(ParamAddress::Volume(Chain::Master)),
                json!("master volume"),
            ),
            (
                Type::CLIPS,
                Value::List(vec![Value::Clip(ClipId(1)), Value::Clip(ClipId(2))]),
                json!([1, 2]),
            ),
        ];
        for (ty, value, written) in values {
            assert_eq!(to_json(&value), written, "{value:?}");
            // Read back, it's what a script would write for the same thing.
            let read = from_json(ty, &written).unwrap_or_else(|| panic!("{written} as {ty}"));
            assert_eq!(to_json(&read), written, "{written} as {ty}");
        }
        assert_eq!(to_json(&Value::None), Json::Null);
        assert_eq!(to_json(&Value::Number(f64::NAN)), Json::Null);
    }

    #[test]
    fn a_choice_is_one_of_its_words() {
        const WHAT: Type = Type::Choice(&["loudest", "quietest", "silence"]);
        assert_eq!(
            from_json(WHAT, &json!("quietest")),
            Some(Value::Text("quietest".into()))
        );
        assert_eq!(from_json(WHAT, &json!("loud")), None);
        assert_eq!(from_json(WHAT, &json!(1)), None);
        assert_eq!(WHAT.to_string(), "one of loudest, quietest or silence");
        assert_eq!(type_text(WHAT), WHAT.to_string());
        assert_eq!(
            Json::Object(type_schema(WHAT)),
            json!({"type": "string", "enum": ["loudest", "quietest", "silence"]})
        );
        assert_eq!(Type::Choice(&["int24"]).to_string(), "int24");
    }

    #[test]
    fn numbers_stand_in_for_each_other_as_scripts_write_them() {
        // A whole number written with a fraction is still whole.
        assert_eq!(from_json(Type::Track, &json!(3.0)), Some(Value::Int(3)));
        assert_eq!(from_json(Type::Number, &json!(2)), Some(Value::Int(2)));
        assert_eq!(
            from_json(Type::Beats, &json!(0.5)),
            Some(Value::Number(0.5))
        );
        // But not where they don't fit.
        for (ty, bad) in [
            (Type::Track, json!(-1)),
            (Type::Track, json!(2.5)),
            (Type::Clip, json!(5_000_000_000_u64)),
            (Type::Int, json!("3")),
            (Type::Text, json!(3)),
            (Type::Bool, json!(1)),
            (Type::Param, json!("track volume")),
            (Type::CLIPS, json!([1, "2"])),
            (Type::CLIPS, json!(1)),
        ] {
            assert_eq!(from_json(ty, &bad), None, "{bad} as {ty}");
        }
    }

    const PARAMS: [Param; 4] = [
        Param::required("clips", Type::CLIPS, "The clips to move."),
        Param::optional("by", Type::Beats, "How far."),
        Param::optional("copy", Type::Bool, "Copies them instead."),
        Param::asked("file", Type::Text, "Where to: the app asks without it."),
    ];

    fn object(json: Json) -> Map<String, Json> {
        match json {
            Json::Object(object) => object,
            _ => panic!("{json} isn't an object"),
        }
    }

    #[test]
    fn arguments_are_read_from_json_by_their_parameters() {
        let json = object(json!({"clips": [1, 4], "by": 2, "copy": null, "file": "/a.wav"}));
        let args = args_from_json(&PARAMS, &json).unwrap();
        assert_eq!(
            args.get::<Vec<ClipId>>("clips"),
            Ok(vec![ClipId(1), ClipId(4)])
        );
        assert_eq!(args.get("by"), Ok(Beats::new(2)));
        assert_eq!(args.value("copy"), None);
        assert_eq!(args.check(&PARAMS), Ok(()));
        let as_json = args_to_json(&args);
        assert_eq!(
            Json::Object(as_json),
            json!({"clips": [1, 4], "by": 2, "file": "/a.wav"})
        );

        let read = |json| args_from_json(&PARAMS, &object(json));
        assert_eq!(
            read(json!({"clips": [1], "file": "/a.wav", "tracks": 2})),
            Err(CommandError::UnknownArgument("tracks".into()))
        );
        assert_eq!(
            read(json!({"clips": "1", "file": "/a.wav"})),
            Err(CommandError::WrongType {
                name: "clips".into(),
                expected: Type::CLIPS
            })
        );
        assert_eq!(
            read(json!({"file": "/a.wav"})),
            Err(CommandError::MissingArgument("clips".into()))
        );
        // The app would ask for the file, which a script can't answer.
        assert_eq!(
            read(json!({"clips": [1], "file": null})),
            Err(CommandError::MissingArgument("file".into()))
        );
        assert_eq!(
            read(json!({"clips": null, "file": "/a.wav"})),
            Err(CommandError::WrongType {
                name: "clips".into(),
                expected: Type::CLIPS
            })
        );
    }

    #[test]
    fn references_take_what_earlier_commands_gave_back() {
        let results: Results = [
            ("comp".to_owned(), json!(7)),
            ("made".to_owned(), json!({"clips": [3, 4], "name": "Kick"})),
        ]
        .into_iter()
        .collect();
        let linked = link(
            &object(json!({
                "device": "$comp",
                "clips": ["$made.clips.1", 9],
                "name": "$made.name",
                "price": "$5",
                "money": "$$comp",
                "nested": {"at": "$made.clips"},
            })),
            &results,
        );
        assert_eq!(
            linked,
            Ok(object(json!({
                "device": 7,
                "clips": [4, 9],
                "name": "Kick",
                "price": "$5",
                "money": "$comp",
                "nested": {"at": [3, 4]},
            })))
        );
        let args = object(json!({"clips": ["$made.clips.0", "$comp"], "to": "$made"}));
        assert_eq!(references(&args), ["made", "comp"]);
        assert!(references(&object(json!({"name": "$$made", "price": "$5"}))).is_empty());
        assert_eq!(
            link(&object(json!({"device": "$made.clips.2"})), &results),
            Err("$made.clips.2 isn't in $made, which is [3,4]".to_owned())
        );
        assert_eq!(
            link(&object(json!({"device": "$eq"})), &Results::new()),
            Err("$eq names no earlier command's result".to_owned())
        );
    }

    #[test]
    fn parameters_are_described_as_json_schema() {
        let schema = Json::Object(schema(&PARAMS));
        assert_eq!(
            schema,
            json!({
                "type": "object",
                "properties": {
                    "clips": {
                        "type": "array",
                        "items": {"type": "integer", "minimum": 0, "maximum": u32::MAX},
                        "description": "The clips to move. (a list, each a clip's id)",
                    },
                    "by": {"type": "number", "description": "How far. (a number of beats)"},
                    "copy": {"type": "boolean", "description": "Copies them instead. (true or false)"},
                    "file": {"type": "string", "description": "Where to: the app asks without it. (text)"},
                },
                "required": ["clips", "file"],
                "additionalProperties": false,
            })
        );
        let none = Json::Object(super::schema(&[]));
        assert_eq!(
            none,
            json!({"type": "object", "properties": {}, "additionalProperties": false})
        );
    }
}
