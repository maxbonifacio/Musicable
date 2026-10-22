//! The menus that list the commands, and how commands are named in them and
//! in the command palette.

use musicable_commands::{
    Command, CommandId, Registry, arrange, automation, edit, file, locator, options, track,
    transport, view,
};

/// A menu's title, the area of commands it lists, and its commands in order.
/// `None` is a separator.
type MenuSpec = (&'static str, &'static str, &'static [Option<CommandId>]);

/// The menus that come first, before those for other areas. A command missing
/// from its area's menu goes at the end of it, and an area of commands
/// without a menu gets one after these, so every command can be reached.
/// Commands that need arguments, which menus can't give, are left out.
const MENUS: [MenuSpec; 7] = [
    (
        "File",
        "file",
        &[
            Some(file::NEW),
            Some(file::NEW_DEMO),
            Some(file::OPEN),
            None,
            Some(file::SAVE),
            Some(file::SAVE_AS),
            None,
            Some(file::IMPORT_AUDIO),
            None,
            Some(file::QUIT),
        ],
    ),
    (
        "Edit",
        "edit",
        &[
            Some(edit::UNDO),
            Some(edit::REDO),
            None,
            Some(edit::CUT),
            Some(edit::COPY),
            Some(edit::PASTE),
            Some(edit::DUPLICATE),
            Some(edit::DELETE),
            None,
            Some(edit::SPLIT),
            Some(edit::RENAME),
            None,
            Some(edit::SELECT_ALL),
        ],
    ),
    (
        "Track",
        "track",
        &[
            Some(track::NEW_AUDIO_TRACK),
            Some(track::NEW_MIDI_TRACK),
            Some(track::NEW_RETURN_TRACK),
            Some(track::NEW_GROUP_TRACK),
            None,
            Some(track::DELETE),
        ],
    ),
    ("Clip", "clip", &[Some(arrange::INSERT_MIDI_CLIP)]),
    (
        "Automation",
        "automation",
        &[Some(automation::SHOW), Some(automation::RE_ENABLE)],
    ),
    (
        "Locator",
        "locator",
        &[
            Some(locator::ADD),
            Some(locator::PREVIOUS),
            Some(locator::NEXT),
        ],
    ),
    (
        "Transport",
        "transport",
        &[
            Some(transport::PLAY_STOP),
            Some(transport::PLAY),
            Some(transport::STOP),
            Some(transport::GO_TO_START),
            None,
            Some(transport::RECORD),
            Some(transport::TOGGLE_PUNCH_IN),
            Some(transport::TOGGLE_PUNCH_OUT),
            None,
            Some(transport::TOGGLE_LOOP),
            Some(transport::TOGGLE_METRONOME),
            None,
            Some(transport::TEMPO_UP),
            Some(transport::TEMPO_DOWN),
        ],
    ),
];

/// The menus that come last, after those for other areas.
const LAST_MENUS: [MenuSpec; 2] = [
    (
        "View",
        "view",
        &[
            Some(view::TOGGLE_MIXER),
            Some(view::TOGGLE_DEVICES),
            Some(view::TOGGLE_BROWSER),
            None,
            Some(view::ZOOM_IN),
            Some(view::ZOOM_OUT),
            Some(view::ZOOM_TO_SELECTION),
            Some(view::TOGGLE_FOLLOW),
            Some(view::TOGGLE_TAKE_LANES),
            None,
            Some(view::NARROW_GRID),
            Some(view::WIDEN_GRID),
            Some(view::TRIPLET_GRID),
            Some(view::FIXED_GRID),
            Some(view::TOGGLE_SNAP),
            None,
            Some(view::COMMAND_PALETTE),
        ],
    ),
    (
        "Options",
        "options",
        &[
            Some(options::AUDIO_SETTINGS),
            Some(options::KEYBOARD_SHORTCUTS),
            None,
            Some(options::RESCAN_PLUGINS),
        ],
    ),
];

/// A menu of commands.
#[derive(Debug)]
pub(crate) struct Menu<'a> {
    pub title: String,
    /// The commands in order, with `None` for a separator.
    pub items: Vec<Option<&'a Command>>,
}

/// Every command in `commands` that runs without arguments, in the menus
/// the menu bar shows. A menu without commands is left out.
pub(crate) fn menus(commands: &Registry) -> Vec<Menu<'_>> {
    let listed = |area: &str| MENUS.iter().chain(&LAST_MENUS).any(|m| m.1 == area);
    let offered = || commands.iter().filter(|c| !c.needs_arguments());
    let mut others: Vec<&str> = offered()
        .map(|c| area_of(&c.id))
        .filter(|area| !listed(area))
        .collect();
    others.dedup();
    let others = others.into_iter().map(|area| Menu {
        title: capitalized(area),
        items: offered()
            .filter(|c| area_of(&c.id) == area)
            .map(Some)
            .collect(),
    });
    MENUS
        .iter()
        .map(|spec| menu(commands, spec))
        .chain(others)
        .chain(LAST_MENUS.iter().map(|spec| menu(commands, spec)))
        .filter(|menu| menu.items.iter().any(Option::is_some))
        .collect()
}

fn menu<'a>(commands: &'a Registry, &(title, area, items): &MenuSpec) -> Menu<'a> {
    let mut listed: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            Some(id) => commands.get(id).filter(|c| !c.needs_arguments()).map(Some),
            None => Some(None),
        })
        .collect();
    let rest = commands.iter().filter(|c| {
        area_of(&c.id) == area && !c.needs_arguments() && !items.contains(&Some(c.id.clone()))
    });
    listed.push(None);
    listed.extend(rest.map(Some));
    // No separator first, last, or after another.
    let mut tidy: Vec<Option<&Command>> = Vec::with_capacity(listed.len());
    for item in listed {
        if item.is_some() || tidy.last().is_some_and(Option::is_some) {
            tidy.push(item);
        }
    }
    if tidy.last() == Some(&None) {
        tidy.pop();
    }
    Menu {
        title: title.to_owned(),
        items: tidy,
    }
}

/// The area a command belongs to: `transport` for `transport.play`.
pub(crate) fn area_of(id: &CommandId) -> &str {
    id.as_str().split('.').next().unwrap_or_default()
}

/// A command as the command palette names it, with its area:
/// "Transport: Toggle Loop".
pub(crate) fn label(command: &Command) -> String {
    format!("{}: {}", capitalized(area_of(&command.id)), command.title)
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(menus: &[Menu<'_>]) -> Vec<String> {
        menus.iter().map(|menu| menu.title.clone()).collect()
    }

    #[test]
    fn every_command_is_in_a_menu() {
        let mut commands = Registry::new();
        musicable_commands::register_all(&mut commands);
        commands.register("transport.later", "Later", |_| {});
        commands.register("macro.later", "Later", |_| {});

        let menus = menus(&commands);
        // Areas without a menu of their own come before View and Options,
        // and the mixer's commands all need arguments.
        assert_eq!(
            titles(&menus),
            [
                "File",
                "Edit",
                "Track",
                "Clip",
                "Automation",
                "Locator",
                "Transport",
                "Macro",
                "View",
                "Options"
            ]
        );
        let listed: Vec<_> = menus
            .iter()
            .flat_map(|menu| menu.items.iter().flatten())
            .map(|command| command.id.clone())
            .collect();
        let offered: Vec<_> = commands.iter().filter(|c| !c.needs_arguments()).collect();
        assert_eq!(listed.len(), offered.len());
        for command in offered {
            assert!(listed.contains(&command.id), "{}", command.id);
        }
        assert!(!listed.contains(&arrange::MOVE));
        // A command missing from its menu goes at its end.
        let transport = &menus[6].items;
        assert_eq!(transport[transport.len() - 2], None);
        assert_eq!(
            transport.last().unwrap().unwrap().id,
            CommandId::from("transport.later")
        );
        // No separators first, last or twice.
        for menu in &menus {
            let items = &menu.items;
            assert!(items.first().unwrap().is_some(), "{}", menu.title);
            assert!(items.last().unwrap().is_some(), "{}", menu.title);
            let doubled = items
                .windows(2)
                .any(|pair| pair[0].is_none() && pair[1].is_none());
            assert!(!doubled, "{}", menu.title);
        }
        // Menus without commands are left out.
        let mut file_only = Registry::new();
        file::register(&mut file_only);
        assert_eq!(titles(&self::menus(&file_only)), ["File"]);
    }

    #[test]
    fn labels_name_the_area() {
        let command = Command::new(transport::PLAY, "Play");
        assert_eq!(label(&command), "Transport: Play");
        assert_eq!(area_of(&edit::UNDO), "edit");
        assert_eq!(capitalized("transport"), "Transport");
        assert_eq!(capitalized(""), "");
    }
}
