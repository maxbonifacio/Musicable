//! The manual's pages that come from the code, in `manual/src/generated`:
//! every command with its default keys, the default keys, and the built-in
//! devices' controls. The manual's own pages include them. The tests check
//! that the files are what the code makes now, so the manual keeps up with
//! it, and that a page of the manual shows each one.
//!
//! `MUSICABLE_BLESS=1 cargo test -p musicable-ui --lib manual` rewrites
//! them. CONTRIBUTING.md#the-manual has more on the manual.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use musicable_commands::{CommandId, Keymap, Registry, Scope};
use musicable_model::native::{NativeKind, ParamInfo, Unit};

use crate::menus::{area_of, capitalized, label, menus};
use crate::{native, panels};

/// The folder of the generated pages, in the manual's source.
const GENERATED: &str = "generated";

/// What every generated page starts with.
const HEADER: &str = "<!-- Made from the code by crates/ui/src/manual.rs: don't edit it. \
                      `MUSICABLE_BLESS=1 cargo test -p musicable-ui --lib manual` \
                      makes it again. -->\n\n";

/// The manual's source.
fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../manual/src")
}

fn blessing() -> bool {
    env::var_os("MUSICABLE_BLESS").is_some()
}

/// Every generated page, by its path in the generated folder, with `/`
/// between names.
fn pages() -> BTreeMap<String, String> {
    let mut commands = Registry::new();
    musicable_commands::register_all(&mut commands);
    let keymap = Keymap::default_keymap();
    let panels = panels::Registry::new();
    let mut pages = BTreeMap::new();
    pages.insert(
        "commands.md".to_owned(),
        commands_page(&commands, &keymap, &panels),
    );
    pages.insert(
        "commands-with-arguments.md".to_owned(),
        arguments_page(&commands),
    );
    pages.insert("keys.md".to_owned(), keys_page(&commands, &keymap, &panels));
    for kind in NativeKind::ALL {
        pages.insert(format!("devices/{}.md", kind.key()), controls_page(kind));
    }
    pages
}

/// The commands the menu bar lists, menu by menu, in its order, with their
/// default keys.
fn commands_page(commands: &Registry, keymap: &Keymap, panels: &panels::Registry) -> String {
    let mut page = HEADER.to_owned();
    for menu in menus(commands) {
        let _ = writeln!(page, "### {}\n", menu.title);
        page.push_str("| Command | Keys | Id |\n|---|---|---|\n");
        for command in menu.items.iter().flatten() {
            let keys = keys(keymap, panels, &command.id);
            let _ = writeln!(
                page,
                "| {} | {keys} | `{}` |",
                cell(&command.title),
                command.id
            );
        }
        page.push('\n');
    }
    page
}

/// The commands that need arguments, which the menus, keys and the command
/// palette leave out, area by area.
fn arguments_page(commands: &Registry) -> String {
    let mut page = HEADER.to_owned();
    let mut area = None;
    // The registry has them in order of id, so each area's come together.
    for command in commands.iter().filter(|c| c.needs_arguments()) {
        let this = area_of(&command.id);
        if area != Some(this) {
            if area.is_some() {
                page.push('\n');
            }
            area = Some(this);
            let _ = writeln!(page, "### {}\n", capitalized(this));
            page.push_str("| Command | Id |\n|---|---|\n");
        }
        let _ = writeln!(page, "| {} | `{}` |", cell(&command.title), command.id);
    }
    page
}

/// Every default key, place by place as the Settings window's Keys page has
/// them, with the command it runs, named as the command palette names it.
fn keys_page(commands: &Registry, keymap: &Keymap, panels: &panels::Registry) -> String {
    let mut page = HEADER.to_owned();
    let mut place = None;
    for (scope, chord, id) in keymap.iter() {
        if place != Some(scope) {
            if place.is_some() {
                page.push('\n');
            }
            place = Some(scope);
            let heading = match scope {
                Scope::Everywhere => "Everywhere".to_owned(),
                Scope::Panel(id) => format!("In the {} panel", panel_title(panels, id)),
            };
            let _ = writeln!(page, "## {heading}\n");
            page.push_str("| Keys | Command |\n|---|---|\n");
        }
        let command = commands.get(id).map_or_else(|| id.to_string(), label);
        let _ = writeln!(page, "| {chord} | {} |", cell(&command));
    }
    page
}

/// A command's default keys, everywhere and then panel by panel:
/// "Ctrl+Y, Ctrl+Shift+Z; Down in the History panel".
fn keys(keymap: &Keymap, panels: &panels::Registry, id: &CommandId) -> String {
    let mut places: Vec<(Scope, Vec<String>)> = Vec::new();
    for (scope, chord) in keymap.bindings_for(id) {
        match places.last_mut() {
            Some((place, chords)) if *place == scope => chords.push(chord.to_string()),
            _ => places.push((scope, vec![chord.to_string()])),
        }
    }
    let places: Vec<_> = places
        .into_iter()
        .map(|(scope, chords)| match scope {
            Scope::Everywhere => chords.join(", "),
            Scope::Panel(id) => format!(
                "{} in the {} panel",
                chords.join(", "),
                panel_title(panels, id)
            ),
        })
        .collect();
    places.join("; ")
}

/// What the panel's tab calls it.
fn panel_title<'a>(panels: &panels::Registry, id: &'a str) -> &'a str {
    panels.get(id).map_or(id, |panel| panel.title)
}

/// A group of a device's controls: its name, empty for none, and its
/// controls' places in the device's table.
type Group = (&'static str, Vec<usize>);

/// The device's controls, a table for each group as the devices panel has
/// them, with their ranges and defaults as it writes them. Groups of the
/// same controls, such as the EQ's bands, share a table.
fn controls_page(kind: NativeKind) -> String {
    let params = kind.params();
    let groups = native::groups(params);
    let mut tables = Vec::new();
    let mut start = 0;
    while start < groups.len() {
        let end = (start + 1..groups.len())
            .find(|&next| !alike(params, &groups[start], &groups[next]))
            .unwrap_or(groups.len());
        tables.push(match &groups[start..end] {
            [group] => group_table(params, group),
            alike => shared_table(params, alike),
        });
        start = end;
    }
    HEADER.to_owned() + &tables.join("\n")
}

/// Whether two named groups have the same controls, with the same ranges.
fn alike(params: &[ParamInfo], a: &Group, b: &Group) -> bool {
    let controls = |(_, members): &Group| -> Vec<(&str, String)> {
        let controls = members.iter().map(|&i| (params[i].name, range(&params[i])));
        controls.collect()
    };
    !a.0.is_empty() && !b.0.is_empty() && controls(a) == controls(b)
}

/// A table of a group's controls, headed by the group's name.
fn group_table(params: &[ParamInfo], (name, members): &Group) -> String {
    let heading = if name.is_empty() { "Control" } else { name };
    let mut table = format!("| {} | Range | Default |\n|---|---|---|\n", cell(heading));
    for param in members.iter().map(|&i| &params[i]) {
        let _ = writeln!(
            table,
            "| {} | {} | {} |",
            cell(param.name),
            cell(&range(param)),
            cell(&param.format(param.default))
        );
    }
    table
}

/// Two tables for groups with the same controls: the controls' ranges, and
/// a row of defaults for each group, with a column for each control.
fn shared_table(params: &[ParamInfo], groups: &[Group]) -> String {
    let names: Vec<_> = groups.iter().map(|(name, _)| cell(name)).collect();
    let heading = match names.as_slice() {
        [a, b] => format!("{a} and {b}"),
        [first, .., last] => format!("{first} to {last}"),
        _ => unreachable!("groups share tables in twos or more"),
    };
    let (_, first) = &groups[0];
    let mut table = format!("| {heading} | Range |\n|---|---|\n");
    for param in first.iter().map(|&i| &params[i]) {
        let _ = writeln!(table, "| {} | {} |", cell(param.name), cell(&range(param)));
    }
    let row = |label: &str, cells: Vec<String>| format!("| {label} | {} |\n", cells.join(" | "));
    table += "\n";
    table += &row(
        "Defaults",
        first.iter().map(|&i| cell(params[i].name)).collect(),
    );
    table += &"|---".repeat(first.len() + 1);
    table += "|\n";
    for ((_, members), name) in groups.iter().zip(&names) {
        let defaults = members.iter().map(|&i| &params[i]);
        table += &row(name, defaults.map(|c| cell(&c.format(c.default))).collect());
    }
    table
}

/// The values a control takes: "20.0 Hz to 20.0 kHz", "Off or On", or its
/// choices.
fn range(param: &ParamInfo) -> String {
    match param.unit {
        Unit::Switch => "Off or On".to_owned(),
        Unit::Choice(choices) => choices.join(", "),
        _ => format!("{} to {}", param.format(param.min), param.format(param.max)),
    }
}

/// `text` for a cell of a table, where `|` would end the cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// The files in `folder` and the folders in it, by their paths in it, with
/// `/` between names.
fn files(folder: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            found.extend(
                files(&path)
                    .into_iter()
                    .map(|file| format!("{name}/{file}")),
            );
        } else {
            found.push(name);
        }
    }
    found
}

#[test]
fn generated_pages_are_what_the_code_makes() {
    let folder = source().join(GENERATED);
    let pages = pages();
    let mut stale = Vec::new();
    for name in files(&folder) {
        if !pages.contains_key(&name) {
            if blessing() {
                fs::remove_file(folder.join(&name)).unwrap();
            } else {
                stale.push(format!("{name}, which nothing makes now"));
            }
        }
    }
    for (name, text) in &pages {
        let path = folder.join(name);
        if blessing() {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
        } else if fs::read_to_string(&path).ok().as_ref() != Some(text) {
            stale.push(name.clone());
        }
    }
    assert!(
        stale.is_empty(),
        "these pages in manual/src/generated aren't what the code makes:\n  {}\n\
         Run\n  MUSICABLE_BLESS=1 cargo test -p musicable-ui --lib manual\n\
         to make them again, and commit them.",
        stale.join("\n  ")
    );
}

/// The pages `SUMMARY.md` lists, as paths in the manual's source.
fn listed(summary: &str) -> Vec<String> {
    summary
        .split("](")
        .skip(1)
        .filter_map(|rest| rest.split_once(')'))
        .map(|(path, _)| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .collect()
}

/// The files `page` includes with `{{#include ...}}`, as written.
fn includes(page: &str) -> Vec<&str> {
    page.split("{{#include ")
        .skip(1)
        .filter_map(|rest| rest.split_once("}}"))
        .map(|(target, _)| {
            // Without the anchor or lines after a `:`.
            target.split(':').next().unwrap_or_default().trim()
        })
        .collect()
}

/// `path` without `.` and `..`.
fn tidy(path: &Path) -> PathBuf {
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.iter().collect()
}

#[test]
fn a_page_of_the_manual_shows_every_generated_page() {
    let source = source();
    let summary = fs::read_to_string(source.join("SUMMARY.md")).unwrap();
    let mut shown = BTreeSet::new();
    for page in listed(&summary) {
        let text = fs::read_to_string(source.join(&page))
            .unwrap_or_else(|error| panic!("SUMMARY.md lists {page}: {error}"));
        let folder = Path::new(&page).parent().unwrap_or(Path::new(""));
        for target in includes(&text) {
            shown.insert(tidy(&folder.join(target)));
        }
    }
    for name in pages().keys() {
        let path = Path::new(GENERATED).join(name);
        assert!(
            shown.contains(&tidy(&path)),
            "no page that SUMMARY.md lists includes {}",
            path.display()
        );
    }
}

#[test]
fn listing_and_including_find_the_paths() {
    let summary = "# Summary\n\n[Intro](intro.md)\n\n- [Draft]()\n- [Keys](reference/keys.md)\n";
    assert_eq!(listed(summary), ["intro.md", "reference/keys.md"]);
    let page = "# Keys\n\n{{#include ../generated/keys.md}}\n\n{{#include a.md:2:10}}\n";
    assert_eq!(includes(page), ["../generated/keys.md", "a.md"]);
    assert_eq!(
        tidy(&Path::new("reference").join("../generated/keys.md")),
        Path::new("generated").join("keys.md")
    );
}
