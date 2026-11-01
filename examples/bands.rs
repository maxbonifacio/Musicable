use tsm_lab::checks::metric;
use tsm_lab::split::Separator;
// Long-term 1/3-octave level change (output minus input, dB) per variant and per part of the
// `full` engine: bands <file.wav> <ratio> <onsets.json>
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r: f64 = args[1].parse().unwrap();
    let a = tsm_lab::audio::Audio::read_wav(std::path::Path::new(&args[0])).unwrap();
    let on = tsm_lab::onsets::load(std::path::Path::new(&args[2])).unwrap();
    let bx = metric::third_octave_db(&a.ch[0]);
    let sr = a.sr;
    let n_out = (a.len() as f64 * r).round() as usize;
    let show = |name: &str, y: &[f32], inp: &[f32]| {
        let bi = metric::third_octave_db(inp);
        let by = metric::third_octave_db(y);
        let s: Vec<String> = bi.iter().zip(&by).map(|(a, b)| format!("{:+4.0}", b - a)).collect();
        println!("{name:8} {}", s.join(""));
    };
    let mut fc = 50.0f64; let mut hdr = String::new(); while fc <= 16000.0 { hdr += &format!("{:4}", if fc < 1000.0 { format!("{:.0}", fc) } else { format!("{:.0}k", fc/1000.0) }); fc *= 2f64.powf(1.0/3.0); }
    println!("band     {hdr}");
    for v in tsm_lab::Variant::ALL {
        let y = tsm_lab::process(&a, &tsm_lab::Options::new(r).variant(v).onsets(on.clone()));
        show(v.name(), &y.ch[0], &a.ch[0]);
    }
    // the `full` engine's parts, stretched one by one (same settings as lib.rs at 44.1 kHz)
    let mut sep = tsm_lab::split::MedianSplit::full(sr);
    sep.energy_parts = true;
    let mut parts = sep.split(&a.ch, sr);
    let onf: Vec<i64> = on.iter().map(|o| o.frame as i64).collect();
    tsm_lab::split::percussive_attacks_only(&mut parts, &onf, 512, 256);
    let strong: Vec<i64> = on.iter().filter(|o| o.strength >= 0.25).map(|o| o.frame as i64).collect();
    tsm_lab::split::gate_before_onsets(&mut parts, &a.ch, &strong, sep.win);
    let h = tsm_lab::pv::stretch_pv(&parts.harmonic, r, n_out, &on, &tsm_lab::pv::PvParams::new(sr));
    show("pv(H)", &h[0], &parts.harmonic[0]);
    let map = tsm_lab::timemap::TimeMap::anchored(r, &onf, 256.0);
    let p = tsm_lab::ola::stretch_ola(&parts.percussive, &map, n_out, &tsm_lab::ola::OlaParams{win:512,hop:256,tol:448}, &onf, 256);
    show("ola(P)", &p[0], &parts.percussive[0]);
    let nz = tsm_lab::noise::stretch_noise(&parts.residual, r, n_out, &onf, &tsm_lab::noise::NoiseParams::new(sr));
    show("noise(R)", &nz[0], &parts.residual[0]);
    let e = |v: &[f32]| 10.0 * v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().log10();
    println!("part energies dB: H {:.1} P {:.1} R {:.1} x {:.1}", e(&parts.harmonic[0]), e(&parts.percussive[0]), e(&parts.residual[0]), e(&a.ch[0]));
    let _ = bx;
}
