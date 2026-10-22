//! The locator commands: named places in the song, shown on the ruler,
//! that the playhead jumps to.

use musicable_model::time::Beats;
use musicable_model::{LocatorId, Project};

use crate::{Command, CommandError, CommandId, Context, Param, Registry, Type, Value, When};

/// Adds a locator where the playhead is while playing, or else at the start
/// marker, as Live's Set button does.
pub const ADD: CommandId = CommandId::new("locator.add");
/// Jumps to the locator before the playhead, or before the start marker
/// while stopped.
pub const PREVIOUS: CommandId = CommandId::new("locator.previous");
/// Jumps to the locator after the playhead, or after the start marker while
/// stopped.
pub const NEXT: CommandId = CommandId::new("locator.next");
/// Moves a locator.
pub const MOVE: CommandId = CommandId::new("locator.move");
/// Names a locator.
pub const RENAME: CommandId = CommandId::new("locator.rename");
/// Deletes a locator.
pub const DELETE: CommandId = CommandId::new("locator.delete");

const LOCATOR: Param = Param::required("locator", Type::Locator, "The locator.");

/// Registers the locator commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(ADD, "Add Locator").with_params(&[
            Param::optional(
                "at",
                Type::Beats,
                "Where: the playhead while playing, or else the start marker.",
            ),
            Param::optional(
                "name",
                Type::Text,
                "Its name: a number, counting the locators, without it.",
            ),
        ]),
        |ctx, args| {
            let at = match args.opt("at")? {
                Some(at) => at,
                None => here(ctx),
            };
            let at = Beats::max(at, Beats::ZERO);
            let name = match args.opt::<String>("name")? {
                Some(name) => valid_name(&name)?,
                None => (ctx.document.project().locators.len() + 1).to_string(),
            };
            let project = ctx.document.project();
            // One locator to a place.
            if let Some(locator) = project.locators.iter().find(|l| l.at == at) {
                return Ok(locator.id.into());
            }
            let index = project.locators.iter().filter(|l| l.at < at).count();
            let id = ctx.document.transact("Add Locator", |tx| {
                let locator = tx.new_locator(name, at);
                let id = locator.id;
                tx.add_locator(index, locator);
                id
            });
            Ok(id.into())
        },
    );
    for (id, title, step) in [
        (PREVIOUS, "Previous Locator", Step::Back),
        (NEXT, "Next Locator", Step::On),
    ] {
        registry.add(Command::new(id, title).when(When::HasLocators), move |ctx, _| {
            let from = here(ctx);
            if let Some(at) = step.from(ctx.document.project(), from) {
                crate::transport::jump(ctx, at);
            }
            Ok(Value::None)
        });
    }
    registry.add(
        Command::new(MOVE, "Move Locator").with_params(&[
            LOCATOR,
            Param::required("at", Type::Beats, "Where it goes."),
        ]),
        |ctx, args| {
            let locator = existing(ctx, args.get("locator")?)?;
            let at = Beats::max(args.get("at")?, Beats::ZERO);
            ctx.document
                .transact("Move Locator", |tx| tx.set_locator_at(locator, at));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(RENAME, "Rename Locator").with_params(&[
            LOCATOR,
            Param::required("name", Type::Text, "Its new name."),
        ]),
        |ctx, args| {
            let locator = existing(ctx, args.get("locator")?)?;
            let name = valid_name(&args.get::<String>("name")?)?;
            ctx.document
                .transact("Rename Locator", |tx| tx.set_locator_name(locator, &name));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(DELETE, "Delete Locator").with_params(&[LOCATOR]),
        |ctx, args| {
            let locator = existing(ctx, args.get("locator")?)?;
            ctx.document.transact("Delete Locator", |tx| {
                tx.remove_locator(locator);
            });
            Ok(Value::None)
        },
    );
}

/// Where locators go and jump from: the playhead while playing, or else the
/// start marker.
fn here(ctx: &Context<'_>) -> Beats {
    if ctx.transport.is_playing() {
        ctx.transport.playhead()
    } else {
        ctx.document.editor().start_marker
    }
}

#[derive(Debug, Clone, Copy)]
enum Step {
    Back,
    On,
}

impl Step {
    /// The position of the nearest locator before `from`, or after it.
    fn from(self, project: &Project, from: Beats) -> Option<Beats> {
        let places = project.locators.iter().map(|l| l.at);
        match self {
            Step::Back => places.filter(|&at| at < from).max(),
            Step::On => places.filter(|&at| at > from).min(),
        }
    }
}

/// `locator`, if the project has it.
fn existing(ctx: &Context<'_>, locator: LocatorId) -> Result<LocatorId, CommandError> {
    match ctx.document.project().locator(locator) {
        Some(_) => Ok(locator),
        None => Err(CommandError::invalid(format!(
            "there's no locator {}",
            locator.0
        ))),
    }
}

/// `name` without the spaces around it, if that leaves something.
pub(crate) fn valid_name(name: &str) -> Result<String, CommandError> {
    match name.trim() {
        "" => Err(CommandError::invalid("a name can't be empty")),
        name => Ok(name.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Args;
    use crate::testing::Setup;

    fn places(setup: &Setup) -> Vec<(String, i64)> {
        let locators = &setup.document.project().locators;
        locators
            .iter()
            .map(|l| (l.name.clone(), l.at.ticks() / Beats::TICKS_PER_BEAT))
            .collect()
    }

    #[test]
    fn locators_go_at_the_start_marker_or_the_playhead() {
        let mut setup = Setup::new(Project::new());
        assert!(!setup.applies(&NEXT));
        setup.document.editor_mut().start_marker = Beats::new(8);
        setup.run(&ADD);
        // Playing, they go where the playhead is.
        setup.transport.playing = true;
        setup.transport.playhead = Beats::new(4);
        setup.run(&ADD);
        // Not twice in one place.
        setup.run(&ADD);
        let args = Args::new().with("at", Beats::new(16)).with("name", " Chorus ");
        let chorus = setup.run_with(&ADD, args).unwrap();
        assert_eq!(chorus, Value::Locator(LocatorId(2)));
        // In order along the song.
        assert_eq!(
            places(&setup),
            [("2".into(), 4), ("1".into(), 8), ("Chorus".into(), 16)]
        );
        assert_eq!(
            setup.committed_names(),
            ["Add Locator", "Add Locator", "Add Locator"]
        );
    }

    #[test]
    fn previous_and_next_jump_between_locators() {
        let mut setup = Setup::new(Project::new());
        for beat in [4, 12] {
            let args = Args::new().with("at", Beats::new(beat));
            setup.run_with(&ADD, args).unwrap();
        }
        setup.run(&NEXT);
        assert_eq!(setup.document.editor().start_marker, Beats::new(4));
        setup.run(&NEXT);
        setup.run(&NEXT);
        assert_eq!(setup.document.editor().start_marker, Beats::new(12));
        setup.run(&PREVIOUS);
        setup.run(&PREVIOUS);
        assert_eq!(setup.document.editor().start_marker, Beats::new(4));
        // Playing, from the playhead.
        setup.transport.playing = true;
        setup.transport.playhead = Beats::new(13);
        setup.run(&PREVIOUS);
        assert_eq!(setup.transport.playhead, Beats::new(12));
        assert_eq!(
            setup.transport.calls,
            ["locate 4", "locate 12", "locate 4", "locate 12"]
        );
    }

    #[test]
    fn locators_move_rename_and_go() {
        let mut setup = Setup::new(Project::new());
        let id = setup.run_with(&ADD, Args::new()).unwrap();
        let args = |more: &[(&str, Value)]| {
            more.iter()
                .fold(Args::new().with("locator", id.clone()), |args, (k, v)| {
                    args.with(k, v.clone())
                })
        };
        setup
            .run_with(&MOVE, args(&[("at", Value::Int(-3))]))
            .unwrap();
        setup
            .run_with(&MOVE, args(&[("at", Value::Int(6))]))
            .unwrap();
        setup
            .run_with(&RENAME, args(&[("name", "Verse".into())]))
            .unwrap();
        assert_eq!(
            setup.run_with(&RENAME, args(&[("name", "  ".into())])),
            Err(CommandError::invalid("a name can't be empty"))
        );
        assert_eq!(places(&setup), [("Verse".into(), 6)]);
        setup.run_with(&DELETE, args(&[])).unwrap();
        assert_eq!(places(&setup), []);
        assert_eq!(
            setup.run_with(&DELETE, args(&[])),
            Err(CommandError::invalid("there's no locator 0"))
        );
        // Moving before the song's start stops at it, where the locator was:
        // that move changed nothing.
        assert_eq!(
            setup.committed_names(),
            ["Add Locator", "Move Locator", "Rename Locator", "Delete Locator"]
        );
    }
}
