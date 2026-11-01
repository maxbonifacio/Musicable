use tsm_lab::checks::sig;
use tsm_lab::split::Separator;
use tsm_lab::fft::Fft;
use realfft::num_complex::Complex32;
fn band_energy(x: &[f32], lo: f64, hi: f64) -> f64 {
    let m = 4096; let mut f = Fft::new(m); let mut acc = 0.0;
    let mut buf = vec![0f32; m]; let mut sp = vec![Complex32::new(0.0,0.0); m/2+1];
    let mut t = 0; while t + m <= x.len() { for i in 0..m { buf[i] = x[t+i] * (0.5 - 0.5*(2.0*std::f32::consts::PI*i as f32/m as f32).cos()); }
      f.forward(&mut buf, &mut sp); for k in 0..sp.len() { let fk = k as f64*44100.0/m as f64; if fk>=lo && fk<hi { acc += sp[k].norm_sqr() as f64; } } t += m/2; }
    acc
}
fn main() {
    let sr = 44100u32; let n = 6 * sr as usize;
    let tone = sig::sine(n, 330.0, 0.108);
    let noise = sig::white(n, 0.078, 3);
    let x: Vec<f32> = tone.iter().zip(&noise).map(|(a, b)| a + b).collect();
    let p = tsm_lab::split::MedianSplit::full(sr).split(&[x.clone()], sr);
    for (name, v) in [("x", &x), ("H", &p.harmonic[0]), ("P", &p.percussive[0]), ("R", &p.residual[0])] {
        println!("{name:6} tone-band(300-360) {:8.2} dB  skirt(354-420) {:8.2} dB  noise-band(1000-1300) {:8.2} dB", 10.0*band_energy(v,300.0,360.0).log10(), 10.0*band_energy(v,354.0,420.0).log10(), 10.0*band_energy(v,1000.0,1300.0).log10());
    }
}
