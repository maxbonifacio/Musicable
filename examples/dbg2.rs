use tsm_lab::checks::{metric, sig};
use tsm_lab::split::Separator;
fn main() {
    let sr = 44100u32;
    let x = sig::white(12 * sr as usize, 0.1, 11);
    let tr = |v: &[f32]| v[sr as usize / 2..v.len() - sr as usize / 2].to_vec();
    let bx = metric::third_octave_db(&tr(&x));
    for &r in &[0.5f64, 2.0, 4.0] {
        let n_out = (x.len() as f64 * r).round() as usize;
        for st in [true, false] {
            let mut p = tsm_lab::pv::PvParams::new(sr);
            p.scale_toff = st;
            let y = tsm_lab::pv::stretch_pv(&[x.clone()], r, n_out, &[], &p);
            let by = metric::third_octave_db(&tr(&y[0]));
            let mean: f64 = by.iter().zip(&bx).map(|(a, b)| a - b).sum::<f64>() / bx.len() as f64;
            println!("pv noise R={r} scale_toff={st}: mean dev {mean:+.2} dB");
        }
    }
    // stereo per band, per path
    let xs = sig::wide_stereo(10 * sr as usize, 21);
    let show = |name: &str, a: &[Vec<f32>]| {
        let b = metric::stereo_bands(&tr(&a[0]), &tr(&a[1]));
        let s: Vec<String> = b.iter().map(|(c, l, _)| format!("{c:.2}/{l:+.1}")).collect();
        println!("{name:>12}: {}", s.join(" "));
    };
    show("input", &xs);
    let parts = tsm_lab::split::MedianSplit::full(sr).split(&xs, sr);
    show("H", &parts.harmonic);
    show("P", &parts.percussive);
    show("R", &parts.residual);
    
    let r = 2.0;
    let n_out = (xs[0].len() as f64 * r).round() as usize;
    let p = tsm_lab::pv::PvParams::new(sr);
    show("pv(H)", &tsm_lab::pv::stretch_pv(&parts.harmonic, r, n_out, &[], &p));
    show("pv(x)", &tsm_lab::pv::stretch_pv(&xs, r, n_out, &[], &p));
    show("ola(P)", &tsm_lab::ola::stretch_ola(&parts.percussive, &tsm_lab::timemap::TimeMap::uniform(r), n_out, 512, 256));
    show("noise(drv)", &tsm_lab::noise::stretch_noise(&parts.residual, r, n_out, &[], &tsm_lab::noise::NoiseParams::new(sr)));
    show("noise(x)", &tsm_lab::noise::stretch_noise(&xs, r, n_out, &[], &tsm_lab::noise::NoiseParams::new(sr)));
    show("full", &tsm_lab::stretch_channels(&xs, sr, r, n_out, &tsm_lab::Options::new(r)));
}
