// Smoke test on real material: level, peak, NaN and long-term spectrum deviation per variant.
use tsm_lab::checks::metric;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r: f64 = args[1].parse().unwrap();
    let a = tsm_lab::audio::Audio::read_wav(std::path::Path::new(&args[0])).unwrap();
    let on = args.get(2).map(|p| tsm_lab::onsets::load(std::path::Path::new(p)).unwrap()).unwrap_or_default();
    let rms = |v: &[f32]| (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
    let bx = metric::third_octave_db(&a.ch[0]);
    for v in tsm_lab::Variant::ALL {
        let y = tsm_lab::process(&a, &tsm_lab::Options::new(r).variant(v).onsets(on.clone()));
        let nan = y.ch.iter().flatten().any(|x| !x.is_finite());
        let pk = y.ch.iter().flatten().fold(0f32, |m, x| m.max(x.abs()));
        let by = metric::third_octave_db(&y.ch[0]);
        let dev: Vec<f64> = bx.iter().zip(&by).map(|(a, b)| b - a).collect();
        let maxdev = dev.iter().fold(0f64, |m, d| m.max(d.abs()));
        println!("{:10} rms L {:+.2} dB R {:+.2} dB  peak {:.3} (in {:.3})  nan {nan}  1/3-oct max dev {maxdev:.2} dB", v.name(),
            20.0 * (rms(&y.ch[0]) / rms(&a.ch[0])).log10(), 20.0 * (rms(&y.ch[1]) / rms(&a.ch[1])).log10(), pk,
            a.ch.iter().flatten().fold(0f32, |m, x| m.max(x.abs())));
    }
}
