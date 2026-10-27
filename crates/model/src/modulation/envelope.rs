//! The envelope: a shape that runs once each time it's started.
//!
//! Once a gate starts it, it waits Delay ms, rises over Attack to full,
//! holds there for Hold, falls over Decay to the Sustain level and stays
//! there while the gate lasts. When the gate ends it falls over Release to
//! nothing, from wherever it has got to. A stage of 0 ms is left out, so
//! with no Delay and no Hold it's an ADSR, and at a Sustain of 100% an ASR.
//! One Shot leaves out the sustain, so it's an AD: it falls over Decay to
//! nothing whatever the gate does, and over Release too if Decay is 0 ms,
//! an AR.
//!
//! Attack, Decay and Release each bend with their curve, from -100% to
//! 100%: at 0% they move in a straight line, above it fast at first and
//! then slowly, as an analog envelope does, and below it slowly at first.
//!
//! What gates it: notes, as Note Trigger says
//! ([`TRIGGERS`](super::TRIGGERS)); its Gate Level above half way, which
//! another modulator or automation can move; and with Transport on, the
//! transport playing. Each new gate starts it again from where it is, so it never
//! jumps. The output goes from 0 to 1.

use super::TRIGGERS;
use crate::native::Scale::{Fader, Linear};
use crate::native::Unit::{Fraction, Milliseconds, Percent};
use crate::native::{ParamInfo, choice, param, switch};

/// How long it waits after it starts, in ms.
pub const DELAY: usize = 0;
/// How long it takes to rise to full, in ms.
pub const ATTACK: usize = 1;
/// How long it holds at full, in ms.
pub const HOLD: usize = 2;
/// How long it takes to fall to the sustain level, in ms.
pub const DECAY: usize = 3;
/// The level it holds while the gate lasts, in percent.
pub const SUSTAIN: usize = 4;
/// How long it takes to fall to nothing once the gate ends, in ms.
pub const RELEASE: usize = 5;
/// Whether it leaves out the sustain, falling to nothing over Decay
/// whatever the gate does.
pub const ONE_SHOT: usize = 6;
/// How the attack bends, in percent: see the [module](self).
pub const ATTACK_CURVE: usize = 7;
pub const DECAY_CURVE: usize = 8;
pub const RELEASE_CURVE: usize = 9;
/// How notes gate it: one of [`TRIGGERS`](super::TRIGGERS).
pub const NOTE_TRIGGER: usize = 10;
/// From 0 to 1: above half way, it gates the envelope.
pub const GATE: usize = 11;
/// Whether the transport playing gates it.
pub const TRANSPORT: usize = 12;

/// The longest a stage can take: 10 s.
pub const STAGE_MAX: f64 = 10_000.0;

#[rustfmt::skip]
pub(crate) const PARAMS: [ParamInfo; 13] = [
    param("delay", "Delay", "", [0.0, STAGE_MAX, 0.0], Milliseconds, Fader),
    param("attack", "Attack", "", [0.0, STAGE_MAX, 10.0], Milliseconds, Fader),
    param("hold", "Hold", "", [0.0, STAGE_MAX, 0.0], Milliseconds, Fader),
    param("decay", "Decay", "", [0.0, STAGE_MAX, 300.0], Milliseconds, Fader),
    param("sustain", "Sustain", "", [0.0, 100.0, 70.0], Percent, Linear),
    param("release", "Release", "", [0.0, STAGE_MAX, 300.0], Milliseconds, Fader),
    switch("one_shot", "One Shot", "", false),
    param("attack_curve", "Attack Curve", "", [-100.0, 100.0, 0.0], Percent, Linear),
    param("decay_curve", "Decay Curve", "", [-100.0, 100.0, 0.0], Percent, Linear),
    param("release_curve", "Release Curve", "", [-100.0, 100.0, 0.0], Percent, Linear),
    choice("note_trigger", "Note Trigger", "Gate", &TRIGGERS, 0),
    param("gate", "Level", "Gate", [0.0, 1.0, 0.0], Fraction, Linear),
    switch("transport", "Transport", "Gate", false),
];

/// How far a stage bent by `curve`, from -1.0 to 1.0, has got `t` of the
/// way through, from 0.0 to 1.0: `t` itself for a straight line, above it
/// for a curve above 0, below it for one below.
pub fn bend(t: f64, curve: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let curve = curve.clamp(-1.0, 1.0);
    let power = 1.0 + 3.0 * curve.abs();
    if curve >= 0.0 {
        1.0 - (1.0 - t).powf(power)
    } else {
        t.powf(power)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_bend_the_stages() {
        for curve in [-1.0, -0.5, 0.0, 0.5, 1.0] {
            assert_eq!(bend(0.0, curve), 0.0);
            assert_eq!(bend(1.0, curve), 1.0);
        }
        assert_eq!(bend(0.25, 0.0), 0.25);
        assert!(bend(0.25, 0.5) > 0.25);
        assert!(bend(0.25, -0.5) < 0.25);
        assert!((bend(0.5, 1.0) - (1.0 - 0.5_f64.powi(4))).abs() < 1e-12);
    }
}
