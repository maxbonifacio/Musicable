//! Integration tests: the self-checks of `tsm-lab check` with their pass limits asserted.
//!
//! Real-material checks use /home/user/stretch-material when it exists and are skipped
//! otherwise (the material is copyrighted and not part of the repository).

use std::path::Path;
use tsm_lab::audio::Audio;
use tsm_lab::checks::{self, Row, SR};
use tsm_lab::onsets::Onset;
use tsm_lab::{Options, Variant, out_len, process};

fn material() -> Option<&'static Path> {
    let p = Path::new("/home/user/stretch-material");
    p.is_dir().then_some(p)
}

fn assert_rows(rows: Vec<Row>) {
    checks::print_table(&rows);
    let failed: Vec<&Row> = rows.iter().filter(|r| r.pass == Some(false)).collect();
    assert!(failed.is_empty(), "failed checks: {failed:#?}");
    assert!(rows.iter().any(|r| r.pass == Some(true)), "no check with a limit ran");
}

#[test]
fn exact_output_length_all_variants() {
    assert_rows(checks::check_length(&Variant::ALL));
}

#[test]
fn ratio_one_reconstructs_input() {
    assert_rows(checks::check_identity(material()));
}

#[test]
fn sine_full() {
    assert_rows(checks::check_sine(Variant::Full, &[0.5, 0.8, 1.25, 2.0]));
}

#[test]
fn sine_pv() {
    assert_rows(checks::check_sine(Variant::Pv, &[0.5, 0.8, 1.25, 2.0]));
}

#[test]
fn attacks_with_onsets_on_time_and_not_doubled() {
    assert_rows(checks::check_attacks(Variant::Full, &[0.5, 0.8, 1.25, 2.0, 4.0], true));
}

#[test]
fn white_noise_spectrum() {
    assert_rows(checks::check_noise(Variant::Full, &[0.5, 2.0, 4.0]));
}

#[test]
fn stereo_image() {
    assert_rows(checks::check_stereo(Variant::Full, &[0.8, 2.0], material()));
}

#[test]
fn pitch_shift() {
    assert_rows(checks::check_pitch(Variant::Full));
}

/// A single click with its onset must come out at exactly round(t * R): no constant latency.
#[test]
fn click_lands_on_scaled_position() {
    let n = 3 * SR as usize;
    for (t0, r) in [(40_000usize, 0.75), (52_917, 1.5), (61_003, 3.0), (70_001, 0.5)] {
        let mut x = vec![0f32; n];
        x[t0] = 0.9;
        for v in [Variant::Full, Variant::Full2Way] {
            let a = Audio::new(SR, vec![x.clone(), x.clone()]);
            let y = process(&a, &Options::new(r).variant(v).onsets(vec![Onset::at(t0 as u64)]));
            assert_eq!(y.len(), out_len(n, r));
            let (peak, _) = y.ch[0].iter().enumerate().fold((0, 0f32), |b, (i, s)| if s.abs() > b.1 { (i, s.abs()) } else { b });
            let want = (t0 as f64 * r).round() as i64;
            assert!((peak as i64 - want).abs() <= 2, "{} R={r}: peak at {peak}, expected {want}", v.name());
        }
    }
}

#[test]
fn wav_roundtrip_is_float_and_keeps_format() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let p = dir.join("roundtrip.wav");
    let x = checks::sig::wide_stereo(1000, 3);
    Audio::new(48000, x.clone()).write_wav(&p).unwrap();
    let spec = hound::WavReader::open(&p).unwrap().spec();
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(spec.bits_per_sample, 32);
    let a = Audio::read_wav(&p).unwrap();
    assert_eq!((a.sr, a.channels(), a.len()), (48000, 2, 1000));
    assert_eq!(a.ch, x);
}
