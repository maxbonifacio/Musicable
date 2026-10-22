//! What commands take and give back: typed arguments, results, and the
//! errors running a command can end in.
//!
//! A command lists its [`Param`]s. Menus, keys and the command palette run
//! commands without arguments, so they only offer commands whose parameters
//! are all optional. The UI, scripts, agents and extensions pass [`Args`]
//! through [`Registry::run_with`](crate::Registry::run_with), which checks
//! them against the parameters before the command runs.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

use musicable_model::clip::ClipId;
use musicable_model::time::Beats;
use musicable_model::{LocatorId, TrackId};

use crate::CommandId;

/// An argument, or what a command gives back.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    /// Nothing: what a command without a result gives back.
    #[default]
    None,
    Bool(bool),
    Int(i64),
    Number(f64),
    Text(String),
    /// A song position or a length.
    Beats(Beats),
    Track(TrackId),
    Clip(ClipId),
    Locator(LocatorId),
    List(Vec<Value>),
}

/// The type of a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Bool,
    Int,
    Number,
    Text,
    Beats,
    Track,
    Clip,
    Locator,
    List(&'static Type),
}

impl Type {
    /// A list of clips.
    pub const CLIPS: Type = Type::List(&Type::Clip);
    /// A list of tracks.
    pub const TRACKS: Type = Type::List(&Type::Track);

    /// Whether `value` can be read as this type. Numbers stand in for
    /// positions in beats, and whole numbers for ids and for floats, as a
    /// script would write them.
    pub fn accepts(self, value: &Value) -> bool {
        match self {
            Type::Bool => bool::from_value(value).is_some(),
            Type::Int => i64::from_value(value).is_some(),
            Type::Number => f64::from_value(value).is_some(),
            Type::Text => matches!(value, Value::Text(_)),
            Type::Beats => Beats::from_value(value).is_some(),
            Type::Track => TrackId::from_value(value).is_some(),
            Type::Clip => ClipId::from_value(value).is_some(),
            Type::Locator => LocatorId::from_value(value).is_some(),
            Type::List(item) => {
                matches!(value, Value::List(values) if values.iter().all(|v| item.accepts(v)))
            }
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Bool => f.write_str("true or false"),
            Type::Int => f.write_str("a whole number"),
            Type::Number => f.write_str("a number"),
            Type::Text => f.write_str("text"),
            Type::Beats => f.write_str("a position in beats"),
            Type::Track => f.write_str("a track"),
            Type::Clip => f.write_str("a clip"),
            Type::Locator => f.write_str("a locator"),
            Type::List(item) => write!(f, "a list, each {item}"),
        }
    }
}

/// One of the arguments a command takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: Cow<'static, str>,
    pub ty: Type,
    /// Whether the command runs without it, doing what [`about`](Self::about)
    /// says.
    pub optional: bool,
    /// What it's for, for the command palette, scripts and agents.
    pub about: Cow<'static, str>,
}

impl Param {
    pub const fn required(name: &'static str, ty: Type, about: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            ty,
            optional: false,
            about: Cow::Borrowed(about),
        }
    }

    pub const fn optional(name: &'static str, ty: Type, about: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            ty,
            optional: true,
            about: Cow::Borrowed(about),
        }
    }
}

/// The arguments for a command, by parameter name.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Args(BTreeMap<String, Value>);

impl Args {
    pub fn new() -> Self {
        Self::default()
    }

    /// The arguments with `name` set to `value`.
    pub fn with(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.set(name, value);
        self
    }

    pub fn set(&mut self, name: &str, value: impl Into<Value>) {
        self.0.insert(name.to_owned(), value.into());
    }

    pub fn value(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }

    /// The argument `name`, which the command needs.
    pub fn get<T: FromValue>(&self, name: &str) -> Result<T, CommandError> {
        self.opt(name)?
            .ok_or_else(|| CommandError::MissingArgument(name.to_owned()))
    }

    /// The argument `name`, if it was given.
    pub fn opt<T: FromValue>(&self, name: &str) -> Result<Option<T>, CommandError> {
        match self.0.get(name) {
            None => Ok(None),
            Some(value) => T::from_value(value)
                .map(Some)
                .ok_or_else(|| CommandError::WrongType {
                    name: name.to_owned(),
                    expected: T::TYPE,
                }),
        }
    }

    /// Checks the arguments against `params`: each required one is there,
    /// each one has its parameter's type, and there are no others.
    pub fn check(&self, params: &[Param]) -> Result<(), CommandError> {
        for param in params {
            match self.0.get(param.name.as_ref()) {
                None if !param.optional => {
                    return Err(CommandError::MissingArgument(param.name.to_string()));
                }
                Some(value) if !param.ty.accepts(value) => {
                    return Err(CommandError::WrongType {
                        name: param.name.to_string(),
                        expected: param.ty,
                    });
                }
                _ => {}
            }
        }
        match self
            .0
            .keys()
            .find(|name| !params.iter().any(|p| p.name == name.as_str()))
        {
            Some(name) => Err(CommandError::UnknownArgument(name.clone())),
            None => Ok(()),
        }
    }
}

/// A Rust type that an argument can be read as.
pub trait FromValue: Sized {
    /// The parameter type it reads.
    const TYPE: Type;

    fn from_value(value: &Value) -> Option<Self>;
}

impl FromValue for bool {
    const TYPE: Type = Type::Bool;

    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

impl FromValue for i64 {
    const TYPE: Type = Type::Int;

    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
}

impl FromValue for f64 {
    const TYPE: Type = Type::Number;

    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Number(n) if n.is_finite() => Some(*n),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
}

impl FromValue for String {
    const TYPE: Type = Type::Text;

    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        }
    }
}

impl FromValue for Beats {
    const TYPE: Type = Type::Beats;

    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Beats(beats) => Some(*beats),
            Value::Int(beats) => beats.checked_mul(Beats::TICKS_PER_BEAT).map(Beats::from_ticks),
            Value::Number(beats) if beats.is_finite() => Some(Beats::from_f64(*beats)),
            _ => None,
        }
    }
}

/// Reads an id, as a value of its own type or as a whole number.
macro_rules! id_from_value {
    ($id:ident, $variant:ident) => {
        impl FromValue for $id {
            const TYPE: Type = Type::$variant;

            fn from_value(value: &Value) -> Option<Self> {
                match value {
                    Value::$variant(id) => Some(*id),
                    Value::Int(id) => u32::try_from(*id).ok().map($id),
                    _ => None,
                }
            }
        }

        impl From<$id> for Value {
            fn from(id: $id) -> Self {
                Value::$variant(id)
            }
        }
    };
}

id_from_value!(TrackId, Track);
id_from_value!(ClipId, Clip);
id_from_value!(LocatorId, Locator);

impl<T: FromValue> FromValue for Vec<T> {
    const TYPE: Type = Type::List(&T::TYPE);

    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::List(values) => values.iter().map(T::from_value).collect(),
            _ => None,
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<f32> for Value {
    fn from(n: f32) -> Self {
        Value::Number(f64::from(n))
    }
}

impl From<&str> for Value {
    fn from(text: &str) -> Self {
        Value::Text(text.to_owned())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Self {
        Value::Text(text)
    }
}

impl From<Beats> for Value {
    fn from(beats: Beats) -> Self {
        Value::Beats(beats)
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(values: Vec<T>) -> Self {
        Value::List(values.into_iter().map(Into::into).collect())
    }
}

/// Why a command didn't run, or stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandError {
    /// No command has this id.
    Unknown(CommandId),
    /// The command doesn't apply now, such as Split with nothing selected:
    /// see [`When`](crate::When).
    NotNow(CommandId),
    MissingArgument(String),
    /// The command has no parameter of this name.
    UnknownArgument(String),
    WrongType {
        name: String,
        expected: Type,
    },
    /// The arguments name something the project doesn't have, or ask for
    /// something it can't do, as the message says.
    Invalid(String),
}

impl CommandError {
    /// An [`Invalid`](Self::Invalid) error with `message`.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(id) => write!(f, "there's no command called {id}"),
            Self::NotNow(id) => write!(f, "{id} doesn't apply now"),
            Self::MissingArgument(name) => write!(f, "the argument {name} is missing"),
            Self::UnknownArgument(name) => write!(f, "there's no argument called {name}"),
            Self::WrongType { name, expected } => {
                write!(f, "the argument {name} must be {expected}")
            }
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for CommandError {}

#[cfg(test)]
mod tests {
    use super::*;

    const PARAMS: [Param; 3] = [
        Param::required("clips", Type::CLIPS, "The clips to move."),
        Param::required("by", Type::Beats, "How far."),
        Param::optional("copy", Type::Bool, "Copies them instead."),
    ];

    #[test]
    fn arguments_are_checked_against_the_parameters() {
        let args = Args::new()
            .with("clips", vec![ClipId(1), ClipId(4)])
            .with("by", Beats::new(2));
        assert_eq!(args.check(&PARAMS), Ok(()));
        assert_eq!(args.get::<Vec<ClipId>>("clips"), Ok(vec![ClipId(1), ClipId(4)]));
        assert_eq!(args.get("by"), Ok(Beats::new(2)));
        assert_eq!(args.opt::<bool>("copy"), Ok(None));

        let missing = Args::new().with("clips", Vec::<ClipId>::new());
        assert_eq!(
            missing.check(&PARAMS),
            Err(CommandError::MissingArgument("by".into()))
        );
        let wrong = args.clone().with("copy", "yes");
        assert_eq!(
            wrong.check(&PARAMS),
            Err(CommandError::WrongType {
                name: "copy".into(),
                expected: Type::Bool,
            })
        );
        let extra = args.clone().with("tracks", 1_i64);
        assert_eq!(
            extra.check(&PARAMS),
            Err(CommandError::UnknownArgument("tracks".into()))
        );
        assert_eq!(
            wrong.check(&PARAMS).unwrap_err().to_string(),
            "the argument copy must be true or false"
        );
    }

    #[test]
    fn scripts_can_write_numbers_for_positions_and_ids() {
        let args = Args::new()
            .with("by", 1.5)
            .with("clips", Value::List(vec![Value::Int(3)]))
            .with("copy", true);
        assert_eq!(args.check(&PARAMS), Ok(()));
        assert_eq!(args.get("by"), Ok(Beats::from_ratio(3, 2)));
        assert_eq!(args.get::<Vec<ClipId>>("clips"), Ok(vec![ClipId(3)]));
        assert_eq!(Args::new().with("by", 2_i64).get("by"), Ok(Beats::new(2)));
        // But not ids that don't fit, or numbers that aren't.
        let bad = Args::new()
            .with("clips", Value::List(vec![Value::Int(-1)]))
            .with("by", f64::NAN);
        assert!(!Type::CLIPS.accepts(bad.value("clips").unwrap()));
        assert!(!Type::Beats.accepts(bad.value("by").unwrap()));
        assert_eq!(Type::CLIPS.to_string(), "a list, each a clip");
    }
}
