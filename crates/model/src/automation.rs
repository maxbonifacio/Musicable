//! Automation: lanes of points that a parameter follows as the song plays,
//! in place of its own value.
//!
//! A [`Lane`] belongs to one parameter, by its [`ParamAddress`], and the
//! project keeps one for each automated parameter
//! ([`Project::automation`]). Between two points the value moves in a
//! straight line in the parameter's [`Scale`]; two points at the same time
//! make a step. Moving the parameter by hand overrides its lane, which
//! stays as it is until automation is re-enabled.

use crate::Project;
use crate::param::{ParamAddress, Scale};
use crate::time::Beats;

/// A point on a lane: the parameter's value at a place in the song.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub at: Beats,
    /// In the parameter's own units, as its value is: linear gain for a
    /// fader or a send, -1.0 to 1.0 for a pan, a plugin's own values for
    /// its parameters.
    pub value: f64,
}

impl Point {
    pub fn new(at: Beats, value: f64) -> Self {
        Self { at, value }
    }
}

/// A parameter's automation.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub param: ParamAddress,
    /// At least one, in order of time and at or after the start of the
    /// song, with finite values. Two at the same time make a step: the
    /// value jumps there from the first's to the second's, and never more
    /// than two share a time (see [`tidy`]). Before the first point the
    /// lane holds the first's value, and after the last the last's.
    pub points: Vec<Point>,
    /// Whether someone moved the parameter by hand since, so it keeps the
    /// value they gave it until automation is re-enabled. The lane stays
    /// as it was meanwhile.
    pub overridden: bool,
}

impl Lane {
    /// A lane for `param` through `points`, tidied.
    pub fn new(param: ParamAddress, mut points: Vec<Point>) -> Self {
        tidy(&mut points);
        Self {
            param,
            points,
            overridden: false,
        }
    }

    /// The lane's value at `at`, moving evenly in `scale` between points.
    /// At a step, the value after it. `None` for a lane without points.
    pub fn value_at(&self, at: Beats, scale: Scale) -> Option<f64> {
        self.value_from(self.points.partition_point(|p| p.at <= at), at, scale)
    }

    /// The lane's value as it comes up to `at`: at a step, the value before
    /// it. Where a stretch drawn over the lane starts, the lane steps from
    /// this to what was drawn.
    pub fn value_before(&self, at: Beats, scale: Scale) -> Option<f64> {
        self.value_from(self.points.partition_point(|p| p.at < at), at, scale)
    }

    /// The value at `at`, between the points before `after` and the one at
    /// `after`.
    fn value_from(&self, after: usize, at: Beats, scale: Scale) -> Option<f64> {
        let (before, next) = match (after.checked_sub(1), self.points.get(after)) {
            (None, next) => return next.map(|p| p.value),
            (Some(before), None) => return Some(self.points[before].value),
            (Some(before), Some(next)) => (self.points[before], next),
        };
        let span = (next.at - before.at).ticks() as f64;
        let t = (at - before.at).ticks() as f64 / span;
        Some(scale.mix(before.value, next.value, t))
    }

    /// Whether the lane holds one value all through: none of its points
    /// differ.
    pub fn is_flat(&self) -> bool {
        self.points.windows(2).all(|w| w[0].value == w[1].value)
    }
}

/// Makes `points` fit for a [`Lane`]: drops those with values that aren't
/// finite, moves those before the start of the song to it, puts them in
/// order of time, keeping the order of points at the same time, and keeps
/// only the first and the last of three or more at the same time.
pub fn tidy(points: &mut Vec<Point>) {
    points.retain(|p| p.value.is_finite());
    for point in points.iter_mut() {
        point.at = point.at.max(Beats::ZERO);
    }
    points.sort_by_key(|p| p.at);
    let mut kept: Vec<Point> = Vec::with_capacity(points.len());
    for (i, &point) in points.iter().enumerate() {
        let same_before = i > 0 && points[i - 1].at == point.at;
        let same_after = points.get(i + 1).is_some_and(|p| p.at == point.at);
        // A point between two at its own time changes nothing.
        if !(same_before && same_after) {
            kept.push(point);
        }
    }
    *points = kept;
}

impl Project {
    /// The lane that automates the parameter at `param`, if there is one.
    pub fn lane(&self, param: ParamAddress) -> Option<&Lane> {
        self.automation.iter().find(|l| l.param == param)
    }

    pub fn lane_mut(&mut self, param: ParamAddress) -> Option<&mut Lane> {
        self.automation.iter_mut().find(|l| l.param == param)
    }

    /// Where the lane for `param` is in the project's list of them.
    pub fn lane_index(&self, param: ParamAddress) -> Option<usize> {
        self.automation.iter().position(|l| l.param == param)
    }

    /// Whether the parameter at `param` follows its automation: it has a
    /// lane, and nobody overrode it.
    pub fn is_automated(&self, param: ParamAddress) -> bool {
        self.lane(param).is_some_and(|l| !l.overridden)
    }

    /// How the parameter at `param` spreads along its lane: the scale of
    /// its [`ParamSpec`](crate::param::ParamSpec) where the project knows
    /// it, and evenly otherwise, as a plugin's parameters do.
    pub fn param_scale(&self, param: ParamAddress) -> Scale {
        self.param_spec(param)
            .map_or(Scale::Linear, |spec| spec.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Chain;

    fn lane(points: &[(i64, f64)]) -> Lane {
        let points = points
            .iter()
            .map(|&(at, value)| Point::new(Beats::new(at), value))
            .collect();
        Lane::new(ParamAddress::Pan(Chain::Master), points)
    }

    #[test]
    fn lanes_hold_their_ends_and_move_straight_between_points() {
        let lane = lane(&[(4, 0.0), (8, 1.0)]);
        let at = |beats: f64| lane.value_at(Beats::from_f64(beats), Scale::Linear);
        assert_eq!(at(0.0), Some(0.0));
        assert_eq!(at(4.0), Some(0.0));
        assert_eq!(at(5.0), Some(0.25));
        assert_eq!(at(8.0), Some(1.0));
        assert_eq!(at(100.0), Some(1.0));
        let fader = lane.value_at(Beats::new(6), Scale::Fader).unwrap();
        assert!((fader - 0.0625).abs() < 1e-12);
    }

    #[test]
    fn two_points_at_a_time_make_a_step() {
        let stepped = lane(&[(0, 0.0), (4, 0.5), (4, -0.5), (8, 0.5)]);
        let at = |beats: f64| stepped.value_at(Beats::from_f64(beats), Scale::Linear);
        assert_eq!(at(2.0), Some(0.25));
        assert_eq!(at(4.0), Some(-0.5));
        assert_eq!(at(6.0), Some(0.0));
        let before = |beats: f64| stepped.value_before(Beats::from_f64(beats), Scale::Linear);
        assert_eq!(before(4.0), Some(0.5));
        assert_eq!(before(2.0), Some(0.25));
        assert_eq!(before(0.0), Some(0.0));
        assert_eq!(before(9.0), Some(0.5));
        assert!(!stepped.is_flat());
        assert!(lane(&[(0, 0.5), (4, 0.5)]).is_flat());
    }

    #[test]
    fn tidying_orders_points_and_drops_what_a_lane_cant_have() {
        let lane = lane(&[
            (8, 1.0),
            (-4, 0.5),
            (4, 0.1),
            (4, 0.2),
            (4, 0.3),
            (6, f64::NAN),
        ]);
        let points: Vec<(Beats, f64)> = lane.points.iter().map(|p| (p.at, p.value)).collect();
        assert_eq!(
            points,
            [
                (Beats::ZERO, 0.5),
                (Beats::new(4), 0.1),
                (Beats::new(4), 0.3),
                (Beats::new(8), 1.0),
            ]
        );
    }

    #[test]
    fn projects_find_lanes_by_parameter() {
        let mut p = Project::new();
        let param = ParamAddress::Volume(Chain::Master);
        assert!(!p.is_automated(param));
        p.automation
            .push(Lane::new(param, vec![Point::new(Beats::ZERO, 0.5)]));
        assert!(p.is_automated(param));
        assert_eq!(p.lane_index(param), Some(0));
        p.lane_mut(param).unwrap().overridden = true;
        assert!(!p.is_automated(param));
        assert!(p.lane(param).is_some());
        assert_eq!(p.param_scale(param), Scale::Fader);
    }
}
