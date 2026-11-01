//! Windows and frame I/O helpers.
//!
//! Convention used everywhere ("local", frame-centred phase): a frame centred at input
//! sample `c` holds x[c+u] w(u) for u in [-L/2, L/2), stored circularly in an FFT buffer of
//! length M >= L at index u mod M. The window is the periodic Hann centred on u = 0, so its
//! transform is real and the STFT phase of a component reflects its position relative to the
//! frame centre.

use std::f64::consts::PI;

#[derive(Clone, Debug)]
pub struct Win {
    pub len: usize,
    /// w(u), index i = u + L/2
    pub w: Vec<f32>,
    /// dw/du (per sample)
    pub dw: Vec<f32>,
    /// u * w(u)
    pub tw: Vec<f32>,
}

impl Win {
    pub fn hann(len: usize) -> Self {
        let mut w = Vec::with_capacity(len);
        let mut dw = Vec::with_capacity(len);
        let mut tw = Vec::with_capacity(len);
        let l = len as f64;
        for i in 0..len {
            let u = i as f64 - l / 2.0;
            let wv = 0.5 + 0.5 * (2.0 * PI * u / l).cos();
            let dv = -0.5 * (2.0 * PI / l) * (2.0 * PI * u / l).sin();
            w.push(wv as f32);
            dw.push(dv as f32);
            tw.push((u * wv) as f32);
        }
        Win { len, w, dw, tw }
    }
    #[inline]
    pub fn half(&self) -> i64 {
        (self.len / 2) as i64
    }
    pub fn sum_sq(&self) -> f64 {
        self.w.iter().map(|&v| (v as f64) * (v as f64)).sum()
    }
}

/// Load x[c+u]*win[u] for u in [-L/2, L/2) circularly into buf (len M). Out-of-range samples are 0.
pub fn load_frame(x: &[f32], c: i64, win: &[f32], buf: &mut [f32]) {
    let l = win.len() as i64;
    let m = buf.len() as i64;
    buf.iter_mut().for_each(|v| *v = 0.0);
    let n = x.len() as i64;
    let h = l / 2;
    // valid u range: 0 <= c+u < n
    let u_lo = (-h).max(-c);
    let u_hi = h.min(n - c); // exclusive
    let mut u = u_lo;
    while u < u_hi {
        let i = (u + h) as usize;
        let idx = u.rem_euclid(m) as usize;
        buf[idx] = x[(c + u) as usize] * win[i];
        u += 1;
    }
}

/// Overlap-add frame (circular buffer, len M, already scaled) times win[u] into out at y+u.
pub fn add_frame(out: &mut [f32], y: i64, frame: &[f32], win: &[f32], scale: f32) {
    let l = win.len() as i64;
    let m = frame.len() as i64;
    let n = out.len() as i64;
    let h = l / 2;
    let u_lo = (-h).max(-y);
    let u_hi = h.min(n - y);
    let mut u = u_lo;
    while u < u_hi {
        let i = (u + h) as usize;
        out[(y + u) as usize] += frame[u.rem_euclid(m) as usize] * win[i] * scale;
        u += 1;
    }
}

/// Accumulate win[u] (already the product of the two windows or whatever weight) at y+u.
pub fn add_weight(norm: &mut [f32], y: i64, weight: &[f32]) {
    let l = weight.len() as i64;
    let n = norm.len() as i64;
    let h = l / 2;
    let u_lo = (-h).max(-y);
    let u_hi = h.min(n - y);
    let mut u = u_lo;
    while u < u_hi {
        norm[(y + u) as usize] += weight[(u + h) as usize];
        u += 1;
    }
}
