//! The menus that list the commands, and how commands are named in them and
//! in the command palette.

use musicable_commands::{Command, CommandId, Registry, edit, file, options, transport, view};

/// A menu's title, the area of commands it lists, and its commands in order.
/// `None` is a separator.
type MenuSpec = (&'static str, &'static str, &'static [Option<CommandId>]);

/// The menus that come first, before those for other areas. A command missing
/// from its area's menu goes at the end of it, and an area of commands
/// without a menu gets one after these, so every command can be reached.
const MENUS: [MenuSpec; 3] = [
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
            Some(file::QUIT),
        ],
    ),
    ("Edit", "edit", &[Some(edit::UNDO), Some(edit::REDO)]),
    (
        "Transport",
        "transport",
        &[
            Some(transport::PLAY_STOP),
            Some(transport::PLAY),
            Some(transport::STOP),
            Some(transport::GO_TO_START),
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
    ("View", "view", &[Some(view::COMMAND_PALETTE)]),
    (
        "Options",
        "options",
        &[
            Some(options::AUDIO_SETTINGS),
            Some(options::KEYBOARD_SHORTCUTS),
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

/// Every command in `commands`, in the menus the menu bar shows. A menu
/// without commands is left out.
pub(crate) fn menus(commands: &Registry) -> Vec<Menu<'_>> {
    let listed = |area: &str| MENUS.iter().chain(&LAST_MENUS).any(|m| m.1 == area);
    let mut others: Vec<&str> = commands
        .iter()
        .map(|c| area_of(&c.id))
        .filter(|area| !listed(area))
        .collect();
    others.dedup();
    let others = others.into_iter().map(|area| Menu {
        title: capitalized(area),
        items: commands
            .iter()
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
    let mut menu = Menu {
        title: title.to_owned(),
        items: items
            .iter()
            .filter_map(|item| match item {
                Some(id) => commands.get(id).map(Some),
                None => Some(None),
            })
            .collect(),
    };
    let rest: Vec<_> = commands
        .iter()
        .filter(|c| area_of(&c.id) == area && !items.contains(&Some(c.id.clone())))
        .collect();
    if !rest.is_empty() {
        menu.items.push(None);
        menu.items.extend(rest.into_iter().map(Some));
    }
    menu
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
        file::register(&mut commands);
        edit::register(&mut commands);
        transport::register(&mut commands);
        options::register(&mut commands);
        view::register(&mut commands);
        commands.register("transport.later", "Later", |_| {});
        commands.register("clip.split", "Split", |_| {});

        let menus = menus(&commands);
        // Areas without a menu of their own come before View and Options.
        assert_eq!(
            titles(&menus),
            ["File", "Edit", "Transport", "Clip", "View", "Options"]
        );
        let listed: Vec<_> = menus
            .iter()
            .flat_map(|menu| menu.items.iter().flatten())
            .map(|command| command.id.clone())
            .collect();
        assert_eq!(listed.len(), commands.iter().count());
        for command in commands.iter() {
            assert!(listed.contains(&command.id), "{}", command.id);
        }
        // A command missing from its menu goes at its end.
        let transport = &menus[2].items;
        assert_eq!(transport[transport.len() - 2], None);
        assert_eq!(
            transport.last().unwrap().unwrap().id,
            CommandId::from("transport.later")
        );
        // Menus without commands are left out.
        let mut file_only = Registry::new();
        file::register(&mut file_only);
        assert_eq!(titles(&self::menus(&file_only)), ["File"]);
    }

    #[test]
    fn labels_name_the_area() {
        let command = Command {
            id: transport::PLAY.clone(),
            title: "Play".into(),
        };
        assert_eq!(label(&command), "Transport: Play");
        assert_eq!(area_of(&edit::UNDO), "edit");
        assert_eq!(capitalized("transport"), "Transport");
        assert_eq!(capitalized(""), "");
    }
}
