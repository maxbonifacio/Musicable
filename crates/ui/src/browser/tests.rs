use std::sync::Arc;

use egui::accesskit::Toggled;
use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use musicable_library::{AudioFacts, Format, Tag};
use musicable_model::plugin::{PluginDevice, PluginSettings};
use musicable_model::{DeviceKind, Project, TrackKind};

use super::*;
use crate::PeaksBuilder;
use crate::panels::BROWSER;
use crate::testing::{Previewing, State, quick_harness};
use crate::{Layout, Split, WindowLayout, Workspace};

/// A record of a second of 44.1 kHz stereo WAV at `path`.
fn record(path: &str) -> Record {
    let audio = AudioFacts {
        format: Format::Wav,
        sample_rate: 44_100,
        channels: 2,
        frames: 44_100,
    };
    Record::new(path, 1000, 0, Some(audio))
}

/// An index of the folders `places`, each with the files given, from the
/// folder.
fn index(places: &[(&str, &[&str])]) -> Index {
    let mut index = Index::new();
    for (folder, files) in places {
        index.add_place(PathBuf::from(folder));
        let records = files.iter().map(|path| record(path)).collect();
        index.set_files(Path::new(folder), records);
    }
    index
}

/// Two folders of samples.
fn samples() -> Index {
    index(&[
        (
            "/samples/Drums",
            &[
                "Kicks/Kick 01.wav",
                "Kicks/Kick 02.wav",
                "Snares/Snare 01.wav",
                "Loop 120.wav",
            ],
        ),
        ("/samples/Vocals", &["Vox.wav"]),
    ])
}

/// The shell with the browser showing, and `library`'s files in it.
fn harness(project: Project, library: Index) -> Harness<'static, State> {
    let mut harness = quick_harness(project);
    let state = harness.state_mut();
    state.library.index = library;
    state.shell.toggle_panel(BROWSER);
    harness.run();
    harness
}

fn click(harness: &mut Harness<'static, State>, label: &str) {
    harness.get_by_label(label).click();
    harness.run();
}

/// Waits long enough that the next click isn't taken for a double click.
fn pause(harness: &mut Harness<'static, State>) {
    for _ in 0..30 {
        harness.step();
    }
}

fn double_click(harness: &mut Harness<'static, State>, label: &str) {
    pause(harness);
    let at = harness.get_by_label(label).rect().center();
    harness.hover_at(at);
    harness.run();
    for pressed in [true, false, true, false] {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
}

/// Drags from `from` to `to` in a few moves, and lets go there.
fn drag(harness: &mut Harness<'static, State>, from: Pos2, to: Pos2) {
    pause(harness);
    harness.hover_at(from);
    harness.run();
    harness.drag_at(from);
    harness.run();
    for step in 1..=5 {
        harness.hover_at(from + (to - from) * step as f32 / 5.0);
        harness.step();
    }
    harness.event(Event::PointerButton {
        pos: to,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run();
}

fn actions(harness: &mut Harness<'static, State>) -> Vec<Action> {
    std::mem::take(&mut harness.state_mut().actions)
}

fn with_file(id: musicable_commands::CommandId, path: &str) -> Action {
    Action::RunWith(id, Args::new().with("file", path))
}

/// The path of the file `file` in the library's folder `folder`, as the
/// browser gives it: with the system's separator between the names it adds
/// to the folder's, a backslash on Windows.
fn in_folder(folder: &str, file: &str) -> String {
    let mut path = PathBuf::from(folder);
    path.extend(file.split('/'));
    path.to_string_lossy().into_owned()
}

/// The text field whose hint is `hint`.
fn field<'a>(harness: &'a Harness<'static, State>, hint: &'static str) -> egui_kittest::Node<'a> {
    harness.get_by(move |node| node.data().placeholder() == Some(hint))
}

fn top(harness: &Harness<'static, State>, label: &str) -> f32 {
    harness.get_by_label(label).rect().top()
}

fn toggled(harness: &Harness<'static, State>, label: &str) -> Option<Toggled> {
    harness.get_by_label(label).accesskit_node().toggled()
}

#[test]
fn the_folders_open_and_picking_a_sample_previews_it() {
    let mut harness = harness(Project::new(), samples());
    harness.get_by_label("5 samples in 2 folders");
    assert!(harness.query_by_label("Kick 01").is_none());

    // A folder's folders come first, then its files, each in order of name.
    click(&mut harness, "Drums");
    let rows = ["Kicks", "Snares", "Loop 120", "Vocals"];
    let tops: Vec<f32> = rows.iter().map(|row| top(&harness, row)).collect();
    assert!(tops.is_sorted(), "{tops:?}");
    click(&mut harness, "Kicks");
    assert!(top(&harness, "Kick 02") < top(&harness, "Snares"));
    assert!(actions(&mut harness).is_empty());

    // A click selects a sample and previews it.
    click(&mut harness, "Kick 01");
    assert_eq!(toggled(&harness, "Kick 01"), Some(Toggled::True));
    let kick = in_folder("/samples/Drums", "Kicks/Kick 01.wav");
    assert_eq!(actions(&mut harness), [with_file(browser::PREVIEW, &kick)]);

    // Without auto preview, it only selects it.
    harness.state_mut().library.auto_preview = false;
    pause(&mut harness);
    click(&mut harness, "Kick 02");
    assert_eq!(toggled(&harness, "Kick 01"), Some(Toggled::False));
    assert!(actions(&mut harness).is_empty());

    // Down and Up go through the samples shown, while the search field has
    // the keys.
    harness.state_mut().library.auto_preview = true;
    harness.get_by_role(Role::TextInput).focus();
    harness.run();
    harness.key_press(Key::ArrowDown);
    harness.run();
    assert_eq!(toggled(&harness, "Loop 120"), Some(Toggled::True));
    harness.key_press(Key::ArrowUp);
    harness.run();
    let preview = |file| with_file(browser::PREVIEW, &in_folder("/samples/Drums", file));
    assert_eq!(
        actions(&mut harness),
        [preview("Loop 120.wav"), preview("Kicks/Kick 02.wav")]
    );

    // A double-click puts it on the selected track.
    double_click(&mut harness, "Loop 120");
    let insert = Action::DropFiles {
        files: vec![PathBuf::from("/samples/Drums/Loop 120.wav")],
        track: None,
        at: None,
    };
    assert_eq!(actions(&mut harness).last(), Some(&insert));

    // Closing a folder hides what's in it.
    click(&mut harness, "Drums");
    assert!(harness.query_by_label("Kicks").is_none());
}

#[test]
fn a_place_rescans_or_leaves_the_library_from_its_menu() {
    let mut harness = harness(Project::new(), samples());
    harness.get_by_label("Vocals").click_secondary();
    harness.run();
    click(&mut harness, "Rescan");
    harness.get_by_label("Vocals").click_secondary();
    harness.run();
    click(&mut harness, "Remove from Library");
    click(&mut harness, "Add Folder");
    let folder = Args::new().with("folder", "/samples/Vocals");
    assert_eq!(
        actions(&mut harness),
        [
            Action::RunWith(browser::RESCAN, folder.clone()),
            Action::RunWith(browser::REMOVE_FOLDER, folder),
            Action::Run(browser::ADD_FOLDER),
        ]
    );

    // Without folders, the browser says how to add one.
    harness.state_mut().library.index = Index::new();
    harness.run();
    harness.get_by_label_contains("Add a folder of samples to the library");
    click(&mut harness, "Add Folder...");
    assert_eq!(actions(&mut harness), [Action::Run(browser::ADD_FOLDER)]);

    // And while it scans, how far it has got.
    let state = harness.state_mut();
    state.library.index = samples();
    state.library.scanning = vec![(PathBuf::from("/samples/Drums"), 1234)];
    harness.run_steps(2);
    harness.get_by_label("Looking through: 1,234 files");
    harness.get_by_label("Drums");
}

#[test]
fn searches_list_what_the_library_found_and_files_like_one() {
    let mut harness = harness(Project::new(), samples());
    harness.get_by_role(Role::TextInput).click();
    harness.run();
    harness.get_by_role(Role::TextInput).type_text("kick");
    harness.run();
    let search = |text: &str| Action::RunWith(browser::SEARCH, Args::new().with("text", text));
    assert_eq!(actions(&mut harness), [search("kick")]);
    // What it found, with the folders they're in, and not the folders.
    assert!(top(&harness, "Kick 01") < top(&harness, "Kick 02"));
    assert!(harness.query_by_label("Drums").is_none());
    harness.get_by_label("2 found");

    // Find Similar lists the files like one, until the search is back.
    harness.get_by_label("Kick 01").click_secondary();
    harness.run();
    click(&mut harness, "Find Similar");
    let kick = in_folder("/samples/Drums", "Kicks/Kick 01.wav");
    assert_eq!(
        actions(&mut harness),
        [with_file(browser::FIND_SIMILAR, &kick)]
    );
    harness.get_by_label("Like Kick 01");
    harness.get_by_label("Vox");
    harness.get_by_label("4 found");
    click(&mut harness, "Back to Search");
    assert_eq!(actions(&mut harness), [search("kick")]);
    assert!(harness.query_by_label("Vox").is_none());

    // A search from elsewhere, as an agent runs one, shows in the field.
    harness.state_mut().library.search("snare");
    harness.run();
    let field = harness.get_by_role(Role::TextInput);
    assert_eq!(field.value().as_deref(), Some("snare"));
    harness.get_by_label("Snare 01");
    harness.state_mut().library.search("nothing like it");
    harness.run();
    harness.get_by_label("No samples match.");
}

#[test]
fn tags_are_given_and_taken_away_in_a_samples_menu() {
    let mut library = samples();
    let kick = PathBuf::from(in_folder("/samples/Drums", "Kicks/Kick 01.wav"));
    let id = library.find(&kick).unwrap();
    library.tag(id, "dusty", true);
    let mut harness = harness(Project::new(), library);
    harness.state_mut().library.search("kick");
    harness.run();
    harness.get_by_label("Kick 01").click_secondary();
    harness.run();
    click(&mut harness, "dusty");
    harness.get_by_label("Kick 01").click_secondary();
    harness.run();
    field(&harness, "New tag").click();
    harness.run();
    field(&harness, "New tag").type_text(" Warm ");
    harness.run();
    harness.key_press(Key::Enter);
    harness.run();
    let tag = |tag: &str, on: bool| {
        let args = Args::new()
            .with("file", kick.to_string_lossy().as_ref())
            .with("tag", tag)
            .with("on", on);
        Action::RunWith(browser::TAG, args)
    };
    assert_eq!(
        actions(&mut harness),
        [tag("dusty", false), tag("Warm", true)]
    );
    // The menu closed with Enter.
    assert!(harness.query_by_label("Find Similar").is_none());
    assert_eq!(
        harness.state().library.index.get(id).unwrap().tags,
        [Tag::yours("dusty")]
    );
}

#[test]
fn the_preview_strip_shows_the_sample_playing_and_stops_it() {
    let mut harness = harness(Project::new(), samples());
    // Nothing to stop until a sample plays.
    harness.get_by_label("Preview");
    assert!(
        harness
            .get_by_label("Stop Preview")
            .accesskit_node()
            .is_disabled()
    );

    let mut builder = PeaksBuilder::new(44_100, 1);
    builder.push(&vec![0.5; 44_100]);
    let path = PathBuf::from("/samples/Vocals/Vox.wav");
    let peaks = Arc::new(builder.finish());
    harness.state_mut().library.preview = Some(Previewing {
        path,
        at: Some(22_050),
        frames: Some(44_100),
        peaks: Some(peaks),
    });
    harness.run();
    click(&mut harness, "Stop Preview");
    click(&mut harness, "Auto Preview");
    pause(&mut harness);
    click(&mut harness, "Preview of Vox");
    assert_eq!(
        actions(&mut harness),
        [
            Action::Run(browser::STOP_PREVIEW),
            Action::SetAutoPreview(false),
            with_file(browser::PREVIEW, "/samples/Vocals/Vox.wav"),
        ]
    );

    // The volume, in steps of a dB.
    harness
        .get_by_role_and_label(Role::Slider, "Preview Volume")
        .focus();
    harness.run();
    harness.key_press(Key::ArrowLeft);
    harness.run();
    assert_eq!(actions(&mut harness), [Action::SetPreviewVolume(-1)]);
}

fn plugin(name: &str, format: PluginFormat, id: &str, instrument: bool) -> PluginEntry {
    PluginEntry {
        format,
        id: id.to_owned(),
        name: name.to_owned(),
        vendor: "Example".to_owned(),
        instrument,
    }
}

fn presets() -> Vec<Preset> {
    let preset = |path: &str, name: &str, kind| Preset {
        path: PathBuf::from(path),
        name: name.to_owned(),
        kind,
    };
    vec![
        preset(
            "/presets/Quiet.musicable-preset",
            "Quiet",
            PresetKind::Device {
                name: "Test Gain".to_owned(),
            },
        ),
        preset(
            "/presets/Vital/Warm Pad.musicable-preset",
            "Warm Pad",
            PresetKind::Plugin {
                format: PluginFormat::Clap,
                id: "org.example.vital".to_owned(),
                name: "Vital".to_owned(),
            },
        ),
        preset(
            "/vst3/Surge/Bright Lead.vstpreset",
            "Bright Lead",
            PresetKind::Vst3 {
                class_id: "ABCDEF0123456789ABCDEF0123456789".to_owned(),
            },
        ),
        preset(
            "/presets/Gone/Old.musicable-preset",
            "Old",
            PresetKind::Plugin {
                format: PluginFormat::Clap,
                id: "org.example.gone".to_owned(),
                name: "Gone".to_owned(),
            },
        ),
    ]
}

#[test]
fn presets_are_listed_by_device_and_added_to_the_devices_chain() {
    let mut project = Project::new();
    let keys = project.add_track("Keys", TrackKind::Midi);
    let mut harness = harness(project, samples());
    let state = harness.state_mut();
    state.plugins = vec![
        plugin("Vital", PluginFormat::Clap, "org.example.vital", true),
        // VST3 class ids can come in either case.
        plugin(
            "Surge XT",
            PluginFormat::Vst3,
            "abcdef0123456789abcdef0123456789",
            true,
        ),
    ];
    state.library.presets = presets();
    state.editor.selected_track = Some(keys);
    harness.run();
    click(&mut harness, "Presets");
    // By device, in order of name, without the presets of plugins that
    // aren't installed.
    let rows = [
        "Surge XT",
        "Bright Lead",
        "Test Gain",
        "Quiet",
        "Vital",
        "Warm Pad",
    ];
    let tops: Vec<f32> = rows.iter().map(|row| top(&harness, row)).collect();
    assert!(tops.is_sorted(), "{tops:?}");
    assert!(harness.query_by_label("Old").is_none());
    harness.get_by_label("3 presets, 1 for plugins not installed");

    double_click(&mut harness, "Warm Pad");
    let add = Args::new()
        .with("preset", "/presets/Vital/Warm Pad.musicable-preset")
        .with("track", keys);
    assert_eq!(
        actions(&mut harness),
        [Action::RunWith(device::ADD_PRESET, add)]
    );

    harness.get_by_role(Role::TextInput).click();
    harness.run();
    harness.get_by_role(Role::TextInput).type_text("quiet");
    harness.run();
    harness.get_by_label("Quiet");
    assert!(harness.query_by_label("Warm Pad").is_none());
    harness.get_by_role(Role::TextInput).type_text(" nothing");
    harness.run();
    harness.get_by_label("No presets match.");
}

#[test]
fn samples_devices_and_presets_drag_onto_tracks_and_into_the_devices_panel() {
    let mut project = Project::new();
    let drums = project.add_track("Drums", TrackKind::Audio);
    let keys = project.add_track("Keys", TrackKind::Midi);
    let mut harness = harness(project, samples());
    let state = harness.state_mut();
    state.plugins = vec![plugin(
        "Vital",
        PluginFormat::Clap,
        "org.example.vital",
        true,
    )];
    state.library.presets = presets();
    state.library.search("kick");
    state.editor.selected_track = Some(keys);
    // Room under the tracks.
    state.shell.toggle_panel(crate::MIXER);
    harness.run();
    actions(&mut harness);

    // A sample on an audio track's lane goes there, where it's let go, on
    // the grid.
    let lanes = harness.get_by_label("Arrangement").rect();
    let viewport = harness.state().editor.viewport;
    let x = lanes.left() + viewport.x_of(Beats::new(8));
    let drums_row = harness.get_by_label("Track Drums").rect();
    let from = harness.get_by_label("Kick 01").rect().center();
    drag(&mut harness, from, pos2(x + 2.0, drums_row.center().y));
    let kick = PathBuf::from("/samples/Drums/Kicks/Kick 01.wav");
    let dropped = Action::DropFiles {
        files: vec![kick.clone()],
        track: Some(drums),
        at: Some(Beats::new(8)),
    };
    assert_eq!(actions(&mut harness), [dropped]);

    // A plugin under the tracks goes on a new track.
    click(&mut harness, "Devices");
    let below = (harness.get_by_label("Track Keys").rect().bottom() + lanes.bottom()) / 2.0;
    let from = harness.get_by_label("Vital").rect().center();
    drag(&mut harness, from, pos2(lanes.center().x, below));
    let vital = devices::add(&harness.state().plugins[0], None);
    assert_eq!(
        actions(&mut harness),
        [Action::RunWith(
            device::ADD_PLUGIN,
            vital.with("new_track", true)
        )]
    );
    // One of Musicable's own on a track goes on its devices.
    let from = harness.get_by_label("EQ").rect().center();
    drag(&mut harness, from, pos2(x + 2.0, drums_row.center().y));
    let eq = Args::new().with("kind", "eq").with("track", drums);
    assert_eq!(actions(&mut harness), [Action::RunWith(device::ADD, eq)]);

    // A preset in the devices panel goes in the chain it shows: the
    // selected track's.
    click(&mut harness, "Presets");
    let panel = harness.get_by_label_contains("No devices.").rect();
    let from = harness.get_by_label("Warm Pad").rect().center();
    drag(&mut harness, from, panel.center());
    let add = Args::new()
        .with("preset", "/presets/Vital/Warm Pad.musicable-preset")
        .with("track", keys);
    assert_eq!(
        actions(&mut harness),
        [Action::RunWith(device::ADD_PRESET, add)]
    );

    // Nothing comes of a sample let go in the devices panel.
    click(&mut harness, "Samples");
    let from = harness.get_by_label("Kick 02").rect().center();
    drag(&mut harness, from, panel.center());
    assert!(actions(&mut harness).is_empty());
}

#[test]
fn devices_dropped_in_the_devices_panel_go_in_its_chain_wherever_it_is() {
    let mut project = Project::new();
    let keys = project.add_track("Keys", TrackKind::Midi);
    let mut harness = harness(project, samples());
    // The devices right of the arrangement and the mixer, as tall as the
    // window.
    let workspace = Workspace {
        slot: None,
        windows: vec![WindowLayout {
            hidden: Vec::new(),
            layout: Layout::new(Split::Across(vec![
                Layout::panel(BROWSER),
                Layout::new(Split::Down(vec![
                    Layout::panel(crate::ARRANGEMENT),
                    Layout::panel(crate::MIXER),
                ])),
                Layout {
                    size: Some(320),
                    ..Layout::panel(crate::DEVICES)
                },
            ])),
            ..WindowLayout::default()
        }],
    };
    let state = harness.state_mut();
    state.shell.set_workspace(&workspace);
    state.editor.selected_track = Some(keys);
    harness.run();
    click(&mut harness, "Devices");
    actions(&mut harness);

    let lanes = harness.get_by_label("Arrangement").rect();
    let panel = harness.get_by_label_contains("No devices.").rect();
    assert!(panel.left() > lanes.right(), "{panel:?} beside {lanes:?}");
    let from = harness.get_by_label("EQ").rect().center();
    drag(&mut harness, from, panel.center());
    let eq = Args::new().with("kind", "eq").with("track", keys);
    assert_eq!(actions(&mut harness), [Action::RunWith(device::ADD, eq)]);
}

#[test]
fn a_device_saves_as_a_preset_with_the_name_typed() {
    let mut project = Project::new();
    let keys = project.add_track("Keys", TrackKind::Midi);
    let synth = project.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: "org.example.vital".to_owned(),
        name: "Vital".to_owned(),
        instrument: true,
        settings: PluginSettings::default(),
    }));
    project.track_mut(keys).unwrap().devices.push(synth.clone());
    let mut harness = harness(project, Index::new());
    harness.state_mut().editor.selected_track = Some(keys);
    harness.run();
    harness.get_by_label("Device Vital").click_secondary();
    harness.run();
    click(&mut harness, "Save as Preset...");
    harness.get_by_label("Save Vital as a Preset");
    assert!(harness.state().shell.asking());
    // The device's name is selected, to be typed over.
    harness
        .get_by_label("Save Vital as a Preset")
        .type_text("Warm Pad");
    harness.run();
    harness.key_press(Key::Enter);
    harness.run();
    let save = Args::new()
        .with("device", synth.id)
        .with("name", "Warm Pad");
    assert_eq!(
        actions(&mut harness),
        [Action::RunWith(device::SAVE_PRESET, save)]
    );
    assert!(harness.query_by_label("Save Vital as a Preset").is_none());
    assert!(!harness.state().shell.asking());
}

#[test]
fn a_key_puts_the_keys_in_the_search_field_of_the_list_showing() {
    let mut harness = quick_harness(Project::new());
    harness.state_mut().shell.focus_browser_search();
    harness.run();
    assert!(harness.get_by_role(Role::TextInput).is_focused());
    click(&mut harness, "Presets");
    harness.state_mut().shell.focus_browser_search();
    harness.run();
    assert!(harness.get_by_role(Role::TextInput).is_focused());
}

#[test]
fn tree_rows_follow_the_folders_in_the_paths() {
    let library = index(&[(
        "/s",
        &[
            "b/x.wav",
            "B b/y.wav",
            "a.wav",
            "b/c/z.wav",
            "Z.wav",
            "b c.wav",
        ],
    )]);
    let files = library.places()[0].files();
    let mut rows = Vec::new();
    let open = |path: &str| path == "b/";
    tree_rows(files, 0, "", 0, &open, &mut rows);
    let shown: Vec<(usize, String)> = rows
        .iter()
        .map(|row| {
            let name = match &row.kind {
                TreeKind::Folder { folder, .. } => folder.clone(),
                TreeKind::File(file) => files[file.file].path.clone(),
                TreeKind::Place { .. } => unreachable!(),
            };
            (row.depth, name)
        })
        .collect();
    let expected = [
        (0, "b/"),
        (1, "b/c/"),
        (1, "b/x.wav"),
        (0, "B b/"),
        (0, "a.wav"),
        (0, "b c.wav"),
        (0, "Z.wav"),
    ];
    let expected: Vec<(usize, String)> = (expected.iter())
        .map(|&(depth, name)| (depth, name.to_owned()))
        .collect();
    assert_eq!(shown, expected);
    assert_eq!(thousands(1_234_567), "1,234,567");
    assert_eq!(thousands(999), "999");
    assert_eq!(count(1, "sample"), "1 sample");
}
