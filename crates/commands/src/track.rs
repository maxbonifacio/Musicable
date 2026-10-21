//! The track commands: new audio, MIDI, return and group tracks.

use musicable_model::TrackKind;

use crate::{CommandId, Context, Registry};

/// Adds an audio track.
pub const NEW_AUDIO_TRACK: CommandId = CommandId::new("track.new_audio_track");
/// Adds a MIDI track.
pub const NEW_MIDI_TRACK: CommandId = CommandId::new("track.new_midi_track");
/// Adds a return track, for sends to feed.
pub const NEW_RETURN_TRACK: CommandId = CommandId::new("track.new_return_track");
/// Adds a group track, for tracks to feed.
pub const NEW_GROUP_TRACK: CommandId = CommandId::new("track.new_group_track");

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
}

/// Adds a track of `kind`, named after its kind and how many of them there
/// are, such as "Audio 3". Return tracks go at the end of the list, and the
/// others before the first return track, so the returns stay together.
fn new_track(ctx: &mut Context<'_>, kind: TrackKind) {
    let (title, word) = match kind {
        TrackKind::Audio => ("New Audio Track", "Audio"),
        TrackKind::Midi => ("New MIDI Track", "MIDI"),
        TrackKind::Return => ("New Return Track", "Return"),
        TrackKind::Group => ("New Group Track", "Group"),
    };
    ctx.document.transact(title, |tx| {
        let tracks = &tx.project().tracks;
        let count = tracks.iter().filter(|t| t.kind == kind).count();
        let index = match kind {
            TrackKind::Return => tracks.len(),
            _ => tracks
                .iter()
                .position(|t| t.kind == TrackKind::Return)
                .unwrap_or(tracks.len()),
        };
        let track = tx.new_track(format!("{word} {}", count + 1), kind);
        tx.add_track(index, track);
    });
}

#[cfg(test)]
mod tests {
    use musicable_model::Project;
    use musicable_model::time::Beats;

    use super::*;
    use crate::{Document, Edit, Transport};

    struct Stopped;

    impl Transport for Stopped {
        fn is_playing(&self) -> bool {
            false
        }
        fn play(&mut self) {}
        fn stop(&mut self) {}
        fn locate(&mut self, _: Beats) {}
    }

    fn run(document: &mut Document, id: &CommandId) {
        let mut registry = Registry::new();
        register(&mut registry);
        let mut context = Context {
            document,
            transport: &mut Stopped,
            requests: &mut Vec::new(),
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
}
