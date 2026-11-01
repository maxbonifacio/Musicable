use tsm_lab::checks::{metric, sig, tonal_attack_eval};
use tsm_lab::split::Separator;
fn main() {
    let sr = 44100u32;
    let (x, on) = sig::plucked_notes(7 * sr as usize, 4);
    let ons = sig::onsets(&on);
    let r = 2.0;
    let n_out = (x.len() as f64 * r).round() as usize;
    let xs = vec![x.clone(), x.clone()];
    let mut parts = *tsm_lab::split::MedianSplit::full(sr).split(&xs, sr).stretch.unwrap();
    tsm_lab::split::gate_before_onsets(&mut parts, &xs, &on.iter().map(|&v| v as i64).collect::<Vec<_>>(), 2048);
    let e = |v: &[f32]| v.iter().map(|a| (*a as f64).powi(2)).sum::<f64>();
    println!("energy H {:.3} P {:.3} R {:.3}", e(&parts.harmonic[0]) / e(&x), e(&parts.percussive[0]) / e(&x), e(&parts.residual[0]) / e(&x));
    let p = tsm_lab::pv::PvParams::new(sr);
    let h = tsm_lab::pv::stretch_pv(&parts.harmonic, r, n_out, &ons, &p);
    let map = tsm_lab::timemap::TimeMap::anchored(r, &on.iter().map(|&v| v as i64).collect::<Vec<_>>(), 256.0);
    let pp = tsm_lab::ola::stretch_ola(&parts.percussive, &map, n_out, 512, 256);
    let nz = tsm_lab::noise::stretch_noise(&parts.residual, r, n_out, &on.iter().map(|&v| v as i64).collect::<Vec<_>>(), &tsm_lab::noise::NoiseParams::new(sr));
    let tot: Vec<f32> = (0..n_out).map(|i| h[0][i] + pp[0][i] + nz[0][i]).collect();
    let ms = 44usize;
    let et = metric::env_rms(&tot, 5 * ms);
    for (name, y) in [("H", &h[0]), ("P", &pp[0]), ("N", &nz[0]), ("tot", &tot)] {
        let ey = metric::env_rms(y, 5 * ms);
        let mut s = String::new();
        for &t in on.iter().take(6) {
            let ys = (r * t as f64).round() as usize;
            let pk = et[ys..ys + 20 * ms].iter().cloned().fold(0f32, f32::max);
            let pre = ey[ys - 50 * ms..ys - 4 * ms].iter().cloned().fold(0f32, f32::max);
            s += &format!("{:7.1}", 20.0 * (pre / pk).log10());
        }
        println!("{name:4} pre-echo dB re total peak per onset: {s}");
    }
    let (err, pre) = tonal_attack_eval(&x, &tot, &on, r);
    println!("timing err {err:.2} ms, pre-echo excess {pre:.1} dB");
    let (err, pre) = tonal_attack_eval(&x, &h[0], &on, r);
    println!("H only: timing err {err:.2} ms, pre-echo excess {pre:.1} dB");
    let ex1 = metric::env_rms(&x, 264);
    let ey1 = metric::env_rms(&tot, 264); let eh1 = metric::env_rms(&h[0], 264);
    for &t in on.iter().skip(1).take(6) {
        let t = t as usize; let ys = (r * t as f64).round() as usize;
        let row_in: Vec<String> = (-10i64..=10).map(|d| format!("{:5.1}", 20.0*(ex1[(t as i64 + d*44) as usize] as f64).log10())).collect();
        let row_out: Vec<String> = (-10i64..=10).map(|d| format!("{:5.1}", 20.0*(ey1[(ys as i64 + d*88) as usize] as f64).log10())).collect();
        println!("in  (1ms steps)  {}", row_in.join(" "));
        println!("out (2ms steps)  {}", row_out.join(" "));
        let row_h: Vec<String> = (-10i64..=10).map(|d| format!("{:5.1}", 20.0*(eh1[(ys as i64 + d*88) as usize] as f64).log10())).collect();
        println!("H   (2ms steps)  {}", row_h.join(" "));
        let a = metric::attack_time(&ex1, t, 12*44, 0.5).unwrap() as i64 - t as i64;
        let b = metric::attack_time(&ey1, ys, 12*44, 0.5).unwrap() as i64 - ys as i64;
        println!("attack in {a} out {b}");
    }
    // input's own pre level
    let ex = metric::env_rms(&x, 5 * ms);
    let mut s = String::new();
    for &t in on.iter().take(6) {
        let t = t as usize;
        let pk = ex[t..t + 10 * ms].iter().cloned().fold(0f32, f32::max);
        let pre = ex[t - 25 * ms..t - 2 * ms].iter().cloned().fold(0f32, f32::max);
        s += &format!("{:7.1}", 20.0 * (pre / pk).log10());
    }
    println!("input pre level: {s}");
}
#[allow(dead_code)]
fn unused() {}
