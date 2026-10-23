//! The track commands: new audio, MIDI, return and group tracks, and
//! naming and deleting tracks.

use musicable_model::{TrackId, TrackKind};

use crate::{
    Command, CommandId, Context, Document, Param, Registry, Transaction, Type, Value, When,
};

/// Adds an audio track.
pub const NEW_AUDIO_TRACK: CommandId = CommandId::new("track.new_audio_track");
/// Adds a MIDI track.
pub const NEW_MIDI_TRACK: CommandId = CommandId::new("track.new_midi_track");
/// Adds a return track, for sends to feed.
pub const NEW_RETURN_TRACK: CommandId = CommandId::new("track.new_return_track");
/// Adds a group track, for tracks to feed.
pub const NEW_GROUP_TRACK: CommandId = CommandId::new("track.new_group_track");
/// Names a track.
pub const RENAME: CommandId = CommandId::new("track.rename");
/// Deletes tracks: the selected ones, without arguments.
pub const DELETE: CommandId = CommandId::new("track.delete");

type Handler = fn(&mut Context<'_>);

/// Every track command, with its title and what it runs.
const COMMANDS: [(CommandId, &str, Handler); 4] = [
    (NEW_AUDIO_TRACK, "New Audio Track", |ctx| {
        new_track(ctx, TrackKind::Audio)
    }),
    (NEW_MIDI_TRACK, "New MIDI Track", |ctx| {
        new_track(ctx, TrackKind::Midi)
    }),
    (NEW_RETURN_TRACK, "New Return Track", |ctx| {
        new_track(ctx, TrackKind::Return)
    }),
    (NEW_GROUP_TRACK, "New Group Track", |ctx| {
        new_track(ctx, TrackKind::Group)
    }),
];

/// Registers the track commands.
pub fn register(registry: &mut Registry) {
    for (id, title, run) in COMMANDS {
        registry.register(id, title, run);
    }
    registry.add(
        Command::new(RENAME, "Rename Track").with_params(&[
            Param::required("track", Type::Track, "The track to name."),
            Param::required("name", Type::Text, "Its new name."),
        ]),
        |ctx, args| {
            let track: TrackId = args.get("track")?;
            let name = crate::locator::valid_name(&args.get::<String>("name")?)?;
            ctx.document
                .transact("Rename Track", |tx| tx.set_track_name(track, &name));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(DELETE, "Delete Tracks")
            .when(When::TracksSelected)
            .with_params(&[Param::optional(
                "tracks",
                Type::TRACKS,
                "The tracks to delete: the selected ones without it.",
            )]),
        |ctx, args| {
            let tracks: Vec<TrackId> = match args.opt("tracks")? {
                Some(tracks) => tracks,
                None => ctx
                    .document
                    .editor()
                    .selection
                    .tracks
                    .iter()
                    .copied()
                    .collect(),
            };
            delete(ctx.document, &tracks);
            Ok(Value::None)
        },
    );
}

/// Deletes the tracks with ids `tracks`, as one transaction. Tracks feeding
/// them feed the master instead, and sends to them go.
pub fn delete(document: &mut Document, tracks: &[TrackId]) {
    let name = match tracks {
        [_] => "Delete Track",
        _ => "Delete Tracks",
    };
    document.transact(name, |tx| {
        for &track in tracks {
            tx.remove_track(track);
        }
    });
}

/// Adds a track of `kind`, named after its kind and how many of them there
/// are, such as "Audio 3".
fn new_track(ctx: &mut Context<'_>, kind: TrackKind) {
    let (title, word) = match kind {
        TrackKind::Audio => ("New Audio Track", "Audio"),
        TrackKind::Midi => ("New MIDI Track", "MIDI"),
        TrackKind::Return => ("New Return Track", "Return"),
        TrackKind::Group => ("New Group Track", "Group"),
    };
    ctx.document.transact(title, |tx| {
        let count = tx
            .project()
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .count();
        add_track(tx, format!("{word} {}", count + 1), kind);
    });
}

/// Adds a track of `kind` called `name`, and returns its id. Return tracks
/// go at the end of the list, and the others before the first return track,
/// so the returns stay together.
pub(crate) fn add_track(tx: &mut Transaction<'_>, name: String, kind: TrackKind) -> TrackId {
    let tracks = &tx.project().tracks;
    let index = match kind {
        TrackKind::Return => tracks.len(),
        _ => tracks
            .iter()
            .position(|t| t.kind == TrackKind::Return)
            .unwrap_or(tracks.len()),
    };
    let track = tx.new_track(name, kind);
    let id = track.id;
    tx.add_track(index, track);
    id
}

#[cfg(test)]
mod tests {
    use musicable_model::Project;

    use super::*;
    use crate::testing::{Setup, StubTransport};
    use crate::{Args, CommandError, Edit};

    fn run(document: &mut Document, id: &CommandId) {
        let mut registry = Registry::new();
        register(&mut registry);
        let mut context = Context {
            document,
            transport: &mut StubTransport::default(),
            requests: &mut Vec::new(),
            library: &mut crate::NoLibrary,
        };
        registry.run(id, &mut context).unwrap();
    }

    #[test]
    fn new_tracks_go_before_the_returns() {
        let mut doc = Document::new(Project::new());
        for id in [
            NEW_AUDIO_TRACK,
            NEW_RETURN_TRACK,
            NEW_MIDI_TRACK,
            NEW_AUDIO_TRACK,
            NEW_RETURN_TRACK,
            NEW_GROUP_TRACK,
        ] {
            run(&mut doc, &id);
        }
        let names: Vec<&str> = doc
            .project()
            .tracks
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "Audio 1", "MIDI 1", "Audio 2", "Group 1", "Return 1", "Return 2"
            ]
        );
        let committed = doc.take_committed();
        assert_eq!(committed.len(), 6);
        assert_eq!(committed[1].name, "New Return Track");
        assert!(matches!(
            &committed[2].edits[..],
            [Edit::AddTrack { index: 1, track }] if track.kind == TrackKind::Midi
        ));
        // Return tracks start solo-safe.
        assert!(doc.project().tracks[5].solo_safe);
    }

    #[test]
    fn tracks_are_named_and_deleted() {
        let mut project = Project::new();
        let bass = project.add_track("Bass", TrackKind::Midi);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let vox = project.add_track("Vox", TrackKind::Audio);
        let mut setup = Setup::new(project);
        let name = |name: &str| Args::new().with("track", keys).with("name", name);
        setup.run_with(&RENAME, name(" Pad ")).unwrap();
        assert_eq!(
            setup.run_with(&RENAME, name("")),
            Err(CommandError::invalid("a name can't be empty"))
        );
        assert_eq!(setup.document.project().tracks[1].name, "Pad");

        assert!(!setup.applies(&DELETE));
        setup.select_tracks(&[bass, vox]);
        setup.run(&DELETE);
        let args = Args::new().with("tracks", vec![keys]);
        setup.run_with(&DELETE, args).unwrap();
        assert!(setup.document.project().tracks.is_empty());
        assert_eq!(
            setup.committed_names(),
            ["Rename Track", "Delete Tracks", "Delete Track"]
        );
    }
}
