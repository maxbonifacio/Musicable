//! The mixer's rules: which buses a track can feed, the pan law, and what
//! mute and solo silence.
//!
//! Signal flows from tracks to buses without loops. A track's output feeds
//! the master or a group track, or with Sends Only, nothing, and its sends
//! feed return tracks. Return tracks feed the master and have no sends, so
//! sends can't make a loop, and a group can't feed itself through other
//! groups.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Chain, Project, Send, SendId, Track, TrackId, TrackKind};

/// Where a track's output goes: a bus it feeds, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum Bus {
    #[default]
    Master,
    /// A group track's bus.
    Group(TrackId),
    /// No bus: the track is heard only through its sends, and through what
    /// other tracks take from it, such as a ghost kick that keys a bass.
    SendsOnly,
}

/// The gains the balance control applies to the left and right channels,
/// for `pan` from -1.0 (left) to 1.0 (right).
///
/// The channel on the side the control turns towards stays at unity, and the
/// other falls in a straight line to silence at the end of the control's
/// travel: halfway, it's at half (-6 dB). At the centre both are exactly
/// 1.0, so a centred track passes unchanged. A balance control never boosts,
/// so panning can't push a channel over full scale.
pub fn pan_gains(pan: f32) -> [f32; 2] {
    let pan = if pan.is_nan() {
        0.0
    } else {
        pan.clamp(-1.0, 1.0)
    };
    if pan >= 0.0 {
        [1.0 - pan, 1.0]
    } else {
        [1.0, 1.0 + pan]
    }
}

/// The track outputs and sends that mute and solo cut. See
/// [`Project::silenced`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Silenced {
    /// Tracks whose output to their bus is cut.
    pub outputs: BTreeSet<TrackId>,
    /// Sends that are cut.
    pub sends: BTreeSet<SendId>,
}

impl Silenced {
    pub fn output(&self, track: TrackId) -> bool {
        self.outputs.contains(&track)
    }

    pub fn send(&self, send: SendId) -> bool {
        self.sends.contains(&send)
    }
}

impl Project {
    /// Whether the track with id `track` can feed `bus`: the master, or a
    /// group track that doesn't feed `track` itself, directly or through
    /// other groups. Return tracks only feed the master. Any track can feed
    /// nothing, with Sends Only.
    pub fn can_feed(&self, track: TrackId, bus: Bus) -> bool {
        let Some(from) = self.track(track) else {
            return false;
        };
        match bus {
            Bus::Master | Bus::SendsOnly => true,
            Bus::Group(group) => {
                from.kind != TrackKind::Return
                    && self
                        .track(group)
                        .is_some_and(|g| g.kind == TrackKind::Group)
                    && !self.feeds(group, track)
            }
        }
    }

    /// Where the track with id `track` can send its output: the master,
    /// then the groups it can feed, in track order, then Sends Only.
    pub fn buses_for(&self, track: TrackId) -> Vec<Bus> {
        let groups = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Group)
            .map(|t| Bus::Group(t.id))
            .filter(|&bus| self.can_feed(track, bus));
        (std::iter::once(Bus::Master).chain(groups))
            .chain([Bus::SendsOnly])
            .collect()
    }

    /// Whether the track with id `from` can send to the track with id `to`:
    /// a return track it has no send to yet. Return tracks have no sends.
    pub fn can_send(&self, from: TrackId, to: TrackId) -> bool {
        let (Some(source), Some(target)) = (self.track(from), self.track(to)) else {
            return false;
        };
        source.kind != TrackKind::Return
            && target.kind == TrackKind::Return
            && source.sends.iter().all(|send| send.to != to)
    }

    /// Where `track`'s output really goes. It's [`Track::output`] where
    /// [`can_feed`](Self::can_feed) allows it, and the master where a
    /// project breaks the rules, so the signal never loops.
    pub fn output_of(&self, track: &Track) -> Bus {
        match track.output {
            Bus::Group(group) if self.can_feed(track.id, Bus::Group(group)) => track.output,
            Bus::SendsOnly => Bus::SendsOnly,
            _ => Bus::Master,
        }
    }

    /// The sends of `track` that really feed a return track. Sends to
    /// anything else, and a return track's sends, are left out.
    pub fn sends_of<'a>(&'a self, track: &'a Track) -> impl Iterator<Item = &'a Send> + 'a {
        let sends = match track.kind {
            TrackKind::Return => &[][..],
            _ => &track.sends[..],
        };
        sends.iter().filter(|send| {
            self.track(send.to)
                .is_some_and(|t| t.kind == TrackKind::Return)
        })
    }

    /// Whether the track with id `from` feeds the track with id `to` through
    /// group outputs, following them as the tracks set them. A track feeds
    /// itself.
    fn feeds(&self, from: TrackId, to: TrackId) -> bool {
        let mut seen = BTreeSet::new();
        let mut at = from;
        loop {
            if at == to {
                return true;
            }
            // A loop that doesn't pass through `to`.
            if !seen.insert(at) {
                return false;
            }
            match self.track(at).map(|t| t.output) {
                Some(Bus::Group(next)) => at = next,
                _ => return false,
            }
        }
    }

    /// The track outputs and sends that mute and solo cut.
    ///
    /// A muted track's output and all its sends, pre-fader ones included,
    /// are cut. What other tracks take from it at its taps carries on, as an
    /// input or a key. While no track is soloed, that's all. While any track
    /// is, only the signal paths through a soloed track are heard:
    ///
    /// - A soloed group solos every track that feeds it.
    /// - A soloed track's output and sends pass, and so does everything
    ///   downstream of it: its group, the returns it sends to, the tracks
    ///   that take their input from it, and theirs.
    /// - Everything that feeds a soloed track passes into it. Soloing a
    ///   return plays what the sends put into it, without the tracks' own
    ///   outputs: the wet signal alone.
    /// - A solo-safe track passes, with whatever is downstream of it, as if
    ///   it were soloed, but it doesn't start soloing. Tracks feeding it are
    ///   cut as usual.
    ///
    /// A track that only keys another's device, through a wire, isn't
    /// downstream of it: soloing the keyed track leaves it cut.
    pub fn silenced(&self) -> Silenced {
        let tracks = &self.tracks;
        let index: BTreeMap<TrackId, usize> =
            tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        // Every track's output and sends, as edges between track indexes.
        // `None` is the master, or with Sends Only, nothing.
        let outputs: Vec<Option<usize>> = tracks
            .iter()
            .map(|t| match self.output_of(t) {
                Bus::Master | Bus::SendsOnly => None,
                Bus::Group(group) => index.get(&group).copied(),
            })
            .collect();
        let sends: Vec<Vec<(SendId, usize)>> = tracks
            .iter()
            .map(|t| {
                self.sends_of(t)
                    .filter_map(|s| Some((s.id, *index.get(&s.to)?)))
                    .collect()
            })
            .collect();
        // The tracks taking their input from each track's taps. Taps come
        // before mute and solo, so these edges are never cut.
        let mut takers = vec![Vec::new(); tracks.len()];
        for (track, tap) in self.live_inputs() {
            let from = self.tap_chain(tap).and_then(Chain::track);
            if let (Some(from), Some(&to)) = (from.and_then(|t| index.get(&t)), index.get(&track)) {
                takers[*from].push(to);
            }
        }

        let mut silenced = Silenced::default();
        let soloing = tracks.iter().any(|t| t.solo);
        // Tracks soloed directly, or through a group they feed.
        let soloed: Vec<bool> = (0..tracks.len())
            .map(|i| {
                let mut at = Some(i);
                while let Some(track) = at {
                    if tracks[track].solo {
                        return true;
                    }
                    at = outputs[track];
                }
                false
            })
            .collect();
        let next = |i: usize| {
            (outputs[i].into_iter())
                .chain(sends[i].iter().map(|&(_, to)| to))
                .chain(takers[i].iter().copied())
        };
        // Downstream of a soloed or solo-safe track.
        let upstream_open = reach(
            tracks.len(),
            |i| soloed[i] || tracks[i].solo_safe,
            |i, out| out.extend(next(i)),
        );
        // Feeding a soloed track: upstream of it, following edges back.
        let mut feeders = vec![Vec::new(); tracks.len()];
        for i in 0..tracks.len() {
            for to in next(i) {
                feeders[to].push(i);
            }
        }
        let downstream_open = reach(
            tracks.len(),
            |i| soloed[i],
            |i, out| out.extend(&feeders[i]),
        );
        // Whether an edge from track `i` into bus `to` passes.
        let passes = |i: usize, to: Option<usize>| {
            !tracks[i].muted
                && (!soloing || upstream_open[i] || to.is_some_and(|to| downstream_open[to]))
        };
        for (i, track) in tracks.iter().enumerate() {
            if !passes(i, outputs[i]) {
                silenced.outputs.insert(track.id);
            }
            for &(send, to) in &sends[i] {
                if !passes(i, Some(to)) {
                    silenced.sends.insert(send);
                }
            }
            // Sends that feed nothing are silent anyway; cut them too, so a
            // project's mute state is the same whichever way it's read.
            for send in &track.sends {
                if !sends[i].iter().any(|&(id, _)| id == send.id) && track.muted {
                    silenced.sends.insert(send.id);
                }
            }
        }
        silenced
    }
}

/// The nodes of a graph of `len` nodes reachable from those where `start`
/// holds, following `next`.
fn reach(
    len: usize,
    start: impl Fn(usize) -> bool,
    next: impl Fn(usize, &mut Vec<usize>),
) -> Vec<bool> {
    let mut reached = vec![false; len];
    let mut stack: Vec<usize> = (0..len).filter(|&i| start(i)).collect();
    for &i in &stack {
        reached[i] = true;
    }
    let mut found = Vec::new();
    while let Some(i) = stack.pop() {
        found.clear();
        next(i, &mut found);
        for &j in &found {
            if !std::mem::replace(&mut reached[j], true) {
                stack.push(j);
            }
        }
    }
    reached
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TrackKind::{Audio, Group, Midi, Return};
    use crate::native::{NativeDevice, NativeKind};
    use crate::routing::{Port, Tap};
    use crate::{DeviceKind, Input};

    /// Track names to ids.
    struct Mix {
        project: Project,
    }

    impl Mix {
        fn new() -> Self {
            Self {
                project: Project::new(),
            }
        }

        fn track(&mut self, name: &str, kind: TrackKind) -> TrackId {
            self.project.add_track(name, kind)
        }

        fn id(&self, name: &str) -> TrackId {
            self.project
                .tracks
                .iter()
                .find(|t| t.name == name)
                .unwrap()
                .id
        }

        fn route(&mut self, name: &str, group: &str) {
            let group = self.id(group);
            let id = self.id(name);
            self.project.track_mut(id).unwrap().output = Bus::Group(group);
        }

        fn send(&mut self, name: &str, to: &str) -> SendId {
            let to = self.id(to);
            let mut send = self.project.new_send(to);
            send.level = 0.5;
            let id = self.id(name);
            self.project.track_mut(id).unwrap().sends.push(send);
            send.id
        }

        fn set(&mut self, name: &str, change: impl FnOnce(&mut Track)) {
            let id = self.id(name);
            change(self.project.track_mut(id).unwrap());
        }

        /// The names of the tracks whose outputs pass, and of the tracks and
        /// returns of the sends that pass, as `track>return`.
        fn heard(&self) -> Vec<String> {
            let silenced = self.project.silenced();
            let mut heard = Vec::new();
            for track in &self.project.tracks {
                if !silenced.output(track.id) {
                    heard.push(track.name.clone());
                }
                for send in &track.sends {
                    if !silenced.send(send.id) {
                        let to = &self.project.track(send.to).unwrap().name;
                        heard.push(format!("{}>{to}", track.name));
                    }
                }
            }
            heard
        }
    }

    /// Drums and bass in a group, keys on their own, a vocal in a group of
    /// its own, every one of them sending to a reverb, and the drums also to
    /// a delay.
    fn mix() -> Mix {
        let mut mix = Mix::new();
        mix.track("Drums", Audio);
        mix.track("Bass", Midi);
        mix.track("Keys", Midi);
        mix.track("Vocal", Audio);
        mix.track("Band", Group);
        mix.track("Vox", Group);
        mix.track("Reverb", Return);
        mix.track("Delay", Return);
        mix.route("Drums", "Band");
        mix.route("Bass", "Band");
        mix.route("Vocal", "Vox");
        for name in ["Drums", "Bass", "Keys", "Vocal"] {
            mix.send(name, "Reverb");
        }
        mix.send("Drums", "Delay");
        mix
    }

    const EVERYTHING: [&str; 13] = [
        "Drums",
        "Drums>Reverb",
        "Drums>Delay",
        "Bass",
        "Bass>Reverb",
        "Keys",
        "Keys>Reverb",
        "Vocal",
        "Vocal>Reverb",
        "Band",
        "Vox",
        "Reverb",
        "Delay",
    ];

    #[test]
    fn without_solo_everything_but_muted_tracks_is_heard() {
        let mut mix = mix();
        assert_eq!(mix.heard(), EVERYTHING);
        mix.set("Drums", |t| t.muted = true);
        mix.set("Vox", |t| t.muted = true);
        let mut expected = EVERYTHING.to_vec();
        // A muted track's pre-fader sends are cut too.
        mix.project.tracks[0].sends[0].pre_fader = true;
        expected.retain(|h| !h.starts_with("Drums") && *h != "Vox");
        assert_eq!(mix.heard(), expected);
    }

    #[test]
    fn a_soloed_track_is_heard_through_its_group_and_returns() {
        let mut mix = mix();
        mix.set("Drums", |t| t.solo = true);
        assert_eq!(
            mix.heard(),
            [
                "Drums",
                "Drums>Reverb",
                "Drums>Delay",
                "Band",
                "Reverb",
                "Delay"
            ]
        );
    }

    #[test]
    fn a_soloed_group_solos_its_tracks() {
        let mut mix = mix();
        mix.set("Band", |t| t.solo = true);
        assert_eq!(
            mix.heard(),
            [
                "Drums",
                "Drums>Reverb",
                "Drums>Delay",
                "Bass",
                "Bass>Reverb",
                "Band",
                "Reverb",
                "Delay"
            ]
        );
    }

    #[test]
    fn a_soloed_return_plays_only_what_the_sends_put_in() {
        let mut mix = mix();
        mix.set("Reverb", |t| t.solo = true);
        // Nothing reaches the master but the returns: the reverb, and the
        // delay, which is solo-safe but gets nothing.
        assert_eq!(
            mix.heard(),
            [
                "Drums>Reverb",
                "Bass>Reverb",
                "Keys>Reverb",
                "Vocal>Reverb",
                "Reverb",
                "Delay"
            ]
        );
    }

    #[test]
    fn solo_safe_tracks_keep_playing_without_what_feeds_them() {
        let mut mix = mix();
        mix.set("Keys", |t| t.solo = true);
        mix.set("Reverb", |t| t.solo_safe = false);
        // The delay is still solo-safe, with nothing sent to it.
        assert_eq!(mix.heard(), ["Keys", "Keys>Reverb", "Reverb", "Delay"]);

        // A solo-safe track in a group is heard through the group, and the
        // group's other tracks are cut.
        mix.set("Bass", |t| t.solo_safe = true);
        assert_eq!(
            mix.heard(),
            [
                "Bass",
                "Bass>Reverb",
                "Keys",
                "Keys>Reverb",
                "Band",
                "Reverb",
                "Delay"
            ]
        );
    }

    #[test]
    fn mute_wins_over_solo() {
        let mut mix = mix();
        mix.set("Vocal", |t| {
            t.solo = true;
            t.muted = true;
        });
        assert_eq!(mix.heard(), ["Vox", "Reverb", "Delay"]);
        // Muting the group a soloed track is in silences the track too.
        mix.set("Vocal", |t| t.muted = false);
        mix.set("Vox", |t| t.muted = true);
        assert_eq!(mix.heard(), ["Vocal", "Vocal>Reverb", "Reverb", "Delay"]);
    }

    #[test]
    fn groups_can_nest_but_not_loop() {
        let mut mix = mix();
        let (band, vox, drums) = (mix.id("Band"), mix.id("Vox"), mix.id("Drums"));
        let p = &mix.project;
        assert!(p.can_feed(vox, Bus::Group(band)));
        assert!(!p.can_feed(band, Bus::Group(band)), "into itself");
        assert!(!p.can_feed(drums, Bus::Group(drums)), "not a group");
        assert!(!p.can_feed(mix.id("Reverb"), Bus::Group(band)), "a return");
        assert!(!p.can_feed(TrackId(99), Bus::Master), "no such track");
        assert_eq!(
            p.buses_for(drums),
            [
                Bus::Master,
                Bus::Group(band),
                Bus::Group(vox),
                Bus::SendsOnly
            ]
        );

        mix.route("Vox", "Band");
        let p = &mix.project;
        assert!(!p.can_feed(band, Bus::Group(vox)), "a loop");
        assert_eq!(p.buses_for(band), [Bus::Master, Bus::SendsOnly]);
        // Soloing the outer group solos the inner one and its tracks.
        mix.set("Band", |t| t.solo = true);
        assert!(!mix.heard().contains(&"Keys".to_owned()));
        assert!(mix.heard().contains(&"Vocal".to_owned()));
    }

    #[test]
    fn broken_routing_feeds_the_master() {
        let mut mix = mix();
        let (band, vox, keys) = (mix.id("Band"), mix.id("Vox"), mix.id("Keys"));
        // A loop, a track that isn't a group, a missing one, and a return
        // feeding a group.
        mix.route("Band", "Vox");
        mix.route("Vox", "Band");
        mix.set("Bass", |t| t.output = Bus::Group(keys));
        mix.set("Keys", |t| t.output = Bus::Group(TrackId(99)));
        mix.route("Reverb", "Band");
        let p = &mix.project;
        for name in ["Band", "Vox", "Bass", "Keys", "Reverb"] {
            let track = p.track(mix.id(name)).unwrap();
            assert_eq!(p.output_of(track), Bus::Master, "{name}");
        }
        // Tracks feeding into a loop still feed the group they name.
        let drums = p.track(mix.id("Drums")).unwrap();
        assert_eq!(p.output_of(drums), Bus::Group(band));
        let vocal = p.track(mix.id("Vocal")).unwrap();
        assert_eq!(p.output_of(vocal), Bus::Group(vox));
    }

    #[test]
    fn sends_only_feed_returns() {
        let mut mix = mix();
        let (keys, reverb, band) = (mix.id("Keys"), mix.id("Reverb"), mix.id("Band"));
        let p = &mix.project;
        assert!(!p.can_send(keys, reverb), "it already sends there");
        assert!(p.can_send(band, reverb));
        assert!(!p.can_send(keys, band), "not a return");
        assert!(!p.can_send(reverb, mix.id("Delay")), "returns don't send");
        // Sends to the wrong place are left out, and cut if the track is
        // muted.
        let stray = mix.send("Keys", "Band");
        let keys_track = mix.project.track(keys).unwrap();
        let fed: Vec<_> = mix.project.sends_of(keys_track).map(|s| s.to).collect();
        assert_eq!(fed, [reverb]);
        assert!(!mix.project.silenced().send(stray));
        mix.set("Keys", |t| t.muted = true);
        assert!(mix.project.silenced().send(stray));
    }

    #[test]
    fn a_sends_only_track_is_heard_through_its_sends() {
        let mut mix = mix();
        let keys = mix.id("Keys");
        assert!(mix.project.can_feed(mix.id("Reverb"), Bus::SendsOnly));
        mix.set("Keys", |t| t.output = Bus::SendsOnly);
        let track = mix.project.track(keys).unwrap();
        assert_eq!(mix.project.output_of(track), Bus::SendsOnly);
        // Its output stage passes, feeding nothing, and solo treats it as
        // any other track.
        assert_eq!(mix.heard(), EVERYTHING);
        mix.set("Keys", |t| t.solo = true);
        assert_eq!(mix.heard(), ["Keys", "Keys>Reverb", "Reverb", "Delay"]);
    }

    #[test]
    fn tracks_taking_a_tracks_sound_follow_its_solo_but_not_its_mute() {
        let mut mix = mix();
        let print = mix.track("Print", Audio);
        let band = Chain::Track(mix.id("Band"));
        mix.set("Print", |t| {
            t.input = Some(Input::Tap(Tap::PostFader(band)))
        });
        // What the print takes comes before the band's mute.
        mix.set("Band", |t| t.muted = true);
        assert!(mix.heard().contains(&"Print".to_owned()));
        mix.set("Band", |t| t.muted = false);
        // Soloing the drums lets the print through, downstream of them.
        mix.set("Drums", |t| t.solo = true);
        assert_eq!(
            mix.heard(),
            [
                "Drums",
                "Drums>Reverb",
                "Drums>Delay",
                "Band",
                "Reverb",
                "Delay",
                "Print"
            ]
        );
        // Soloing the print plays what feeds it, into the band, but not the
        // band itself.
        mix.set("Drums", |t| t.solo = false);
        mix.set("Print", |t| t.solo = true);
        assert_eq!(mix.heard(), ["Drums", "Bass", "Reverb", "Delay", "Print"]);
        // Resampling takes no track's sound.
        mix.set("Print", |t| t.input = Some(Input::RESAMPLING));
        assert_eq!(mix.heard(), ["Reverb", "Delay", "Print"]);
        assert_eq!(
            mix.project.live_inputs(),
            [(print, Tap::PostFader(Chain::Master))]
        );
    }

    #[test]
    fn a_track_that_only_keys_another_stays_cut_while_that_one_is_soloed() {
        let mut mix = mix();
        let compressor = mix.project.new_device(DeviceKind::Native(NativeDevice::new(
            NativeKind::Compressor,
        )));
        let port = Port::Sidechain(compressor.id);
        mix.set("Vocal", |t| t.devices.push(compressor));
        let from = Tap::PostFx(Chain::Track(mix.id("Drums")));
        let wire = mix.project.new_wire(from, port);
        mix.project.wires.push(wire);
        mix.set("Vocal", |t| t.solo = true);
        assert!(!mix.heard().contains(&"Drums".to_owned()));
    }

    #[test]
    fn the_balance_law() {
        assert_eq!(pan_gains(0.0), [1.0, 1.0]);
        assert_eq!(pan_gains(-0.0), [1.0, 1.0]);
        assert_eq!(pan_gains(0.5), [0.5, 1.0]);
        assert_eq!(pan_gains(-0.25), [1.0, 0.75]);
        assert_eq!(pan_gains(1.0), [0.0, 1.0]);
        assert_eq!(pan_gains(-1.0), [1.0, 0.0]);
        assert_eq!(pan_gains(7.0), [0.0, 1.0]);
        assert_eq!(pan_gains(f32::NAN), [1.0, 1.0]);
    }
}
