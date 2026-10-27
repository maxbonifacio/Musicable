//! The modulation commands: adding Macros, LFOs and Envelopes to the
//! modulator strips of tracks and the master, setting their parameters,
//! giving a Macro its knobs and buttons, and routing modulators' outputs to
//! the parameters they move, each routing with its amount, curve, scale,
//! lag and bypass. [`musicable_model::modulation`] says what each is.
//!
//! A modulator reaches the parameters of its own channel, a group's those
//! of the tracks that feed it too, and the master's every parameter, so a
//! new routing must be to a parameter in reach. A routing's amount is a
//! share of its parameter's range, from -1 to 1, along the parameter's
//! control, or a plugin parameter's own units.

use musicable_model::modulation::{
    ControlKind, Curve, Modulator, ModulatorId, ModulatorKind, Output, Routing, RoutingId,
};
use musicable_model::param::ParamAddress;
use musicable_model::{Chain, Project};

use crate::{Args, Command, CommandError, CommandId, Context, Param, Registry, Type, Value};

/// Adds a modulator to a track's modulator strip, or the master's.
pub const ADD: CommandId = CommandId::new("modulation.add");
/// Deletes a modulator, with its routings and those to its parameters.
pub const DELETE: CommandId = CommandId::new("modulation.delete");
/// Moves a modulator to another place in its strip.
pub const MOVE: CommandId = CommandId::new("modulation.move");
/// Names a modulator.
pub const RENAME: CommandId = CommandId::new("modulation.rename");
/// Sets a modulator's parameter.
pub const SET_PARAM: CommandId = CommandId::new("modulation.set_param");
/// Adds a knob or a button to a Macro.
pub const ADD_CONTROL: CommandId = CommandId::new("modulation.add_control");
/// Deletes one of a Macro's knobs or buttons.
pub const DELETE_CONTROL: CommandId = CommandId::new("modulation.delete_control");
/// Names one of a Macro's knobs or buttons.
pub const RENAME_CONTROL: CommandId = CommandId::new("modulation.rename_control");
/// Has a modulator's output move a parameter.
pub const ADD_ROUTING: CommandId = CommandId::new("modulation.add_routing");
/// Deletes a routing.
pub const DELETE_ROUTING: CommandId = CommandId::new("modulation.delete_routing");
/// Sets how far a routing moves its parameter.
pub const SET_AMOUNT: CommandId = CommandId::new("modulation.set_amount");
/// Sets how a routing bends its output.
pub const SET_CURVE: CommandId = CommandId::new("modulation.set_curve");
/// Has another output scale a routing's amount, or none.
pub const SET_SCALE: CommandId = CommandId::new("modulation.set_scale");
/// Sets how long a routing takes to follow its output.
pub const SET_LAG: CommandId = CommandId::new("modulation.set_lag");
/// Bypasses a routing, or brings it back.
pub const SET_BYPASS: CommandId = CommandId::new("modulation.set_bypass");

const MODULATOR: Param = Param::required("modulator", Type::Modulator, "The modulator.");
const MACRO: Param = Param::required("modulator", Type::Modulator, "The Macro.");
const CONTROL: Param = Param::required(
    "control",
    Type::Int,
    "The knob or button: its number, which its parameter and its output have.",
);
const ROUTING: Param = Param::required("routing", Type::Routing, "The routing.");
const NAME: Param = Param::required("name", Type::Text, "Its new name.");

/// The kinds' keys, as [`ADD`] takes them.
const KINDS: Type = Type::Choice(&["macro", "lfo", "envelope"]);
/// The kinds of a Macro's controls, as [`ADD_CONTROL`] takes them.
const CONTROL_KINDS: Type = Type::Choice(&["knob", "toggle", "momentary"]);
const CURVES: Type = Type::Choice(&Curve::KEYS);

/// How many steps a curve that becomes stepped has, unless it's told.
const STEPS: u32 = 4;

/// Registers the modulation commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(ADD, "Add Modulator").with_params(&[
            Param::required(
                "kind",
                KINDS,
                "Which: macro, knobs and buttons played by hand; lfo, a shape that \
                 repeats; or envelope, a shape that runs once each time it's started.",
            ),
            Param::optional(
                "track",
                Type::Track,
                "The track it goes on: the selected track without it.",
            ),
            Param::optional("master", Type::Bool, "Puts it on the master instead."),
            Param::optional(
                "index",
                Type::Int,
                "Where it goes in the strip: the end without it.",
            ),
            Param::optional(
                "name",
                Type::Text,
                "Its name: its kind's and the first number no modulator on the strip has \
                 with it, such as LFO 2, without it.",
            ),
        ]),
        |ctx, args| {
            let key: String = args.get("kind")?;
            let kind = ModulatorKind::from_key(&key).expect("the choices are the kinds' keys");
            let chain = crate::device::chain(
                ctx,
                args.opt("track")?,
                args.opt("master")?.unwrap_or(false),
                "modulator",
            )?;
            let index = index(args)?.unwrap_or(usize::MAX);
            let name = args.opt::<String>("name")?;
            let name = name
                .as_deref()
                .map(crate::locator::valid_name)
                .transpose()?;
            let id = ctx
                .document
                .transact(&format!("Add {}", kind.name()), |tx| {
                    let name = name.unwrap_or_else(|| {
                        let strip = tx.project().modulation(chain);
                        strip.expect("the chain is there").new_name(kind)
                    });
                    let modulator = tx.new_modulator(kind, name);
                    let id = modulator.id;
                    tx.add_modulator(chain, index, modulator);
                    id
                });
            Ok(Value::Modulator(id))
        },
    );
    registry.add(
        Command::new(DELETE, "Delete Modulator").with_params(&[MODULATOR]),
        |ctx, args| {
            let id = args.get("modulator")?;
            let name = format!("Delete {}", find(ctx.document.project(), id)?.1.name);
            ctx.document.transact(&name, |tx| {
                tx.remove_modulator(id);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(MOVE, "Move Modulator").with_params(&[
            MODULATOR,
            Param::required(
                "index",
                Type::Int,
                "Where it goes in its strip, from 0, counting the strip without it: the \
                 end if that's past it.",
            ),
        ]),
        |ctx, args| {
            let id = args.get("modulator")?;
            let name = format!("Move {}", find(ctx.document.project(), id)?.1.name);
            let index = index(args)?.expect("the index is required");
            ctx.document
                .transact(&name, |tx| tx.move_modulator(id, index));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(RENAME, "Rename Modulator").with_params(&[MODULATOR, NAME]),
        |ctx, args| {
            let id = args.get("modulator")?;
            find(ctx.document.project(), id)?;
            let name = crate::locator::valid_name(&args.get::<String>("name")?)?;
            ctx.document
                .transact("Rename Modulator", |tx| tx.set_modulator_name(id, &name));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_PARAM, "Set Modulator Parameter").with_params(&[
            MODULATOR,
            Param::required(
                "param",
                Type::Int,
                "The parameter: its place in an LFO's or an Envelope's table, from 0, as \
                 project-format.md numbers them, or a Macro's knob or button.",
            ),
            Param::required(
                "value",
                Type::Number,
                "Its value, in its own units, such as Hz or ms, kept in the parameter's \
                 range: from 0 to 1 for a knob, and 0 or 1 for a button.",
            ),
        ]),
        |ctx, args| {
            let id = args.get("modulator")?;
            let n = number(args.get("param")?, "parameter")?;
            let value: f64 = args.get("value")?;
            let (_, modulator) = find(ctx.document.project(), id)?;
            if modulator.param(n).is_none() {
                return Err(no_param(modulator, n));
            }
            let name = format!("Change {}", modulator.name);
            ctx.document.transact(&name, |tx| {
                // As if by hand: automation stops moving it.
                let param = ParamAddress::Modulator(id, n);
                tx.override_automation(param);
                tx.set_modulator_param(id, n, value);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(ADD_CONTROL, "Add Macro Control").with_params(&[
            MACRO,
            Param::optional(
                "kind",
                CONTROL_KINDS,
                "Which: knob, which turns from 0 to 1; toggle, a button each press turns \
                 on or off; or momentary, a button that's on while it's held. A knob \
                 without it.",
            ),
            Param::optional(
                "index",
                Type::Int,
                "Where it goes among the Macro's controls: the end without it.",
            ),
            Param::optional(
                "name",
                Type::Text,
                "Its name: Knob or Button and the first number free on the Macro without \
                 it.",
            ),
        ]),
        |ctx, args| {
            let id = args.get("modulator")?;
            let kind = match args.opt::<String>("kind")? {
                Some(key) => ControlKind::from_key(&key).expect("the choices are the kinds"),
                None => ControlKind::Knob,
            };
            let index = index(args)?.unwrap_or(usize::MAX);
            let name = args.opt::<String>("name")?;
            let name = name
                .as_deref()
                .map(crate::locator::valid_name)
                .transpose()?;
            macro_of(ctx.document.project(), id)?;
            let step = if kind.is_button() {
                "Add Button"
            } else {
                "Add Knob"
            };
            let control = ctx.document.transact(step, |tx| {
                let mut control = tx.new_control(id, kind).expect("the Macro is there");
                if let Some(name) = name {
                    control.name = name;
                }
                let number = control.id;
                tx.add_control(id, index, control);
                number
            });
            Ok(Value::Int(control.into()))
        },
    );
    registry.add(
        Command::new(DELETE_CONTROL, "Delete Macro Control").with_params(&[MACRO, CONTROL]),
        |ctx, args| {
            let id = args.get("modulator")?;
            let (control, name) = control(ctx.document.project(), id, args)?;
            ctx.document.transact(&format!("Delete {name}"), |tx| {
                tx.remove_control(id, control);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(RENAME_CONTROL, "Rename Macro Control").with_params(&[MACRO, CONTROL, NAME]),
        |ctx, args| {
            let id = args.get("modulator")?;
            let (control, _) = control(ctx.document.project(), id, args)?;
            let name = crate::locator::valid_name(&args.get::<String>("name")?)?;
            ctx.document.transact("Rename Macro Control", |tx| {
                tx.set_control_name(id, control, &name)
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(ADD_ROUTING, "Add Routing").with_params(&[
            Param::required(
                "modulator",
                Type::Modulator,
                "The modulator whose output moves the parameter.",
            ),
            Param::optional(
                "output",
                Type::Int,
                "Which of its outputs: a Macro's knob or button, by its number. Its first \
                 without it, which is an LFO's or an Envelope's only one.",
            ),
            Param::required(
                "param",
                Type::Param,
                "The parameter it moves, which the modulator must reach: one on its own \
                 channel, on a track feeding it if it's on a group, or anywhere if it's \
                 on the master.",
            ),
            Param::required(
                "amount",
                Type::Number,
                "How far it moves the parameter while the output is at 1: a share of the \
                 parameter's range, from -1 to 1, along its control, or for a plugin's \
                 parameter, in the plugin's own units. Below 0, it moves it the other \
                 way.",
            ),
        ]),
        |ctx, args| {
            let id = args.get("modulator")?;
            let param: ParamAddress = args.get("param")?;
            let amount: f64 = args.get("amount")?;
            let project = ctx.document.project();
            let output = output(project, id, args.opt("output")?)?;
            let (chain, _) = find(project, id)?;
            routable(project, chain, param)?;
            if let Some(routing) = project
                .modulation(chain)
                .and_then(|m| m.routing_between(output, param))
            {
                return Err(CommandError::invalid(format!(
                    "routing {} moves it from that output already",
                    routing.id.0
                )));
            }
            fits(project, param, amount)?;
            let routing = ctx.document.transact("Add Routing", |tx| {
                let routing = tx.new_routing(output, param, amount);
                tx.add_routing(chain, usize::MAX, routing);
                routing.id
            });
            Ok(Value::Routing(routing))
        },
    );
    registry.add(
        Command::new(DELETE_ROUTING, "Delete Routing").with_params(&[ROUTING]),
        |ctx, args| {
            let id = args.get("routing")?;
            routing(ctx.document.project(), id)?;
            ctx.document.transact("Delete Routing", |tx| {
                tx.remove_routing(id);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_AMOUNT, "Set Routing Amount").with_params(&[
            ROUTING,
            Param::required(
                "amount",
                Type::Number,
                "How far it moves its parameter while its output is at 1, as \
                 modulation.add_routing takes it.",
            ),
        ]),
        |ctx, args| {
            let amount: f64 = args.get("amount")?;
            set(ctx, args, "Set Routing Amount", |project, routing| {
                fits(project, routing.to, amount)?;
                Ok(Routing { amount, ..routing })
            })
        },
    );
    registry.add(
        Command::new(SET_CURVE, "Set Routing Curve").with_params(&[
            ROUTING,
            Param::required(
                "curve",
                CURVES,
                "How it bends its output: linear, straight; exponential, slowly at \
                 first; logarithmic, fast at first; s_curve, slowly at both ends; \
                 stepped, in even steps; absolute, its size without its sign; positive, \
                 only above 0; or negative, only below. A bipolar output bends evenly \
                 about 0.",
            ),
            Param::optional(
                "steps",
                Type::Int,
                "How many steps a stepped curve has, from 2 to 128: as many as it has, \
                 or 4, without it.",
            ),
        ]),
        |ctx, args| {
            let key: String = args.get("curve")?;
            let steps = args.opt::<i64>("steps")?;
            set(ctx, args, "Set Routing Curve", |_, routing| {
                let steps = match (steps, routing.curve) {
                    (Some(steps), _) => u32::try_from(steps).unwrap_or(0),
                    (None, Curve::Stepped(steps)) => steps,
                    (None, _) => STEPS,
                };
                let curve = Curve::from_key(&key, steps).expect("the choices are the keys");
                if !curve.is_valid() {
                    return Err(CommandError::invalid(format!(
                        "a stepped curve has from 2 to {} steps",
                        Curve::STEPS_MAX
                    )));
                }
                Ok(Routing { curve, ..routing })
            })
        },
    );
    registry.add(
        Command::new(SET_SCALE, "Set Routing Scale").with_params(&[
            ROUTING,
            Param::optional(
                "modulator",
                Type::Modulator,
                "The modulator whose output scales its amount, on the routing's channel, \
                 as velocity would: none without it.",
            ),
            Param::optional(
                "output",
                Type::Int,
                "Which of the modulator's outputs: its first without it.",
            ),
        ]),
        |ctx, args| {
            let scale = match args.opt("modulator")? {
                Some(id) => Some((id, output(ctx.document.project(), id, args.opt("output")?)?)),
                None => None,
            };
            set(ctx, args, "Set Routing Scale", |project, routing| {
                let (chain, ..) = project.find_routing(routing.id).expect("set finds it");
                let scale = match scale {
                    Some((id, output)) => {
                        if find(project, id)?.0 != chain {
                            return Err(CommandError::invalid(format!(
                                "modulator {} isn't on the routing's channel",
                                id.0
                            )));
                        }
                        Some(output)
                    }
                    None => None,
                };
                Ok(Routing { scale, ..routing })
            })
        },
    );
    registry.add(
        Command::new(SET_LAG, "Set Routing Lag").with_params(&[
            ROUTING,
            Param::required(
                "lag",
                Type::Number,
                "How long it takes to follow its output, in ms, from 0, at once, to 10000: \
                 about two thirds of the way in that time.",
            ),
        ]),
        |ctx, args| {
            let lag: f64 = args.get("lag")?;
            if !(0.0..=Routing::LAG_MAX).contains(&lag) {
                return Err(CommandError::invalid(format!(
                    "a lag is from 0 to {} ms",
                    Routing::LAG_MAX
                )));
            }
            set(ctx, args, "Set Routing Lag", |_, routing| {
                Ok(Routing { lag, ..routing })
            })
        },
    );
    registry.add(
        Command::new(SET_BYPASS, "Set Routing Bypass").with_params(&[
            ROUTING,
            Param::required(
                "bypassed",
                Type::Bool,
                "Whether it's bypassed: it moves nothing, and keeps its settings.",
            ),
        ]),
        |ctx, args| {
            let bypassed: bool = args.get("bypassed")?;
            let verb = if bypassed { "Bypass" } else { "Bring Back" };
            set(ctx, args, &format!("{verb} Routing"), |_, routing| {
                Ok(Routing {
                    bypassed,
                    ..routing
                })
            })
        },
    );
}

/// Gives the routing `args` name the settings `change` makes of its own,
/// as the step `name`.
fn set(
    ctx: &mut Context<'_>,
    args: &Args,
    name: &str,
    change: impl FnOnce(&Project, Routing) -> Result<Routing, CommandError>,
) -> Result<Value, CommandError> {
    let id = args.get("routing")?;
    let project = ctx.document.project();
    let changed = change(project, *routing(project, id)?)?;
    ctx.document.transact(name, |tx| tx.set_routing(changed));
    Ok(Value::None)
}

/// The `index` argument, if there's one: 0 for a negative one.
fn index(args: &Args) -> Result<Option<usize>, CommandError> {
    let index = args.opt::<i64>("index")?;
    Ok(index.map(|index| usize::try_from(index).unwrap_or(0)))
}

/// `n` as the number of a modulator's `what`.
fn number(n: i64, what: &str) -> Result<u32, CommandError> {
    u32::try_from(n).map_err(|_| CommandError::invalid(format!("there's no {what} {n}")))
}

/// The modulator with id `id`, and the channel it's on.
fn find(project: &Project, id: ModulatorId) -> Result<(Chain, &Modulator), CommandError> {
    match project.find_modulator(id) {
        Some((chain, _, modulator)) => Ok((chain, modulator)),
        None => Err(CommandError::invalid(format!(
            "there's no modulator {}",
            id.0
        ))),
    }
}

/// The Macro with id `id`.
fn macro_of(project: &Project, id: ModulatorId) -> Result<&Modulator, CommandError> {
    let (_, modulator) = find(project, id)?;
    match modulator.kind {
        ModulatorKind::Macro => Ok(modulator),
        kind => Err(CommandError::invalid(format!(
            "{} is an {}, which has no knobs or buttons of its own",
            modulator.name,
            kind.name()
        ))),
    }
}

/// The number and the name of the Macro's control that `args` name.
fn control(project: &Project, id: ModulatorId, args: &Args) -> Result<(u32, String), CommandError> {
    let modulator = macro_of(project, id)?;
    let n = number(args.get("control")?, "control")?;
    match modulator.control(n) {
        Some(control) => Ok((n, control.name.clone())),
        None => Err(CommandError::invalid(format!(
            "{} has no control {n}",
            modulator.name
        ))),
    }
}

/// Output `n` of the modulator with id `id`, or its first without `n`.
fn output(project: &Project, id: ModulatorId, n: Option<i64>) -> Result<Output, CommandError> {
    let (_, modulator) = find(project, id)?;
    let n = match n {
        Some(n) => number(n, "output")?,
        None => match modulator.outputs().first() {
            Some(&first) => first,
            None => {
                return Err(CommandError::invalid(format!(
                    "{} has no knobs or buttons to route",
                    modulator.name
                )));
            }
        },
    };
    match modulator.has_output(n) {
        true => Ok(Output::new(id, n)),
        false => Err(CommandError::invalid(format!(
            "{} has no output {n}",
            modulator.name
        ))),
    }
}

/// The routing with id `id`.
fn routing(project: &Project, id: RoutingId) -> Result<&Routing, CommandError> {
    match project.find_routing(id) {
        Some((_, _, routing)) => Ok(routing),
        None => Err(CommandError::invalid(format!(
            "there's no routing {}",
            id.0
        ))),
    }
}

/// Whether a modulator on `chain` can move the parameter at `param`: the
/// project has it, modulators can move it, and it's in reach.
fn routable(project: &Project, chain: Chain, param: ParamAddress) -> Result<(), CommandError> {
    let text = crate::param_text(param);
    if !project.has_param(param) {
        return Err(CommandError::invalid(format!("the project has no {text}")));
    }
    if !project.can_modulate(param) {
        return Err(CommandError::invalid(format!(
            "modulators can't move {text}"
        )));
    }
    if !project.in_reach(chain, param) {
        return Err(CommandError::invalid(format!(
            "{text} is out of the modulator's reach: its own channel's parameters, those \
             of the tracks feeding a group it's on, and on the master, every one"
        )));
    }
    Ok(())
}

/// Whether `amount` suits a routing to the parameter at `param`.
fn fits(project: &Project, param: ParamAddress, amount: f64) -> Result<(), CommandError> {
    match project.amount_fits(param, amount) {
        true => Ok(()),
        false if amount.is_finite() => Err(CommandError::invalid(
            "an amount is from -1 to 1: a share of the parameter's range",
        )),
        false => Err(CommandError::invalid("an amount must be a number")),
    }
}

fn no_param(modulator: &Modulator, n: u32) -> CommandError {
    let message = match modulator.kind {
        ModulatorKind::Macro => format!("{} has no control {n}", modulator.name),
        kind => format!(
            "an {} has no parameter {n}: they're 0 to {}",
            kind.name(),
            kind.params().len() - 1
        ),
    };
    CommandError::invalid(message)
}

#[cfg(test)]
mod tests {
    use musicable_model::modulation::{envelope, lfo};
    use musicable_model::native::{NativeDevice, NativeKind, eq};
    use musicable_model::{DeviceKind, TrackId, TrackKind};

    use super::*;
    use crate::testing::Setup;

    #[test]
    fn the_choices_are_the_keys() {
        let keys = |ty: Type| match ty {
            Type::Choice(choices) => choices.to_vec(),
            _ => unreachable!("a choice"),
        };
        let kinds: Vec<_> = ModulatorKind::ALL.iter().map(|k| k.key()).collect();
        assert_eq!(keys(KINDS), kinds);
        let controls: Vec<_> = ControlKind::ALL.iter().map(|k| k.key()).collect();
        assert_eq!(keys(CONTROL_KINDS), controls);
    }

    /// A project with a group feeding the master, a bass track feeding the
    /// group with an EQ on it, and a pad track.
    fn project() -> (Project, [TrackId; 3]) {
        let mut project = Project::new();
        let group = project.add_track("Group", TrackKind::Group);
        let bass = project.add_track("Bass", TrackKind::Audio);
        let pad = project.add_track("Pad", TrackKind::Audio);
        project.track_mut(bass).unwrap().output = musicable_model::mixer::Bus::Group(group);
        let eq = project.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Eq)));
        project.track_mut(bass).unwrap().devices.push(eq);
        (project, [group, bass, pad])
    }

    fn add(setup: &mut Setup, kind: &str, track: TrackId) -> ModulatorId {
        let args = Args::new().with("kind", kind).with("track", track);
        match setup.run_with(&ADD, args).unwrap() {
            Value::Modulator(id) => id,
            other => panic!("{other:?}"),
        }
    }

    fn route(setup: &mut Setup, from: ModulatorId, param: ParamAddress, amount: f64) -> Value {
        let args = Args::new()
            .with("modulator", Value::Modulator(from))
            .with("param", param)
            .with("amount", amount);
        setup
            .run_with(&ADD_ROUTING, args)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn modulators_go_on_strips_with_names_of_their_own() {
        let (project, [_, bass, _]) = project();
        let mut setup = Setup::new(project);
        let first = add(&mut setup, "lfo", bass);
        let second = add(&mut setup, "lfo", bass);
        let envelope = add(&mut setup, "envelope", bass);
        let strip = |setup: &Setup| -> Vec<String> {
            let modulation = setup.document.project().modulation(Chain::Track(bass));
            (modulation.unwrap().modulators.iter())
                .map(|m| m.name.clone())
                .collect()
        };
        assert_eq!(strip(&setup), ["LFO 1", "LFO 2", "Envelope 1"]);
        assert_eq!(setup.document.undo_name(), Some("Add Envelope"));

        let to = |index: i64| {
            Args::new()
                .with("modulator", Value::Modulator(envelope))
                .with("index", index)
        };
        setup.run_with(&MOVE, to(0)).unwrap();
        assert_eq!(strip(&setup), ["Envelope 1", "LFO 1", "LFO 2"]);
        setup.run_with(&MOVE, to(9)).unwrap();
        assert_eq!(strip(&setup), ["LFO 1", "LFO 2", "Envelope 1"]);
        let rename = Args::new()
            .with("modulator", Value::Modulator(first))
            .with("name", " Wobble ");
        setup.run_with(&RENAME, rename.clone()).unwrap();
        assert!(setup.run_with(&RENAME, rename.with("name", " ")).is_err());
        let delete = Args::new().with("modulator", Value::Modulator(second));
        setup.run_with(&DELETE, delete.clone()).unwrap();
        assert_eq!(strip(&setup), ["Wobble", "Envelope 1"]);
        assert_eq!(setup.document.undo_name(), Some("Delete LFO 2"));
        assert!(setup.run_with(&DELETE, delete).is_err());

        // The master has a strip too, and the selected track is the default.
        let master = Args::new().with("kind", "macro").with("master", true);
        setup.run_with(&ADD, master).unwrap();
        let macros = &setup.document.project().master.modulation.modulators;
        assert_eq!(macros[0].name, "Macro 1");
        assert!(
            setup
                .run_with(&ADD, Args::new().with("kind", "lfo"))
                .is_err()
        );
        let missing = Args::new().with("kind", "lfo").with("track", TrackId(9));
        assert!(setup.run_with(&ADD, missing).is_err());
    }

    #[test]
    fn parameters_are_set_as_by_hand_within_their_ranges() {
        let (project, [_, bass, _]) = project();
        let mut setup = Setup::new(project);
        let lfo = add(&mut setup, "lfo", bass);
        let rate = ParamAddress::Modulator(lfo, lfo::RATE as u32);
        setup.document.transact("Draw", |tx| {
            let point = musicable_model::automation::Point::new(Default::default(), 2.0);
            tx.set_lane_points(rate, vec![point]);
        });
        let set = |n: usize, value: f64| {
            Args::new()
                .with("modulator", Value::Modulator(lfo))
                .with("param", n as i64)
                .with("value", value)
        };
        setup.run_with(&SET_PARAM, set(lfo::RATE, 1000.0)).unwrap();
        let project = setup.document.project();
        let value = |n: usize| project.find_modulator(lfo).unwrap().2.value(n as u32);
        assert_eq!(value(lfo::RATE), Some(100.0));
        assert!(!project.is_automated(rate));
        assert_eq!(setup.document.undo_name(), Some("Change LFO 1"));
        setup.run_with(&SET_PARAM, set(lfo::SHAPE, 2.4)).unwrap();
        let shape = setup
            .document
            .project()
            .find_modulator(lfo)
            .unwrap()
            .2
            .value(0);
        assert_eq!(shape, Some(2.0));
        assert!(setup.run_with(&SET_PARAM, set(99, 0.0)).is_err());
    }

    #[test]
    fn macros_have_knobs_and_buttons_by_number() {
        let (project, [_, bass, _]) = project();
        let mut setup = Setup::new(project);
        let macros = add(&mut setup, "macro", bass);
        let on = |args: Args| args.with("modulator", Value::Modulator(macros));
        let button = setup
            .run_with(&ADD_CONTROL, on(Args::new().with("kind", "momentary")))
            .unwrap();
        assert_eq!(button, Value::Int(1));
        assert_eq!(setup.document.undo_name(), Some("Add Button"));
        let named = on(Args::new().with("name", "Drive").with("index", 0));
        assert_eq!(setup.run_with(&ADD_CONTROL, named).unwrap(), Value::Int(2));
        let names = |setup: &Setup| -> Vec<String> {
            let (_, _, m) = setup.document.project().find_modulator(macros).unwrap();
            m.controls.iter().map(|c| c.name.clone()).collect()
        };
        assert_eq!(names(&setup), ["Drive", "Knob 1", "Button 1"]);

        let rename = on(Args::new().with("control", 1).with("name", "Hold"));
        setup.run_with(&RENAME_CONTROL, rename).unwrap();
        let delete = on(Args::new().with("control", 0));
        setup.run_with(&DELETE_CONTROL, delete.clone()).unwrap();
        assert_eq!(names(&setup), ["Drive", "Hold"]);
        assert_eq!(setup.document.undo_name(), Some("Delete Knob 1"));
        assert!(setup.run_with(&DELETE_CONTROL, delete).is_err());
        // Its numbers aren't handed out again.
        assert!(setup.document.undo());
        assert!(setup.document.undo());
        let again = setup.run_with(&ADD_CONTROL, on(Args::new())).unwrap();
        assert_eq!(again, Value::Int(3));

        let lfo = add(&mut setup, "lfo", bass);
        let knob = Args::new().with("modulator", Value::Modulator(lfo));
        assert!(setup.run_with(&ADD_CONTROL, knob).is_err());
    }

    #[test]
    fn routings_reach_their_channel_and_what_feeds_it() {
        let (project, [group, bass, pad]) = project();
        let eq = project.tracks[1].devices[0].id;
        let mut setup = Setup::new(project);
        let lfo = add(&mut setup, "lfo", bass);
        let frequency = ParamAddress::Device(eq, eq::FREQ as u32);
        let routed = route(&mut setup, lfo, frequency, 0.5);
        let Value::Routing(routing) = routed else {
            panic!("{routed:?}");
        };
        let found = |setup: &Setup| *setup.document.project().find_routing(routing).unwrap().2;
        assert_eq!(found(&setup).amount, 0.5);
        assert_eq!(setup.document.undo_name(), Some("Add Routing"));

        let args = |from: ModulatorId, param: ParamAddress, amount: f64| {
            Args::new()
                .with("modulator", Value::Modulator(from))
                .with("param", param)
                .with("amount", amount)
        };
        // Once from each output, within the range's share, and in reach.
        assert!(
            setup
                .run_with(&ADD_ROUTING, args(lfo, frequency, 0.25))
                .is_err()
        );
        let gain = ParamAddress::Volume(Chain::Track(bass));
        assert!(setup.run_with(&ADD_ROUTING, args(lfo, gain, 1.5)).is_err());
        let on_pad = ParamAddress::Volume(Chain::Track(pad));
        assert!(
            setup
                .run_with(&ADD_ROUTING, args(lfo, on_pad, 0.5))
                .is_err()
        );
        let shape = ParamAddress::Modulator(lfo, lfo::SHAPE as u32);
        assert!(setup.run_with(&ADD_ROUTING, args(lfo, shape, 0.5)).is_err());
        let group_lfo = add(&mut setup, "lfo", group);
        route(&mut setup, group_lfo, gain, -0.25);
        let master = Args::new().with("kind", "envelope").with("master", true);
        let Value::Modulator(envelope) = setup.run_with(&ADD, master).unwrap() else {
            unreachable!("a modulator");
        };
        route(&mut setup, envelope, on_pad, 1.0);

        // Settings, one at a time.
        let with = |args: Args| args.with("routing", Value::Routing(routing));
        setup
            .run_with(&SET_AMOUNT, with(Args::new().with("amount", -1.0)))
            .unwrap();
        assert!(
            setup
                .run_with(&SET_AMOUNT, with(Args::new().with("amount", 2.0)))
                .is_err()
        );
        setup
            .run_with(&SET_CURVE, with(Args::new().with("curve", "stepped")))
            .unwrap();
        assert_eq!(found(&setup).curve, Curve::Stepped(STEPS));
        let steps = with(Args::new().with("curve", "stepped").with("steps", 1));
        assert!(setup.run_with(&SET_CURVE, steps).is_err());
        setup
            .run_with(&SET_CURVE, with(Args::new().with("curve", "s_curve")))
            .unwrap();
        let macros = add(&mut setup, "macro", bass);
        let scale = with(Args::new().with("modulator", Value::Modulator(macros)));
        setup.run_with(&SET_SCALE, scale).unwrap();
        assert_eq!(found(&setup).scale, Some(Output::new(macros, 0)));
        let elsewhere = with(Args::new().with("modulator", Value::Modulator(group_lfo)));
        assert!(setup.run_with(&SET_SCALE, elsewhere).is_err());
        setup
            .run_with(&SET_LAG, with(Args::new().with("lag", 250.0)))
            .unwrap();
        assert!(
            setup
                .run_with(&SET_LAG, with(Args::new().with("lag", -1.0)))
                .is_err()
        );
        setup
            .run_with(&SET_BYPASS, with(Args::new().with("bypassed", true)))
            .unwrap();
        assert_eq!(setup.document.undo_name(), Some("Bypass Routing"));
        let settings = found(&setup);
        assert_eq!(
            (
                settings.amount,
                settings.curve,
                settings.lag,
                settings.bypassed
            ),
            (-1.0, Curve::SCurve, 250.0, true)
        );

        // Deleting the Macro takes it away as the scale, in the same step.
        let delete = Args::new().with("modulator", Value::Modulator(macros));
        setup.run_with(&DELETE, delete).unwrap();
        assert_eq!(found(&setup).scale, None);
        assert!(setup.document.undo());
        assert_eq!(found(&setup).scale, Some(Output::new(macros, 0)));

        setup.run_with(&DELETE_ROUTING, with(Args::new())).unwrap();
        assert!(setup.document.project().find_routing(routing).is_none());
        assert!(setup.run_with(&DELETE_ROUTING, with(Args::new())).is_err());
    }

    #[test]
    fn what_goes_takes_its_routings_and_its_lanes_with_it() {
        let (project, [group, bass, _]) = project();
        let eq = project.tracks[1].devices[0].id;
        let mut setup = Setup::new(project);
        let lfo = add(&mut setup, "lfo", group);
        let envelope = add(&mut setup, "envelope", bass);
        let frequency = ParamAddress::Device(eq, eq::FREQ as u32);
        let attack = ParamAddress::Modulator(envelope, envelope::ATTACK as u32);
        route(&mut setup, lfo, frequency, 0.5);
        route(&mut setup, lfo, attack, 0.5);
        route(&mut setup, envelope, frequency, 0.25);
        setup.document.transact("Draw", |tx| {
            let point = musicable_model::automation::Point::new(Default::default(), 20.0);
            tx.set_lane_points(attack, vec![point]);
        });
        let routings = |setup: &Setup| -> usize {
            (setup.document.project().modulations())
                .map(|(_, m)| m.routings.len())
                .sum()
        };
        assert_eq!(routings(&setup), 3);
        let before = setup.document.project().clone();

        // The EQ takes the routings to it, on its track and on the group.
        setup.document.transact("Delete EQ", |tx| {
            tx.remove_device(eq);
        });
        assert_eq!(routings(&setup), 1);
        assert!(setup.document.undo());
        assert_eq!(setup.document.project(), &before);

        // The Envelope takes the group's routing to it, and its lane.
        let delete = Args::new().with("modulator", Value::Modulator(envelope));
        setup.run_with(&DELETE, delete).unwrap();
        assert_eq!(routings(&setup), 1);
        assert!(setup.document.project().automation.is_empty());
        assert!(setup.document.undo());
        assert_eq!(setup.document.project(), &before);

        // The bass track takes the group's routings to its parameters.
        crate::track::delete(&mut setup.document, &[bass]);
        assert_eq!(routings(&setup), 0);
        assert!(setup.document.undo());
        assert_eq!(setup.document.project(), &before);
    }
}
