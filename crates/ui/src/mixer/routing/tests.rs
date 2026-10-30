//! The Routing panel in a window, its changes carried out on a document as
//! the app carries them out.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use musicable_commands::editor::Editor;
use musicable_commands::{
    CommandError, Context, Document, Keymap, NoLibrary, Registry, Request, Value, wire,
};
use musicable_model::native::{NativeDevice, NativeKind};
use musicable_model::time::Beats;
use musicable_model::{DeviceKind, Monitor};

use super::*;
use crate::shell::{Audio, KeymapStatus, Playback};
use crate::testing::Player;
use crate::{Levels, Plugins, Waveforms};

struct State {
    panel: RoutingPanel,
    document: Document,
    commands: Registry,
    keymap: Keymap,
    player: Player,
    levels: Levels,
    waveforms: Waveforms,
    parts: Parts,
    /// What the panel asked for, and what the commands asked of the app.
    actions: Vec<Action>,
    requests: Vec<Request>,
    errors: Vec<CommandError>,
}

impl State {
    fn project(&self) -> &Project {
        self.document.project()
    }

    fn editor(&self) -> &Editor {
        self.document.editor()
    }

    /// Carries out `action` as the app does.
    fn carry_out(&mut self, action: &Action) {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.player,
            requests: &mut self.requests,
            library: &mut NoLibrary,
        };
        let ran = match action {
            Action::Run(id) => self.commands.run_with(id, &Args::new(), &mut ctx),
            Action::RunWith(id, args) => self.commands.run_with(id, args, &mut ctx),
            Action::Mixer(change) => {
                musicable_commands::mixer::change(&mut self.document, change.clone())
                    .map(|()| Value::None)
            }
            Action::BeginGesture => {
                self.document.begin_gesture();
                Ok(Value::None)
            }
            Action::EndGesture => {
                self.document.end_gesture();
                Ok(Value::None)
            }
            _ => Ok(Value::None),
        };
        if let Err(error) = ran {
            self.errors.push(error);
        }
    }
}

/// The ids of the test project's tracks, device and wire.
struct Ids {
    kick: TrackId,
    bass: TrackId,
    lead: TrackId,
    double: TrackId,
    print: TrackId,
    drums: TrackId,
    reverb: TrackId,
    compressor: DeviceId,
    key: WireId,
}

/// Kick, an audio track feeding the Drums group, with a send to the Reverb
/// return; Bass, an audio track whose Compressor Kick's Post FX keys; Lead
/// and Double, MIDI tracks, Double playing Lead's notes; and Print, an audio
/// track recording Bass's Post Fader.
fn project() -> (Project, Ids) {
    let mut project = Project::new();
    let kick = project.add_track("Kick", TrackKind::Audio);
    let bass = project.add_track("Bass", TrackKind::Audio);
    let lead = project.add_track("Lead", TrackKind::Midi);
    let double = project.add_track("Double", TrackKind::Midi);
    let print = project.add_track("Print", TrackKind::Audio);
    let drums = project.add_track("Drums", TrackKind::Group);
    let reverb = project.add_track("Reverb", TrackKind::Return);
    let send = project.new_send(reverb);
    let track = project.track_mut(kick).unwrap();
    track.output = Bus::Group(drums);
    track.sends.push(send);
    let compressor = NativeDevice::new(NativeKind::Compressor);
    let compressor = project.new_device(DeviceKind::Native(compressor));
    let ids = (compressor.id, Port::Sidechain(compressor.id));
    project.track_mut(bass).unwrap().devices.push(compressor);
    let key = project.new_wire(Tap::PostFx(Chain::Track(kick)), ids.1);
    project.wires.push(key);
    let inputs = [
        (double, Tap::PreFx(lead)),
        (print, Tap::PostFader(Chain::Track(bass))),
    ];
    for (track, tap) in inputs {
        let track = project.track_mut(track).unwrap();
        track.input = Some(Input::Tap(tap));
        track.monitor = Monitor::In;
    }
    let ids = Ids {
        kick,
        bass,
        lead,
        double,
        print,
        drums,
        reverb,
        compressor: ids.0,
        key: key.id,
    };
    (project, ids)
}

fn harness(project: Project) -> Harness<'static, State> {
    let mut commands = Registry::new();
    musicable_commands::register_all(&mut commands);
    let state = State {
        panel: RoutingPanel::default(),
        document: Document::new(project),
        commands,
        keymap: Keymap::default_keymap(),
        player: Player::default(),
        levels: Levels::default(),
        waveforms: Waveforms::default(),
        parts: Parts::default(),
        actions: Vec::new(),
        requests: Vec::new(),
        errors: Vec::new(),
    };
    // Frames as quick as a screen's, so two clicks make a double click.
    let mut harness = Harness::builder()
        .with_size([900.0, 480.0])
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(40)
        .build_ui_state(
            |ui, state: &mut State| {
                let mut actions = Vec::new();
                {
                    let view = View {
                        project: state.document.project(),
                        editor: state.document.editor(),
                        waveforms: &state.waveforms,
                        name: "Untitled",
                        modified: false,
                        commands: &state.commands,
                        keymap: &state.keymap,
                        default_keymap: &state.keymap,
                        history: state.document.steps(),
                        playback: Playback {
                            playing: false,
                            record: false,
                            playhead: Beats::ZERO,
                            seconds: 0.0,
                        },
                        recordings: &[],
                        levels: &state.levels,
                        performance: &crate::Performance::default(),
                        inputs: &[],
                        midi_inputs: &[],
                        audio: Audio {
                            summary: "",
                            running: true,
                            load: None,
                            dropouts: 0,
                            details: "",
                            lead: None,
                        },
                        keymap_file: KeymapStatus::default(),
                        messages: &[],
                        agent: None,
                        analysing: None,
                        rendering: None,
                        plugins: Plugins::default(),
                        native: &[],
                        library: crate::Library::default(),
                        workspaces: crate::Workspaces::default(),
                        monitors: &[],
                    };
                    let (theme, mouse) = (Theme::default(), MouseMap::live());
                    let panel = &mut state.panel;
                    panel.show(ui, &view, &theme, &mouse, &state.parts, &mut actions);
                }
                for action in &actions {
                    state.carry_out(action);
                }
                state.actions.extend(actions);
            },
            state,
        );
    harness.run();
    harness
}

/// The value the menu called `label` shows.
fn value(harness: &Harness<'_, State>, label: &str) -> Option<String> {
    harness.get_by_role_and_label(Role::ComboBox, label).value()
}

/// Opens the menu called `label`.
fn open(harness: &mut Harness<'_, State>, label: &str) {
    harness.get_by_role_and_label(Role::ComboBox, label).click();
    harness.run();
}

/// Picks `item` from the menu open, scrolled to it.
fn pick(harness: &mut Harness<'_, State>, item: &str) {
    harness
        .get_by_role_and_label(Role::Button, item)
        .scroll_to_me();
    harness.run();
    harness.get_by_role_and_label(Role::Button, item).click();
    harness.run();
}

/// The top of the line of the row that `label`, one of its controls, is
/// on.
fn top(harness: &Harness<'_, State>, label: &str) -> f32 {
    harness.get_by_label(label).rect().center().y
}

/// The end called `name` on the row that `label`, one of its controls, is
/// on.
fn end<'a>(harness: &'a Harness<'_, State>, name: &'a str, label: &str) -> egui_kittest::Node<'a> {
    let y = top(harness, label);
    (harness.get_all_by_label(name))
        .find(|end| (end.rect().center().y - y).abs() < 2.0)
        .unwrap_or_else(|| panic!("no {name} on the row of {label}"))
}

#[test]
fn the_routing_panel_lists_every_connection_under_its_kind() {
    let (project, _) = project();
    let harness = harness(project);
    // Each track's output, to the bus it feeds.
    for (track, bus) in [
        ("Kick", "Drums"),
        ("Bass", "Master"),
        ("Lead", "Master"),
        ("Double", "Master"),
        ("Print", "Master"),
        ("Drums", "Master"),
        ("Reverb", "Master"),
    ] {
        let label = format!("{track} Output");
        assert_eq!(value(&harness, &label).as_deref(), Some(bus), "{track}");
        harness.get_by_role_and_label(Role::Slider, &format!("{track} Volume"));
    }
    // The send, with its level and its Pre-Fader switch.
    harness.get_by_role_and_label(Role::Slider, "Kick Send to Reverb");
    harness.get_by_label("Pre-Fader Kick Send to Reverb");
    // The inputs from other tracks: another MIDI track's notes, and
    // another track's sound.
    let inputs = [
        ("Double", "from Lead, Pre FX"),
        ("Print", "from Bass, Post Fader"),
    ];
    for (track, from) in inputs {
        let label = format!("{track} Input");
        assert_eq!(value(&harness, &label).as_deref(), Some(from), "{track}");
    }
    // The key, from Kick into Bass's Compressor, with its gain.
    let source = "Bass Compressor Sidechain Source";
    assert_eq!(value(&harness, source).as_deref(), Some("Kick, Post FX"));
    harness.get_by_role_and_label(Role::Slider, "Bass Compressor Sidechain Gain");
    end(&harness, "Bass, Compressor", source);
    // Each kind under its heading, in that order.
    let rows = [
        "Outputs",
        "Kick Output",
        "Reverb Output",
        "Sends",
        "Kick Send to Reverb",
        "Inputs",
        "Double Input",
        "Print Input",
        "Keys",
        source,
    ];
    let tops: Vec<f32> = rows.iter().map(|label| top(&harness, label)).collect();
    assert!(tops.is_sorted_by(|a, b| a < b), "{tops:?}");
}

#[test]
fn without_tracks_or_connections_it_says_so() {
    let harness = harness(Project::new());
    harness.get_by_label(NO_TRACKS);
}

#[test]
fn the_routing_panel_shows_one_tracks_connections_alone() {
    let (project, ids) = project();
    let mut harness = harness(project);
    assert_eq!(value(&harness, "Show").as_deref(), Some(ALL_TRACKS));
    let shown = |harness: &Harness<'_, State>, label: &str| harness.query_by_label(label).is_some();
    open(&mut harness, "Show");
    pick(&mut harness, "Bass");
    // Those that come from Bass, go to it, or go to one of its devices.
    for label in [
        "Bass Output",
        "Print Input",
        "Bass Compressor Sidechain Source",
    ] {
        assert!(shown(&harness, label), "{label}");
    }
    for label in [
        "Kick Output",
        "Kick Send to Reverb",
        "Double Input",
        "Sends",
    ] {
        assert!(!shown(&harness, label), "{label}");
    }
    // The master's: what feeds it.
    open(&mut harness, "Show");
    pick(&mut harness, "Master");
    assert_eq!(value(&harness, "Show").as_deref(), Some("Master"));
    assert!(shown(&harness, "Drums Output") && !shown(&harness, "Kick Output"));
    assert!(!shown(&harness, "Inputs") && !shown(&harness, "Keys"));
    // A track that goes takes its filter with it.
    open(&mut harness, "Show");
    pick(&mut harness, "Print");
    assert!(shown(&harness, "Print Input") && !shown(&harness, "Bass Output"));
    let args = Args::new().with("tracks", vec![ids.print]);
    let delete = Action::RunWith(musicable_commands::track::DELETE, args);
    harness.state_mut().carry_out(&delete);
    harness.run();
    assert_eq!(value(&harness, "Show").as_deref(), Some(ALL_TRACKS));
    assert!(shown(&harness, "Kick Output") && !shown(&harness, "Print Input"));
    assert!(
        harness.state().errors.is_empty(),
        "{:?}",
        harness.state().errors
    );
}

#[test]
fn a_filter_that_shows_nothing_says_so() {
    let mut project = Project::new();
    let lonely = project.add_track("Lonely", TrackKind::Audio);
    project.track_mut(lonely).unwrap().output = Bus::SendsOnly;
    let mut harness = harness(project);
    harness.get_by_label("Lonely Output");
    open(&mut harness, "Show");
    pick(&mut harness, "Master");
    assert!(harness.query_by_label("Lonely Output").is_none());
    harness.get_by_label("Nothing comes from Master, or goes to it.");
}

#[test]
fn clicking_an_end_shows_its_track_or_device() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let selected = |harness: &Harness<'_, State>| harness.state().editor().selected_track;
    // The group Kick feeds.
    end(&harness, "Drums", "Kick Output").click();
    harness.run();
    assert_eq!(selected(&harness), Some(Chain::Track(ids.drums)));
    let tracks = &harness.state().editor().selection.tracks;
    assert_eq!(tracks.iter().copied().collect::<Vec<_>>(), [ids.drums]);
    // Where a track's output goes, the master.
    end(&harness, "Master", "Bass Output").click();
    harness.run();
    assert_eq!(selected(&harness), Some(Chain::Master));
    // The return track a send goes to.
    end(&harness, "Reverb", "Kick Send to Reverb").click();
    harness.run();
    assert_eq!(selected(&harness), Some(Chain::Track(ids.reverb)));
    // The track an input comes from, and the one it goes to.
    end(&harness, "Lead, Pre FX", "Double Input").click();
    harness.run();
    assert_eq!(selected(&harness), Some(Chain::Track(ids.lead)));
    end(&harness, "Double", "Double Input").click();
    harness.run();
    assert_eq!(selected(&harness), Some(Chain::Track(ids.double)));
    // The device a key goes into, in the devices panel.
    let source = "Bass Compressor Sidechain Source";
    let device = end(&harness, "Bass, Compressor", source);
    assert_eq!(device.accesskit_node().role(), Role::Link);
    device.click();
    harness.run();
    assert_eq!(selected(&harness), Some(Chain::Track(ids.bass)));
    let devices = &harness.state().editor().devices.selected;
    assert_eq!(
        devices.iter().copied().collect::<Vec<_>>(),
        [ids.compressor]
    );
    // It comes into sight there.
    let revealed = harness.state_mut().panel.take_revealed();
    assert_eq!(revealed, Some(ids.compressor));
    // Sends Only is nowhere to show.
    let output = Action::Mixer(Change::Output(ids.kick, Bus::SendsOnly));
    harness.state_mut().carry_out(&output);
    harness.run();
    let nowhere = end(&harness, "Sends Only", "Kick Output");
    assert_eq!(nowhere.accesskit_node().role(), Role::Label);
    assert!(
        harness.state().errors.is_empty(),
        "{:?}",
        harness.state().errors
    );
}

#[test]
fn each_row_edits_as_its_control_does() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let slider = |harness: &Harness<'_, State>, label: &str, value: f64| {
        let node = harness.get_by_role_and_label(Role::Slider, label);
        node.set_value_accesskit(value);
    };
    // An output's fader, in dB.
    slider(&harness, "Kick Volume", -6.0);
    harness.run();
    let kick = |harness: &Harness<'_, State>| harness.state().project().track(ids.kick).cloned();
    let gain = kick(&harness).unwrap().gain;
    assert!((gain - 0.501).abs() < 0.001, "{gain}");
    // Double-clicking it sets 0 dB, as a fader's double-click does.
    harness
        .get_by_role_and_label(Role::Slider, "Kick Volume")
        .click();
    harness.run();
    harness
        .get_by_role_and_label(Role::Slider, "Kick Volume")
        .click();
    harness.run();
    assert_eq!(kick(&harness).unwrap().gain, 1.0);
    // The output's menu has the strip's buses.
    open(&mut harness, "Kick Output");
    pick(&mut harness, "Master");
    assert_eq!(value(&harness, "Kick Output").as_deref(), Some("Master"));
    // The send's level and its switch.
    slider(&harness, "Kick Send to Reverb", -12.0);
    harness.run();
    harness
        .get_by_label("Pre-Fader Kick Send to Reverb")
        .click();
    harness.run();
    let send = kick(&harness).unwrap().sends[0];
    assert!((send.level - 0.251).abs() < 0.001, "{}", send.level);
    assert!(send.pre_fader);
    // Its right-click menu removes it, as the strip's does.
    harness
        .get_by_role_and_label(Role::Slider, "Kick Send to Reverb")
        .click_secondary();
    harness.run();
    harness.get_by_label("Remove Send").click();
    harness.run();
    assert!(kick(&harness).unwrap().sends.is_empty());
    assert!(harness.query_by_label("Sends").is_none());
    // An input's menu has No Input and the other tracks.
    open(&mut harness, "Print Input");
    harness.get_by_role_and_label(Role::Button, "Kick").hover();
    harness.run();
    harness.get_by_label("Post FX").click();
    harness.run();
    let from_kick = Input::Tap(Tap::PostFx(Chain::Track(ids.kick)));
    let print = |harness: &Harness<'_, State>| {
        let project = harness.state().project();
        project.track(ids.print).unwrap().input.clone()
    };
    assert_eq!(print(&harness), Some(from_kick));
    assert_eq!(
        value(&harness, "Print Input").as_deref(),
        Some("from Kick, Post FX")
    );
    open(&mut harness, "Print Input");
    harness.get_by_label("No Input").click();
    harness.run();
    assert_eq!(print(&harness), None);
    // With no input from a track, it's not a connection.
    assert!(harness.query_by_label("Print Input").is_none());
    // A key's source menu is the Sidechain column's, and its gain the
    // same bar.
    let source = "Bass Compressor Sidechain Source";
    open(&mut harness, source);
    harness.get_by_role_and_label(Role::Button, "Drums").hover();
    harness.run();
    harness.get_by_label("Post Fader").click();
    harness.run();
    assert_eq!(
        value(&harness, source).as_deref(),
        Some("Drums, Post Fader")
    );
    slider(&harness, "Bass Compressor Sidechain Gain", 0.5);
    harness.run();
    let key = *harness.state().project().wire(ids.key).unwrap();
    assert_eq!(key.from, Tap::PostFader(Chain::Track(ids.drums)));
    assert_eq!(key.gain, 0.5);
    open(&mut harness, source);
    harness.get_by_label("No Input").click();
    harness.run();
    assert!(harness.state().project().wires.is_empty());
    assert!(harness.query_by_label("Keys").is_none());
    let ran = (harness.state().actions.iter()).filter(|action| match action {
        Action::RunWith(id, _) => *id == wire::SET_SOURCE || *id == wire::DISCONNECT,
        _ => false,
    });
    assert_eq!(ran.count(), 2);
    assert!(
        harness.state().errors.is_empty(),
        "{:?}",
        harness.state().errors
    );
}

#[test]
fn a_second_wire_into_a_side_input_is_numbered() {
    let (mut project, ids) = project();
    let port = Port::Sidechain(ids.compressor);
    let wire = project.new_wire(Tap::PostFx(Chain::Track(ids.lead)), port);
    project.wires.push(wire);
    let wire = project.new_wire(Tap::PostFx(Chain::Track(ids.drums)), port);
    project.wires.push(wire);
    let harness = harness(project);
    let source = "Bass Compressor Sidechain Source";
    assert_eq!(value(&harness, source).as_deref(), Some("Kick, Post FX"));
    let second = format!("{source} 2");
    assert_eq!(value(&harness, &second).as_deref(), Some("Lead, Post FX"));
    harness.get_by_role_and_label(Role::Slider, "Bass Compressor Sidechain Gain 3");
}

#[test]
fn a_note_wire_plays_its_notes_on_the_channel_picked() {
    let (mut project, ids) = project();
    let strings = project.add_track("Strings", TrackKind::Midi);
    let synth = project.new_device(DeviceKind::TestInstrument { level: 0.5 });
    let port = Port::Notes(synth.id);
    project.track_mut(strings).unwrap().devices.push(synth);
    let wire = project.new_wire(Tap::PostFx(Chain::Track(ids.lead)), port);
    let id = wire.id;
    project.wires.push(wire);
    let mut harness = harness(project);
    // Under a heading of its own, after the keys: where its notes come
    // from, the instrument they play, and its channel.
    let source = "Strings Device Notes Source";
    let channel = "Strings Device Notes Channel";
    assert_eq!(value(&harness, source).as_deref(), Some("Lead, Post FX"));
    assert_eq!(value(&harness, channel).as_deref(), Some("Any Channel"));
    end(&harness, "Strings, Device", source);
    let key = "Bass Compressor Sidechain Source";
    let rows = ["Keys", key, "Notes", source];
    let tops: Vec<f32> = rows.iter().map(|label| top(&harness, label)).collect();
    assert!(tops.is_sorted_by(|a, b| a < b), "{tops:?}");
    // It has no gain.
    assert!(
        harness
            .query_by_label("Strings Device Notes Gain")
            .is_none()
    );

    // Picking a channel plays them on it.
    open(&mut harness, channel);
    pick(&mut harness, "Channel 3");
    assert_eq!(harness.state().project().wire(id).unwrap().channel, Some(2));
    assert_eq!(value(&harness, channel).as_deref(), Some("Channel 3"));
    open(&mut harness, channel);
    pick(&mut harness, "Any Channel");
    assert_eq!(harness.state().project().wire(id).unwrap().channel, None);

    // Its source menu has the points that carry notes: Double's, a MIDI
    // track's, but none of an audio track's.
    open(&mut harness, source);
    harness
        .get_by_role_and_label(Role::Button, "Double")
        .hover();
    harness.run();
    for point in ["Pre FX", "Post FX"] {
        assert!(harness.query_by_label(point).is_some(), "{point}");
    }
    assert!(harness.query_by_label("Post Fader").is_none());
    harness.get_by_label("Pre FX").click();
    harness.run();
    let from = harness.state().project().wire(id).unwrap().from;
    assert_eq!(from, Tap::PreFx(ids.double));
    assert!(
        harness.state().errors.is_empty(),
        "{:?}",
        harness.state().errors
    );
}

#[test]
fn the_routing_panel_docks_beside_the_mixer_and_brings_devices_into_sight() {
    use crate::panels::{DEVICES, MIXER, PIANO_ROLL, ROUTING};

    let (project, ids) = project();
    let mut harness = crate::testing::harness(project);
    let shell = &mut harness.state_mut().shell;
    shell.toggle_panel(ROUTING);
    shell.switch_devices_clip_editor(PIANO_ROLL);
    harness.run();
    // In front of the mixer, in a tab beside it.
    let shell = &harness.state().shell;
    assert!(shell.panel_shown(ROUTING) && !shell.panel_shown(MIXER));
    assert!(!shell.panel_shown(DEVICES));
    harness.get_by_label("Routing panel");
    // Bass's connections, for the key to be in sight.
    harness
        .get_by_role_and_label(Role::ComboBox, "Show")
        .click();
    harness.run();
    harness.get_by_role_and_label(Role::Button, "Bass").click();
    harness.run();
    harness
        .get_by_role_and_label(Role::Link, "Bass, Compressor")
        .click();
    harness.run();
    let args = Args::new().with("devices", vec![ids.compressor]);
    let select = Action::RunWith(device::SELECT, args);
    assert!(harness.state().actions.contains(&select));
    // The devices come forward, from behind the piano roll.
    let shell = &harness.state().shell;
    assert!(shell.panel_shown(DEVICES) && !shell.panel_shown(PIANO_ROLL));
}

#[test]
fn each_end_is_as_wide_as_its_widest_name() {
    let theme = Theme::default();
    let sizes = &theme.mixer;
    let gain = 2.0 * sizes.strip;
    // In a wide panel, each end fits the names in it, and a strip's width
    // at least, for the rows to read across.
    let columns = Columns::across(2000.0, &theme, [150.0, 10.0]);
    assert_eq!((columns.from, columns.to), (150.0, sizes.strip));
    assert_eq!(columns.gain, gain);
    // In a narrow one, the ends share what the gain and the arrow leave.
    let columns = Columns::across(600.0, &theme, [400.0, 400.0]);
    let room = (600.0 - gain - columns.arrow - 3.0 * sizes.gap) / 2.0;
    assert_eq!((columns.from, columns.to), (room, room));
}
