use tsm_lab::checks::{metric, sig};
fn main() {
    let sr = 44100u32;
    let xs = sig::wide_stereo(10 * sr as usize, 21);
    let tr = |v: &[f32]| v[sr as usize / 2..v.len() - sr as usize / 2].to_vec();
    let show = |name: &str, a: &[Vec<f32>]| {
        let b = metric::stereo_bands(&tr(&a[0]), &tr(&a[1]));
        let s: Vec<String> = b.iter().map(|(c, l, e)| format!("{c:.2}/{l:+.2}/{:.0}", 10.0*e.log10())).collect();
        println!("{name:>12}: {}", s.join(" "));
    };
    show("input", &xs);
    for r in [0.8, 2.0] {
        let n_out = (xs[0].len() as f64 * r).round() as usize;
        for v in [tsm_lab::Variant::Full, tsm_lab::Variant::Full2Way, tsm_lab::Variant::Pv] {
        show(&format!("{} {r}", v.name()), &tsm_lab::stretch_channels(&xs, sr, r, n_out, &tsm_lab::Options::new(r).variant(v)));
        }
    }
}
