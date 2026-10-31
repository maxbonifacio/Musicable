//! `play_midi`: what an agent plays into Musicable Agent MIDI, the MIDI
//! input agents have, as MIDI messages at their times.

use rmcp::model::JsonObject;
use serde_json::{Value as Json, json};

use crate::tools::Refused;

/// MIDI messages to play: each one's seconds from the start, and its three
/// bytes.
pub type Messages = Vec<(f64, [u8; 3])>;

/// The most events a call plays, and the longest it can take.
const MOST_EVENTS: usize = 4000;
const LONGEST: f64 = 120.0;

/// The kinds of event, by the key each event's object names it with.
const KINDS: [&str; 5] = ["note", "cc", "bend", "pressure", "program"];

/// The messages `arguments` give, from `play_midi`.
pub(crate) fn messages(arguments: &JsonObject) -> Result<Messages, Refused> {
    let refused = |why: String| Refused::Arguments(why);
    let Some(Json::Array(events)) = arguments.get("events") else {
        return Err(refused(
            "events must be a list of notes and controllers.".into(),
        ));
    };
    if events.is_empty() {
        return Err(refused("There's nothing to play.".into()));
    }
    if events.len() > MOST_EVENTS {
        return Err(refused(format!(
            "There are {} events, and a call plays at most {MOST_EVENTS}.",
            events.len()
        )));
    }
    let mut messages = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let event =
            event_messages(event).map_err(|why| refused(format!("Event {}: {why}", index + 1)))?;
        messages.extend(event);
    }
    let end = messages.iter().map(|(at, _)| *at).fold(0.0, f64::max);
    if end > LONGEST {
        return Err(refused(format!(
            "It plays for {end:.1} seconds, and a call plays at most {LONGEST}: split it over \
             several calls."
        )));
    }
    messages.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(messages)
}

/// One event's messages, or why it isn't one.
fn event_messages(event: &Json) -> Result<Messages, String> {
    let Json::Object(event) = event else {
        return Err(
            "an event is an object, such as {\"note\": 60, \"at\": 0, \"length\": 0.5}.".into(),
        );
    };
    let kinds: Vec<&str> = (KINDS.iter().copied())
        .filter(|kind| event.contains_key(*kind))
        .collect();
    let kind = match kinds.as_slice() {
        [kind] => *kind,
        [] => return Err(format!("an event is one of {}.", KINDS.join(", "))),
        _ => {
            return Err(format!(
                "an event is one thing, not {}.",
                kinds.join(" and ")
            ));
        }
    };
    let allowed: &[&str] = match kind {
        "note" => &["at", "channel", "length", "velocity", "release"],
        "cc" => &["at", "channel", "value"],
        "pressure" => &["at", "channel", "for"],
        _ => &["at", "channel"],
    };
    if let Some(other) = (event.keys()).find(|key| *key != kind && !allowed.contains(&key.as_str()))
    {
        return Err(format!("a {kind} event takes no {other}."));
    }
    let at = match event.get("at") {
        None | Some(Json::Null) => 0.0,
        Some(at) => at
            .as_f64()
            .filter(|at| (0.0..=LONGEST).contains(at))
            .ok_or(format!("at is from 0 to {LONGEST} seconds."))?,
    };
    let channel = match event.get("channel") {
        None | Some(Json::Null) => 0,
        Some(channel) => match channel.as_u64() {
            Some(channel @ 1..=16) => channel as u8 - 1,
            _ => return Err("channel is from 1 to 16.".into()),
        },
    };
    let byte = |name: &str, default: Option<u8>| -> Result<u8, String> {
        match event.get(name) {
            None | Some(Json::Null) => default.ok_or(format!("a {kind} event needs its {name}.")),
            Some(value) => match value.as_u64() {
                Some(value @ 0..=127) => Ok(value as u8),
                _ => Err(format!("{name} is from 0 to 127.")),
            },
        }
    };
    Ok(match kind {
        "note" => {
            let note = byte("note", None)?;
            let velocity = byte("velocity", Some(100))?;
            if velocity == 0 {
                return Err("velocity is from 1 to 127: 0 is a note off.".into());
            }
            let length = event
                .get("length")
                .ok_or("a note needs its length, in seconds.")?
                .as_f64()
                .filter(|length| (0.0..=LONGEST).contains(length))
                .ok_or(format!("length is from 0 to {LONGEST} seconds."))?;
            let release = byte("release", Some(64))?;
            vec![
                (at, [0x90 | channel, note, velocity]),
                (at + length, [0x80 | channel, note, release]),
            ]
        }
        "cc" => vec![(
            at,
            [0xB0 | channel, byte("cc", None)?, byte("value", None)?],
        )],
        "program" => vec![(at, [0xC0 | channel, byte("program", None)?, 0])],
        "bend" => {
            let bend = event["bend"]
                .as_f64()
                .filter(|bend| (-1.0..=1.0).contains(bend))
                .ok_or("bend is from -1, all the way down, to 1, all the way up.")?;
            let value = ((bend + 1.0) * 8192.0).round().clamp(0.0, 16383.0) as u16;
            vec![(
                at,
                [0xE0 | channel, (value & 0x7F) as u8, (value >> 7) as u8],
            )]
        }
        "pressure" => {
            let pressure = event["pressure"]
                .as_f64()
                .filter(|pressure| (0.0..=1.0).contains(pressure))
                .ok_or("pressure is from 0 to 1.")?;
            let value = (pressure * 127.0).round() as u8;
            match event.get("for") {
                None | Some(Json::Null) => vec![(at, [0xD0 | channel, value, 0])],
                Some(_) => vec![(at, [0xA0 | channel, byte("for", None)?, value])],
            }
        }
        _ => unreachable!("every kind is listed"),
    })
}

/// The schema of `play_midi`'s events.
pub(crate) fn schema() -> Json {
    let number = |about: &str| json!({ "type": "number", "description": about });
    let byte = |about: &str| json!({ "type": "integer", "minimum": 0, "maximum": 127, "description": about });
    let common = |mut properties: Json| {
        if let Some(properties) = properties.as_object_mut() {
            properties.insert(
                "at".into(),
                number("Seconds from the start, 0 unless it says."),
            );
            properties.insert(
                "channel".into(),
                json!({ "type": "integer", "minimum": 1, "maximum": 16, "description": "1 unless it says. MPE gives each note a channel of its own, from 2." }),
            );
        }
        properties
    };
    json!({
        "type": "array",
        "minItems": 1,
        "maxItems": MOST_EVENTS,
        "description": "What to play. Each object is one event, named by its key.",
        "items": {
            "anyOf": [
                { "type": "object", "required": ["note", "length"], "properties": common(json!({
                    "note": byte("The note: 60 is middle C."),
                    "length": number("How long it's held, in seconds."),
                    "velocity": byte("1 to 127, 100 unless it says."),
                    "release": byte("The note off's velocity, 64 unless it says."),
                })) },
                { "type": "object", "required": ["cc", "value"], "properties": common(json!({
                    "cc": byte("The controller: 64 is the sustain pedal, 1 the mod wheel."),
                    "value": byte("Its value: the pedal is down from 64."),
                })) },
                { "type": "object", "required": ["bend"], "properties": common(json!({
                    "bend": number("Pitch bend, from -1 to 1: 0 is none."),
                })) },
                { "type": "object", "required": ["pressure"], "properties": common(json!({
                    "pressure": number("Pressure, from 0 to 1: the channel's, or with for, a note's."),
                    "for": byte("The note it presses, for polyphonic pressure."),
                })) },
                { "type": "object", "required": ["program"], "properties": common(json!({
                    "program": byte("A program change."),
                })) },
            ],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(events: Json) -> Result<Messages, String> {
        let arguments = json!({ "events": events });
        messages(arguments.as_object().unwrap()).map_err(|refused| match refused {
            Refused::Arguments(why) => why,
            Refused::NoTool(name) => name,
        })
    }

    #[test]
    fn events_are_midi_messages_in_time_order() {
        let messages = parse(json!([
            {"note": 60, "length": 0.5, "at": 0.25},
            {"cc": 64, "value": 127},
            {"note": 64, "length": 1, "velocity": 90, "channel": 2, "release": 10},
            {"bend": 1, "at": 0.5, "channel": 2},
            {"bend": -1, "at": 0.6},
            {"pressure": 0.5, "at": 0.7, "channel": 2},
            {"pressure": 1, "for": 64, "at": 0.8},
            {"program": 5, "at": 2},
        ]))
        .unwrap();
        assert_eq!(
            messages,
            [
                (0.0, [0xB0, 64, 127]),
                (0.0, [0x91, 64, 90]),
                (0.25, [0x90, 60, 100]),
                (0.5, [0xE1, 0x7F, 0x7F]),
                (0.6, [0xE0, 0, 0]),
                (0.7, [0xD1, 64, 0]),
                (0.75, [0x80, 60, 64]),
                (0.8, [0xA0, 64, 127]),
                (1.0, [0x81, 64, 10]),
                (2.0, [0xC0, 5, 0]),
            ]
        );
        // No bend is the middle.
        assert_eq!(parse(json!([{"bend": 0}])).unwrap(), [(0.0, [0xE0, 0, 64])]);
    }

    #[test]
    fn events_that_make_no_sense_are_refused() {
        let cases = [
            (json!([]), "There's nothing to play."),
            (
                json!([{"note": 60}]),
                "Event 1: a note needs its length, in seconds.",
            ),
            (
                json!([{"note": 128, "length": 1}]),
                "Event 1: note is from 0 to 127.",
            ),
            (
                json!([{"note": 60, "length": 1, "velocity": 0}]),
                "Event 1: velocity is from 1 to 127: 0 is a note off.",
            ),
            (json!([{"cc": 1}]), "Event 1: a cc event needs its value."),
            (
                json!([{"cc": 1, "value": 2, "channel": 17}]),
                "Event 1: channel is from 1 to 16.",
            ),
            (
                json!([{"bend": 2}]),
                "Event 1: bend is from -1, all the way down, to 1, all the way up.",
            ),
            (
                json!([{"note": 60, "cc": 1}]),
                "Event 1: an event is one thing, not note and cc.",
            ),
            (
                json!([{"chord": 1}]),
                "Event 1: an event is one of note, cc, bend, pressure, program.",
            ),
            (
                json!([{"bend": 0, "length": 1}]),
                "Event 1: a bend event takes no length.",
            ),
            (
                json!([{"note": 60, "length": 100, "at": 50}]),
                "It plays for 150.0 seconds, and a call plays at most 120: split it over several calls.",
            ),
        ];
        for (events, why) in cases {
            assert_eq!(parse(events.clone()), Err(why.to_owned()), "{events}");
        }
    }
}
