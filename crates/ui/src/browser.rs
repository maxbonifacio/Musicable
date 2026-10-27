//! The browser, beside the arrangement, as in Live: the samples in the
//! folders added to the library, the devices to put on tracks, and the
//! presets, each with a search. Picking a sample previews it, a double-click
//! puts it on the selected track, and samples, devices and presets drag onto
//! the arrangement's tracks and the mixer's strips, or into the devices
//! panel: between the devices there, or a sample onto a sampler. The devices
//! panel's own devices drag the same way.
//!
//! What the browser does, it does with the library's commands, and what it
//! lists is what the library's last search or Find Similar found, so a
//! search run from elsewhere, such as by an agent, shows here too.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use egui::{
    Align, Align2, Area, Button, Context, FontId, Frame, Id, Key, Label, Layout, Mesh, Modifiers,
    Order, Popup, PopupCloseBehavior, Rect, Response, RichText, Role, ScrollArea, Sense, Shape,
    Slider, Stroke, StrokeKind, TextEdit, Ui, WidgetInfo, pos2, vec2,
};
use musicable_commands::{Args, browser, device};
use musicable_library::{FileId, Index, Preset, PresetKind, Record};
use musicable_model::native::NativeKind;
use musicable_model::time::Beats;
use musicable_model::{Chain, DeviceId, PluginFormat, TrackId};

use crate::devices::{self, PluginEntry, onto};
use crate::search::score;
use crate::shell::{Action, View};
use crate::theme::Theme;
use crate::waveform::Peaks;

/// The quietest previews play, in dB, and the loudest.
pub const MIN_PREVIEW_VOLUME: i32 = -60;
pub const MAX_PREVIEW_VOLUME: i32 = 6;

/// The search fields' ids, so a key can focus them.
const SAMPLE_SEARCH: &str = "sample search";
const PRESET_SEARCH: &str = "preset search";

/// The browser's library, as the app has it this frame.
#[derive(Debug, Clone, Copy)]
pub struct Library<'a> {
    /// The folders added, and the audio files in them.
    pub index: &'a Index,
    /// Whether the app has read the library's file: until then it's empty.
    pub loaded: bool,
    /// The folders being looked through, and how many files each has found
    /// so far.
    pub scanning: &'a [(PathBuf, usize)],
    /// The folders whose files' headers are being read, and how many are
    /// left to read in each.
    pub reading: &'a [(PathBuf, usize)],
    /// What the library lists.
    pub listing: Listing<'a>,
    /// The files listed, best first, and how many were found in all: the
    /// list stops at a thousand.
    pub found: &'a [FileId],
    pub total: usize,
    pub presets: &'a [Preset],
    /// Whether the presets are being looked for.
    pub finding_presets: bool,
    /// The sample previewing, or last previewed.
    pub preview: Option<Preview<'a>>,
    /// Whether picking a sample previews it.
    pub auto_preview: bool,
    /// How loud previews play, in dB.
    pub preview_volume: i32,
}

impl Default for Library<'_> {
    /// An empty library, read.
    fn default() -> Self {
        static EMPTY: Index = Index::new();
        Self {
            index: &EMPTY,
            loaded: true,
            scanning: &[],
            reading: &[],
            listing: Listing::Search(""),
            found: &[],
            total: 0,
            presets: &[],
            finding_presets: false,
            preview: None,
            auto_preview: true,
            preview_volume: 0,
        }
    }
}

/// What the library lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing<'a> {
    /// What a search found: nothing for an empty one, when the browser
    /// shows the folders instead.
    Search(&'a str),
    /// The files most like this one.
    Similar(&'a Path),
}

/// The sample previewing, or the one last previewed.
#[derive(Debug, Clone, Copy)]
pub struct Preview<'a> {
    pub path: &'a Path,
    /// Where it's playing, in its own frames, while it plays.
    pub at: Option<u64>,
    /// How long it is, in its frames, once it's open.
    pub frames: Option<u64>,
    /// Its waveform, once it's drawn.
    pub peaks: Option<&'a Peaks>,
}

/// Something dragged from the browser, or from the devices panel: egui's
/// drag-and-drop payload.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Dragged {
    Sample(PathBuf),
    /// One of Musicable's own devices.
    Native(NativeKind),
    Plugin(PluginEntry),
    Preset {
        path: PathBuf,
        name: String,
    },
    /// Devices of one chain, by their title bars in the devices panel: they
    /// move where they're let go, or copies of them go there with `copy`.
    /// `name` is the device's, or says how many there are.
    Devices {
        devices: Vec<DeviceId>,
        copy: bool,
        name: String,
    },
}

/// Where something dragged is let go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Destination {
    /// A track, at a time on it over the arrangement's lanes.
    Track {
        track: TrackId,
        audio: bool,
        at: Option<Beats>,
    },
    /// Under the tracks, for a track of its own.
    NewTrack { at: Option<Beats> },
    /// The end of a chain, or of the selected track's without one: the
    /// master's, a mixer strip's, or an empty one in the devices panel.
    Chain(Option<Chain>),
    /// Between the devices of the chain the devices panel shows: the place
    /// the first of what's dragged takes, counting the chain without the
    /// devices dragged along it.
    Devices { chain: Chain, index: usize },
    /// A sampler in the devices panel, which takes a sample as its own.
    Sampler(DeviceId),
}

impl Dragged {
    /// What it's called by the pointer while it's dragged.
    pub(crate) fn name(&self) -> String {
        match self {
            Self::Sample(path) => stem(path),
            Self::Native(kind) => kind.name().to_owned(),
            Self::Plugin(plugin) => plugin.name.clone(),
            Self::Preset { name, .. } => name.clone(),
            Self::Devices {
                name, copy: true, ..
            } => format!("Copy {name}"),
            Self::Devices { name, .. } => name.clone(),
        }
    }

    /// What letting it go at `target` asks for, if it can go there. A sample
    /// goes on an audio track, or else where Insert Audio puts it, or into
    /// a sampler.
    pub(crate) fn dropped(&self, target: &Destination) -> Option<Action> {
        let args = match self {
            Self::Sample(file) => {
                let (track, at) = match *target {
                    Destination::Track { track, audio, at } => (audio.then_some(track), at),
                    Destination::NewTrack { at } => (None, at),
                    Destination::Chain(_) | Destination::Devices { .. } => return None,
                    Destination::Sampler(device) => {
                        let file = file.clone();
                        return Some(Action::LoadSample { device, file });
                    }
                };
                let files = vec![file.clone()];
                return Some(Action::DropFiles { files, track, at });
            }
            Self::Native(kind) => (device::ADD, Args::new().with("kind", kind.key())),
            Self::Plugin(plugin) => (device::ADD_PLUGIN, devices::add(plugin, None)),
            Self::Preset { path, .. } => (device::ADD_PRESET, preset_args(path)),
            Self::Devices { devices, copy, .. } => {
                let command = if *copy { device::COPY } else { device::MOVE };
                (command, Args::new().with("devices", devices.clone()))
            }
        };
        let (command, args) = args;
        let args = match *target {
            Destination::Track { track, .. } => args.with("track", track),
            Destination::NewTrack { .. } => args.with("new_track", true),
            Destination::Chain(chain) => onto(args, chain),
            Destination::Devices { chain, index } => {
                onto(args, Some(chain)).with("index", index as i64)
            }
            Destination::Sampler(_) => return None,
        };
        Some(Action::RunWith(command, args))
    }
}

/// What's dragged, if it can go to `target`.
pub(crate) fn dragged_to(ctx: &Context, target: &Destination) -> Option<Dragged> {
    let dragged = egui::DragAndDrop::payload::<Dragged>(ctx)?;
    dragged.dropped(target).map(|_| (*dragged).clone())
}

/// Lets something dragged go into `rect` as `target`, such as a mixer
/// strip's chain: outlines `rect` while it's over it, and returns what
/// letting it go there asks for.
pub(crate) fn drop_in(
    ctx: &Context,
    rect: Rect,
    target: &Destination,
    theme: &Theme,
) -> Option<Action> {
    let over = ctx
        .pointer_latest_pos()
        .is_some_and(|pos| rect.contains(pos));
    let dragged = dragged_to(ctx, target).filter(|_| over)?;
    let layer = egui::LayerId::new(Order::Foreground, Id::unique("drop outline"));
    let outline = Stroke::new(2.0, theme.start_marker);
    let painter = ctx.layer_painter(layer);
    painter.rect_stroke(rect.shrink(1.0), 0.0, outline, StrokeKind::Inside);
    if !ctx.input(|i| i.pointer.any_released()) {
        return None;
    }
    egui::DragAndDrop::clear_payload(ctx);
    dragged.dropped(target)
}

/// Shows the name of what's dragged by the pointer, if it's in this
/// window: past its edge, the window it's over shows it.
pub(crate) fn show_dragged(ctx: &Context) {
    let Some(dragged) = egui::DragAndDrop::payload::<Dragged>(ctx) else {
        return;
    };
    let pos = ctx.pointer_latest_pos();
    let Some(pos) = pos.filter(|&pos| ctx.content_rect().contains(pos)) else {
        return;
    };
    Area::new(Id::unique("dragged from the browser"))
        .order(Order::Tooltip)
        .fixed_pos(pos + vec2(14.0, 14.0))
        .interactable(false)
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.label(dragged.name());
            });
        });
}

/// Which of the browser's lists shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Samples,
    Devices,
    Presets,
}

/// The browser's own state.
#[derive(Debug, Default)]
pub(crate) struct BrowserPanel {
    tab: Tab,
    samples: Samples,
    /// Musicable's own devices and the plugins, under Devices.
    devices: devices::Browser,
    presets: Presets,
    /// Whether to put the keys in the search field of the list showing.
    focus: bool,
}

impl BrowserPanel {
    /// Shows the list `view` and `chain` call for, with tabs for the others.
    /// Devices and presets go to `chain`'s devices, or the selected track's
    /// without a chain.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        chain: Option<Chain>,
        theme: &Theme,
        actions: &mut Vec<Action>,
    ) {
        ui.add_space(theme.devices.margin);
        ui.horizontal(|ui| {
            for (tab, title) in [
                (Tab::Samples, "Samples"),
                (Tab::Devices, "Devices"),
                (Tab::Presets, "Presets"),
            ] {
                if ui.selectable_label(self.tab == tab, title).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.separator();
        if std::mem::take(&mut self.focus) {
            let id = match self.tab {
                Tab::Samples => Id::unique(SAMPLE_SEARCH),
                Tab::Devices => Id::unique(devices::PLUGIN_SEARCH),
                Tab::Presets => Id::unique(PRESET_SEARCH),
            };
            ui.memory_mut(|memory| memory.request_focus(id));
        }
        match self.tab {
            Tab::Samples => self.samples.show(ui, view, theme, actions),
            Tab::Devices => self.devices.show(ui, view, chain, theme, actions),
            Tab::Presets => self.presets.show(ui, view, chain, theme, actions),
        }
    }

    /// Puts the keys in the search field of the list showing, from the next
    /// frame.
    pub fn focus_search(&mut self) {
        self.focus = true;
    }

    /// Shows the devices to add.
    pub fn show_devices(&mut self) {
        self.tab = Tab::Devices;
    }
}

/// The samples list's own state.
#[derive(Debug, Default)]
struct Samples {
    /// What's typed in the search field.
    search: String,
    /// The search last asked for, so one run from elsewhere shows in the
    /// field.
    asked: Option<String>,
    selected: Option<PathBuf>,
    /// The folders open in the tree of folders, by their place's folder and
    /// their path from it, with `/` after each name: empty for the place.
    open: BTreeSet<(PathBuf, String)>,
    /// The tree's rows, and the index's generation and the open folders'
    /// they were worked out for.
    tree: Vec<TreeRow>,
    tree_for: Option<(u64, u64)>,
    /// Counts the changes to `open`.
    opened: u64,
    /// The row to scroll to, and the list's scrolling and height last
    /// frame.
    reveal: Option<usize>,
    scrolled: f32,
    height: f32,
    /// The tag typed into a file's menu.
    new_tag: String,
}

/// A row of the tree of folders.
#[derive(Debug, Clone, PartialEq)]
struct TreeRow {
    depth: usize,
    kind: TreeKind,
}

#[derive(Debug, Clone, PartialEq)]
enum TreeKind {
    /// A place, by its index, open or not.
    Place {
        place: usize,
        open: bool,
    },
    /// A folder in a place, by its path from the place's folder with `/`
    /// after each name.
    Folder {
        place: usize,
        folder: String,
        open: bool,
    },
    File(FileId),
}

impl Samples {
    fn show(&mut self, ui: &mut Ui, view: &View<'_>, theme: &Theme, actions: &mut Vec<Action>) {
        let library = &view.library;
        if let Listing::Search(text) = library.listing
            && self.asked.as_deref() != Some(text)
        {
            self.search = text.to_owned();
            self.asked = Some(text.to_owned());
        }
        let id = Id::unique(SAMPLE_SEARCH);
        // Up and Down go through the list while the search field has the
        // keys, as in Live.
        let step = match ui.memory(|memory| memory.has_focus(id)) {
            true => ui.input_mut(|i| {
                match (
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                ) {
                    (true, false) => -1,
                    (false, true) => 1,
                    _ => 0,
                }
            }),
            false => 0,
        };
        let field = TextEdit::singleline(&mut self.search)
            .id(id)
            .hint_text("Search samples")
            .desired_width(f32::INFINITY);
        if ui.add(field).changed() {
            self.asked = Some(self.search.clone());
            let args = Args::new().with("text", self.search.as_str());
            actions.push(Action::RunWith(browser::SEARCH, args));
        }
        if let Listing::Similar(like) = library.listing {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Like {}", stem(like))).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let back = ui.small_button("×").on_hover_text("Back to the search");
                    back.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Back to Search"));
                    if back.clicked() {
                        let args = Args::new().with("text", self.search.as_str());
                        actions.push(Action::RunWith(browser::SEARCH, args));
                    }
                });
            });
        }
        let tree = library.listing == Listing::Search("");
        if tree {
            self.update_tree(library.index);
        }
        let files: Vec<FileId> = match tree {
            true => (self.tree.iter())
                .filter_map(|row| match row.kind {
                    TreeKind::File(file) => Some(file),
                    _ => None,
                })
                .collect(),
            false => library.found.to_vec(),
        };
        if step != 0 {
            self.step(library, &files, step, tree, actions);
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            preview_strip(ui, library, theme, actions);
            ui.separator();
            self.status(ui, library, tree, actions);
            ui.separator();
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                self.list(ui, view, tree, theme, actions);
            });
        });
    }

    /// Works the tree's rows out again if the index or the open folders
    /// changed.
    fn update_tree(&mut self, index: &Index) {
        let key = (index.generation(), self.opened);
        if self.tree_for == Some(key) {
            return;
        }
        self.tree_for = Some(key);
        self.tree.clear();
        for (number, place) in index.places().iter().enumerate() {
            let folder = place.folder().to_owned();
            let open = self.open.contains(&(folder.clone(), String::new()));
            self.tree.push(TreeRow {
                depth: 0,
                kind: TreeKind::Place {
                    place: number,
                    open,
                },
            });
            if open {
                let open = |path: &str| self.open.contains(&(folder.clone(), path.to_owned()));
                tree_rows(place.files(), number, "", 1, &open, &mut self.tree);
            }
        }
    }

    /// Selects the file `step` files after the selected one in `files`,
    /// and previews it.
    fn step(
        &mut self,
        library: &Library<'_>,
        files: &[FileId],
        step: isize,
        tree: bool,
        actions: &mut Vec<Action>,
    ) {
        let index = library.index;
        let at = self.selected.as_deref().and_then(|path| index.find(path));
        let next = match at.and_then(|at| files.iter().position(|&file| file == at)) {
            Some(i) => i.checked_add_signed(step).filter(|&i| i < files.len()),
            None => (!files.is_empty()).then_some(0),
        };
        let Some(next) = next else {
            return;
        };
        let Some(path) = index.path_of(files[next]) else {
            return;
        };
        self.reveal = Some(match tree {
            true => (self.tree.iter())
                .position(|row| row.kind == TreeKind::File(files[next]))
                .unwrap_or(0),
            false => next,
        });
        self.pick(path, library, actions);
    }

    /// Selects the file at `path`, and previews it if previews are on.
    fn pick(&mut self, path: PathBuf, library: &Library<'_>, actions: &mut Vec<Action>) {
        if library.auto_preview {
            actions.push(preview(&path));
        }
        self.selected = Some(path);
    }

    /// The list: the tree of folders without a search, or what the search
    /// or Find Similar found.
    fn list(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        tree: bool,
        theme: &Theme,
        actions: &mut Vec<Action>,
    ) {
        let library = &view.library;
        let index = library.index;
        if tree && index.places().is_empty() {
            if library.loaded {
                let text = "Add a folder of samples to the library to browse, search and \
                            preview them here.";
                ui.add(Label::new(RichText::new(text).weak()).wrap());
                if ui.button("Add Folder...").clicked() {
                    actions.push(Action::Run(browser::ADD_FOLDER));
                }
            }
            return;
        }
        if !tree && library.found.is_empty() {
            let text = match library.listing {
                Listing::Similar(_) => "Nothing like it.",
                Listing::Search(_) => "No samples match.",
            };
            ui.label(RichText::new(text).weak());
            return;
        }
        let rows = if tree {
            self.tree.len()
        } else {
            library.found.len()
        };
        let selected = self.selected.as_deref().and_then(|path| index.find(path));
        let height = row_height(ui);
        let mut scroll = ScrollArea::vertical().id_salt("samples").auto_shrink(false);
        if let Some(row) = self.reveal.take() {
            // As little scrolling as shows the whole row, or its top in a
            // list shorter than a row.
            let spacing = ui.spacing().item_spacing.y;
            let top = row as f32 * (height + spacing);
            let offset = self.scrolled.max(top + height - self.height).min(top);
            scroll = scroll.vertical_scroll_offset(offset);
        }
        let output = scroll.show_rows(ui, height, rows, |ui, range| {
            for row in range {
                if tree {
                    let kind = self.tree[row].kind.clone();
                    let depth = self.tree[row].depth;
                    self.tree_row(ui, view, depth, &kind, selected, theme, actions);
                } else {
                    let file = library.found[row];
                    self.file_row(ui, view, file, 0, true, selected, theme, actions);
                }
            }
        });
        self.scrolled = output.state.clamped_offset().y;
        self.height = output.inner_rect.height();
    }

    #[allow(clippy::too_many_arguments)]
    fn tree_row(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        depth: usize,
        kind: &TreeKind,
        selected: Option<FileId>,
        theme: &Theme,
        actions: &mut Vec<Action>,
    ) {
        let index = view.library.index;
        let (place, folder, open) = match kind {
            TreeKind::File(file) => {
                self.file_row(ui, view, *file, depth, false, selected, theme, actions);
                return;
            }
            TreeKind::Place { place, open } => (*place, String::new(), *open),
            TreeKind::Folder {
                place,
                folder,
                open,
            } => (*place, folder.clone(), *open),
        };
        let Some(found) = index.places().get(place) else {
            return;
        };
        let name = match folder.trim_end_matches('/').rsplit_once('/') {
            _ if folder.is_empty() => found.name(),
            Some((_, name)) => name.to_owned(),
            None => folder.trim_end_matches('/').to_owned(),
        };
        let mut readings = view.library.reading.iter();
        let reading = readings.find(|(f, _)| f == found.folder());
        let detail = match (
            folder.is_empty(),
            self.scanning(view, found.folder()),
            reading,
        ) {
            (true, Some(files), _) => format!("{} files...", thousands(files)),
            (true, None, Some(&(_, left))) => format!("{} to read...", thousands(left)),
            _ => String::new(),
        };
        let row = row(ui, theme, depth, Some(open), &name, &detail, false);
        let key = (found.folder().to_owned(), folder.clone());
        if row.clicked() {
            match open {
                true => self.open.remove(&key),
                false => self.open.insert(key),
            };
            self.opened += 1;
        }
        if folder.is_empty() {
            let row = row.on_hover_text(format!(
                "{}\n{} samples",
                found.folder().display(),
                thousands(found.files().len())
            ));
            row.context_menu(|ui| {
                let args = Args::new().with("folder", found.folder().to_string_lossy().as_ref());
                if ui.button("Rescan").clicked() {
                    actions.push(Action::RunWith(browser::RESCAN, args.clone()));
                }
                if ui.button("Remove from Library").clicked() {
                    actions.push(Action::RunWith(browser::REMOVE_FOLDER, args));
                }
            });
        }
    }

    /// How many files the scan of `folder` has found so far, while it runs.
    fn scanning(&self, view: &View<'_>, folder: &Path) -> Option<usize> {
        let mut scans = view.library.scanning.iter();
        scans.find(|(f, _)| f == folder).map(|&(_, files)| files)
    }

    /// The row of the file `file`, `depth` steps in, which says what
    /// folder it's in if `in_folder`.
    #[allow(clippy::too_many_arguments)]
    fn file_row(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        file: FileId,
        depth: usize,
        in_folder: bool,
        selected: Option<FileId>,
        theme: &Theme,
        actions: &mut Vec<Action>,
    ) {
        let library = &view.library;
        let index = library.index;
        let (Some(record), Some(path)) = (index.get(file), index.path_of(file)) else {
            return;
        };
        let detail = match in_folder {
            true => record.folder().to_owned(),
            false => String::new(),
        };
        let response = row(
            ui,
            theme,
            depth,
            None,
            record.stem(),
            &detail,
            selected == Some(file),
        );
        response.dnd_set_drag_payload(Dragged::Sample(path.clone()));
        if response.double_clicked() {
            actions.push(insert(&path));
        } else if response.clicked() {
            self.pick(path.clone(), library, actions);
        }
        let response = response.on_hover_text(describe(record, &path));
        // Open while the tag field takes clicks.
        let menu =
            Popup::context_menu(&response).close_behavior(PopupCloseBehavior::CloseOnClickOutside);
        menu.show(|ui| {
            self.file_menu(ui, record, &path, actions);
        });
    }

    /// What a file's menu offers: previewing it, inserting it, finding
    /// files like it, and its tags.
    fn file_menu(&mut self, ui: &mut Ui, record: &Record, path: &Path, actions: &mut Vec<Action>) {
        let file = path.to_string_lossy();
        let similar = Args::new().with("file", file.as_ref());
        for (title, action) in [
            ("Preview", preview(path)),
            ("Insert", insert(path)),
            (
                "Find Similar",
                Action::RunWith(browser::FIND_SIMILAR, similar),
            ),
        ] {
            if ui.button(title).clicked() {
                actions.push(action);
                ui.close();
            }
        }
        ui.separator();
        ui.label(RichText::new("Tags").weak());
        let tag = |tag: &str, on: bool| {
            let args = Args::new()
                .with("file", file.as_ref())
                .with("tag", tag)
                .with("on", on);
            Action::RunWith(browser::TAG, args)
        };
        for given in &record.tags {
            let button = ui.add(Button::selectable(true, given.id.as_str()));
            if button.on_hover_text("Click to take the tag away").clicked() {
                actions.push(tag(&given.id, false));
                ui.close();
            }
        }
        let field = TextEdit::singleline(&mut self.new_tag)
            .hint_text("New tag")
            .desired_width(120.0);
        let response = ui.add(field);
        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            let typed = std::mem::take(&mut self.new_tag);
            if !typed.trim().is_empty() {
                actions.push(tag(typed.trim(), true));
            }
            ui.close();
        }
    }

    /// The line under the list: how many samples there are or were found,
    /// or how a scan or the reading of headers after it is going, and the
    /// button that adds folders.
    fn status(&self, ui: &mut Ui, library: &Library<'_>, tree: bool, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let add = ui.button("+").on_hover_text("Add a folder of samples");
                add.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Add Folder"));
                if add.clicked() {
                    actions.push(Action::Run(browser::ADD_FOLDER));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let scanned: usize = library.scanning.iter().map(|&(_, files)| files).sum();
                    let text = if !library.loaded {
                        ui.spinner();
                        "Reading the library...".to_owned()
                    } else if !library.scanning.is_empty() {
                        ui.spinner();
                        format!("Looking through: {} files", thousands(scanned))
                    } else if !library.reading.is_empty() {
                        ui.spinner();
                        let left = library.reading.iter().map(|&(_, left)| left).sum();
                        format!("Reading: {} left", count(left, "file"))
                    } else if tree {
                        let places = library.index.places().len();
                        format!(
                            "{} in {}",
                            count(library.index.len(), "sample"),
                            count(places, "folder")
                        )
                    } else if library.total > library.found.len() {
                        format!(
                            "{} of {} found",
                            thousands(library.found.len()),
                            thousands(library.total)
                        )
                    } else {
                        format!("{} found", thousands(library.total))
                    };
                    ui.add(Label::new(RichText::new(text).weak()).truncate());
                });
            });
        });
    }
}

/// Adds to `rows` the rows of the folder `folder` of the place numbered
/// `place`, whose files are `files`, `depth` steps in: its folders, those
/// `open` says are open with what's in them, then its files.
fn tree_rows(
    files: &[Record],
    place: usize,
    folder: &str,
    depth: usize,
    open: &dyn Fn(&str) -> bool,
    rows: &mut Vec<TreeRow>,
) {
    // The files in a folder are a run of them, as they're in order of path.
    let start = files.partition_point(|record| record.path.as_str() < folder);
    let end = start + files[start..].partition_point(|record| record.path.starts_with(folder));
    let mut folders = Vec::new();
    let mut found = Vec::new();
    let mut i = start;
    while i < end {
        let rest = &files[i].path[folder.len()..];
        match rest.split_once('/') {
            Some((name, _)) => {
                let path = format!("{folder}{name}/");
                let inside = files[i..end].partition_point(|record| record.path.starts_with(&path));
                folders.push(path);
                i += inside;
            }
            None => {
                found.push(i);
                i += 1;
            }
        }
    }
    folders.sort_by_cached_key(|path| path.trim_end_matches('/').to_lowercase());
    found.sort_by_cached_key(|&i| files[i].name().to_lowercase());
    for path in folders {
        let is_open = open(&path);
        rows.push(TreeRow {
            depth,
            kind: TreeKind::Folder {
                place,
                folder: path.clone(),
                open: is_open,
            },
        });
        if is_open {
            tree_rows(files, place, &path, depth + 1, open, rows);
        }
    }
    rows.extend(found.into_iter().map(|file| TreeRow {
        depth,
        kind: TreeKind::File(FileId { place, file }),
    }));
}

/// The strip under the samples: the waveform of the sample previewing or
/// last previewed, with where it's playing, the button that turns previews
/// on picking on and off, and the previews' volume.
fn preview_strip(ui: &mut Ui, library: &Library<'_>, theme: &Theme, actions: &mut Vec<Action>) {
    let preview = library.preview;
    let size = vec2(ui.available_width(), theme.browser.waveform);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let label = preview.map_or("Preview".to_owned(), |p| {
        format!("Preview of {}", stem(p.path))
    });
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
    if let Some(preview) = preview
        && response.on_hover_text("Click to play it again").clicked()
    {
        actions.push(self::preview(preview.path));
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, theme.corner, theme.lane);
        if let Some(preview) = preview {
            if let Some(peaks) = preview.peaks {
                waveform(&painter, rect, peaks, theme);
            }
            let length = preview.frames.or(preview.peaks.map(Peaks::frames));
            if let (Some(at), Some(length)) = (preview.at, length.filter(|&f| f > 0)) {
                let x = rect.left() + rect.width() * (at as f32 / length as f32).min(1.0);
                let line = Stroke::new(1.0, theme.playhead);
                painter.vline(x, rect.y_range(), line);
            }
            let font = FontId::proportional(theme.small_text);
            let name =
                painter.layout_no_wrap(stem(preview.path).to_owned(), font, theme.header_text);
            let at = rect.left_top() + vec2(theme.padding, 2.0);
            // On a backdrop, so the name reads over the waveform.
            let backdrop = Rect::from_min_size(at, name.size()).expand(2.0);
            painter.rect_filled(backdrop, theme.corner, theme.lane.gamma_multiply(0.8));
            painter.galley(at, name, theme.header_text);
        }
    }
    ui.horizontal(|ui| {
        let on = library.auto_preview;
        let auto = ui
            .add(Button::selectable(on, "Auto"))
            .on_hover_text("Previews samples when they're picked");
        auto.widget_info(|| WidgetInfo::selected(Role::Button, true, on, "Auto Preview"));
        if auto.clicked() {
            actions.push(Action::SetAutoPreview(!on));
        }
        let playing = preview.is_some_and(|p| p.at.is_some());
        let stop = ui.add_enabled(playing, Button::new("Stop"));
        stop.widget_info(|| WidgetInfo::labeled(Role::Button, playing, "Stop Preview"));
        if stop.clicked() {
            actions.push(Action::Run(browser::STOP_PREVIEW));
        }
        let mut volume = library.preview_volume;
        ui.spacing_mut().slider_width = (ui.available_width() - 60.0).max(40.0);
        let slider = Slider::new(&mut volume, MIN_PREVIEW_VOLUME..=MAX_PREVIEW_VOLUME)
            .suffix(" dB")
            .clamping(egui::SliderClamping::Always);
        let slider = ui.add(slider).on_hover_text("How loud previews play");
        let value = f64::from(volume);
        slider.widget_info(|| WidgetInfo::slider(true, value, "Preview Volume"));
        if slider.changed() {
            actions.push(Action::SetPreviewVolume(volume));
        }
    });
}

/// `peaks`, across `rect`.
fn waveform(painter: &egui::Painter, rect: Rect, peaks: &Peaks, theme: &Theme) {
    let frames = peaks.frames() as f64;
    let width = f64::from(rect.width().max(1.0));
    let middle = rect.center().y;
    let half = (rect.height() / 2.0 - 1.0).max(0.0);
    let mut mesh = Mesh::default();
    let mut x = rect.left();
    while x < rect.right() {
        let along = f64::from(x - rect.left());
        let range = peaks.range(along / width * frames, (along + 1.0) / width * frames);
        if let Some(peak) = range {
            let top = middle - peak.max.clamp(-1.0, 1.0) * half;
            let bottom = (middle - peak.min.clamp(-1.0, 1.0) * half).max(top + 1.0);
            let column = Rect::from_x_y_ranges(x..=x + 1.0, top..=bottom);
            mesh.add_colored_rect(column, theme.preview_wave);
        }
        x += 1.0;
    }
    painter.add(Shape::mesh(mesh));
}

/// The presets list's own state.
#[derive(Debug, Default)]
struct Presets {
    /// What's typed in the search field.
    search: String,
}

impl Presets {
    /// Lists the presets for the devices there are, by device, and adds the
    /// one the user picks to `chain`'s devices.
    fn show(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        chain: Option<Chain>,
        theme: &Theme,
        actions: &mut Vec<Action>,
    ) {
        let library = &view.library;
        let field = TextEdit::singleline(&mut self.search)
            .id(Id::unique(PRESET_SEARCH))
            .hint_text("Search presets")
            .desired_width(f32::INFINITY);
        ui.add(field);
        // By device, the devices in order of name.
        let mut by_device: BTreeMap<String, (&str, Vec<&Preset>)> = BTreeMap::new();
        let mut missing = 0;
        for preset in library.presets {
            let Some(device) = device_of(preset, view.plugins.installed) else {
                missing += 1;
                continue;
            };
            if score(&self.search, &format!("{} {device}", preset.name), "", &[]).is_none() {
                continue;
            }
            let group = by_device.entry(device.to_lowercase());
            group.or_insert((device, Vec::new())).1.push(preset);
        }
        let to = devices::chain_name(view.project, chain);
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.horizontal(|ui| {
                if library.finding_presets {
                    ui.spinner();
                }
                let shown = library.presets.len() - missing;
                let mut text = count(shown, "preset");
                if missing > 0 {
                    text.push_str(&format!(", {missing} for plugins not installed"));
                }
                ui.add(Label::new(RichText::new(text).weak()).truncate());
            });
            ui.separator();
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ScrollArea::vertical()
                    .id_salt("presets")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        if by_device.is_empty() {
                            let text = match (library.presets.len() - missing, self.search.trim()) {
                                (0, _) if library.finding_presets => "Looking for presets...",
                                (0, _) => {
                                    "No presets yet. Right-click a device's name and pick Save \
                                     as Preset to keep its settings here."
                                }
                                _ => "No presets match.",
                            };
                            ui.add(Label::new(RichText::new(text).weak()).wrap());
                        }
                        for (device, presets) in by_device.values() {
                            ui.label(RichText::new(*device).strong());
                            for preset in presets {
                                preset_row(ui, preset, chain, &to, theme, actions);
                            }
                        }
                    });
            });
        });
    }
}

/// A preset's row, which adds the preset to `chain` when double-clicked,
/// and drags.
fn preset_row(
    ui: &mut Ui,
    preset: &Preset,
    chain: Option<Chain>,
    to: &str,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    let response = row(ui, theme, 1, None, &preset.name, "", false);
    response.dnd_set_drag_payload(Dragged::Preset {
        path: preset.path.clone(),
        name: preset.name.clone(),
    });
    if response.double_clicked() {
        let args = onto(preset_args(&preset.path), chain);
        actions.push(Action::RunWith(device::ADD_PRESET, args));
    }
    response.on_hover_text(format!(
        "{}\nDouble-click to add it to {to}, or drag it onto a track.",
        preset.path.display()
    ));
}

/// The name of the device `preset` is for, if Musicable has the device: a
/// plugin's only if it's installed.
fn device_of<'a>(preset: &'a Preset, installed: &'a [PluginEntry]) -> Option<&'a str> {
    let plugin = |format: PluginFormat, id: &str| {
        let found = installed.iter().find(|plugin| {
            plugin.format == format
                && match format {
                    // A VST3 preset gives the class id in capitals.
                    PluginFormat::Vst3 => plugin.id.eq_ignore_ascii_case(id),
                    PluginFormat::Clap => plugin.id == id,
                }
        });
        found.map(|plugin| plugin.name.as_str())
    };
    match &preset.kind {
        PresetKind::Device { name } => Some(name),
        PresetKind::Plugin { format, id, .. } => plugin(*format, id),
        PresetKind::Vst3 { class_id } => plugin(PluginFormat::Vst3, class_id),
    }
}

/// The arguments that add the preset at `path`.
fn preset_args(path: &Path) -> Args {
    Args::new().with("preset", path.to_string_lossy().as_ref())
}

/// A row of the browser's lists, `depth` steps in: an arrow first for a
/// folder, pointing down if it's open, then `name`, and `detail` after it,
/// weaker. It senses clicks and drags.
fn row(
    ui: &mut Ui,
    theme: &Theme,
    depth: usize,
    folder: Option<bool>,
    name: &str,
    detail: &str,
    selected: bool,
) -> Response {
    let size = vec2(ui.available_width(), row_height(ui));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let role = match folder {
        Some(_) => Role::TreeItem,
        None => Role::ListItem,
    };
    response.widget_info(|| WidgetInfo::selected(role, true, selected, name));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() {
        painter.rect_filled(rect, theme.corner, visuals.weak_bg_fill);
    }
    let color = visuals.text_color();
    let indent = theme.browser.indent;
    let left = rect.left() + theme.padding + depth as f32 * indent;
    let y = rect.center().y;
    if let Some(open) = folder {
        let (x, r) = (left + indent / 2.0, indent / 4.0);
        let points = match open {
            true => vec![
                pos2(x - r, y - r / 2.0),
                pos2(x + r, y - r / 2.0),
                pos2(x, y + r),
            ],
            false => vec![
                pos2(x - r / 2.0, y - r),
                pos2(x + r, y),
                pos2(x - r / 2.0, y + r),
            ],
        };
        painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    }
    let x = left + indent;
    let font = FontId::proportional(theme.text);
    let name = painter.layout_no_wrap(name.to_owned(), font, color);
    let width = name.size().x;
    painter.galley(pos2(x, y - name.size().y / 2.0), name, color);
    if !detail.is_empty() {
        let weak = ui.visuals().weak_text_color();
        let at = pos2(x + width + theme.padding * 2.0, y);
        let font = FontId::proportional(theme.small_text);
        painter.text(at, Align2::LEFT_CENTER, detail, font, weak);
    }
    response
}

fn row_height(ui: &Ui) -> f32 {
    ui.spacing().interact_size.y
}

fn preview(path: &Path) -> Action {
    let args = Args::new().with("file", path.to_string_lossy().as_ref());
    Action::RunWith(browser::PREVIEW, args)
}

/// Puts the file at `path` on the selected track, as Insert Audio does.
fn insert(path: &Path) -> Action {
    Action::DropFiles {
        files: vec![path.to_owned()],
        track: None,
        at: None,
    }
}

/// What a file's tooltip says: where it is, what kind of audio it is, how
/// long it is, and its tags.
fn describe(record: &Record, path: &Path) -> String {
    let mut text = path.display().to_string();
    if let Some(audio) = &record.audio {
        let channels = match audio.channels {
            1 => "mono".to_owned(),
            2 => "stereo".to_owned(),
            n => format!("{n} channels"),
        };
        text.push_str(&format!(
            "\n{} · {} kHz · {channels} · {:.2} s",
            audio.format.name(),
            f64::from(audio.sample_rate) / 1000.0,
            audio.seconds()
        ));
    }
    if !record.tags.is_empty() {
        let tags: Vec<&str> = record.tags.iter().map(|tag| tag.id.as_str()).collect();
        text.push_str(&format!("\nTags: {}", tags.join(", ")));
    }
    text.push_str("\nDouble-click to put it on the selected track, or drag it onto one.");
    text
}

/// A file's name without its extension.
fn stem(path: &Path) -> String {
    let stem = path.file_stem().unwrap_or(path.as_os_str());
    stem.to_string_lossy().into_owned()
}

/// `n` with commas between the thousands: 12,345.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut text = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// `n` of `what`: "1 sample", "12,345 samples".
fn count(n: usize, what: &str) -> String {
    match n {
        1 => format!("1 {what}"),
        n => format!("{} {what}s", thousands(n)),
    }
}

#[cfg(test)]
mod tests;
