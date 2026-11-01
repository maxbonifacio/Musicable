//! Monotone piecewise-linear time maps from output sample position y to input position t.

#[derive(Clone, Debug)]
pub struct TimeMap {
    /// Strictly increasing control points (y, t).
    pts: Vec<(f64, f64)>,
    /// Slope dt/dy used outside the control points (1/R).
    slope: f64,
}

impl TimeMap {
    /// t = y / r.
    pub fn uniform(r: f64) -> Self {
        TimeMap { pts: vec![(0.0, 0.0)], slope: 1.0 / r }
    }

    /// Onset-anchored map for grain placement (percussive OLA path).
    ///
    /// Around each onset t0 (input sample) the map has slope 1 over [y*-h, y*+h] with
    /// y* = round(r*t0) and t(y*) = t0 exactly, so every grain overlapping the attack reads it
    /// from the same relative position and all copies land on y*. Outside the core the map
    /// bends back to t = y/r over K*h output samples (K = max(3, 2r-1)), so no grain whose
    /// window misses the core can see the attack. Close onsets shrink h; bends that would
    /// break monotonicity are dropped.
    pub fn anchored(r: f64, onsets: &[i64], half: f64) -> Self {
        if onsets.is_empty() {
            return Self::uniform(r);
        }
        let k = (2.0 * r - 1.0).max(3.0);
        let ys: Vec<f64> = onsets.iter().map(|&t| (r * t as f64).round()).collect();
        let ts: Vec<f64> = onsets.iter().map(|&t| t as f64).collect();
        // (y, t, is_bend)
        let mut pts: Vec<(f64, f64, bool)> = Vec::new();
        let push = |pts: &mut Vec<(f64, f64, bool)>, p: (f64, f64, bool)| {
            loop {
                match pts.last() {
                    None => {
                        pts.push(p);
                        return;
                    }
                    Some(&(ly, lt, lb)) => {
                        if p.0 > ly && p.1 > lt {
                            pts.push(p);
                            return;
                        }
                        if lb {
                            pts.pop();
                            continue;
                        }
                        // conflicting non-bend: skip p
                        return;
                    }
                }
            }
        };
        for i in 0..ys.len() {
            let (y, t) = (ys[i], ts[i]);
            let mut h = half;
            if i > 0 {
                h = h.min(0.49 * (y - ys[i - 1]).min(t - ts[i - 1]));
            }
            if i + 1 < ys.len() {
                h = h.min(0.49 * (ys[i + 1] - y).min(ts[i + 1] - t));
            }
            if h < 1.0 {
                push(&mut pts, (y, t, false));
                continue;
            }
            let yb = y - k * h;
            push(&mut pts, (yb, yb / r, true));
            push(&mut pts, (y - h, t - h, false));
            push(&mut pts, (y, t, false));
            push(&mut pts, (y + h, t + h, false));
            let ya = y + k * h;
            push(&mut pts, (ya, ya / r, true));
        }
        TimeMap { pts: pts.into_iter().map(|(y, t, _)| (y, t)).collect(), slope: 1.0 / r }
    }

    pub fn eval(&self, y: f64) -> f64 {
        let p = &self.pts;
        if y <= p[0].0 {
            return p[0].1 + (y - p[0].0) * self.slope;
        }
        let last = p[p.len() - 1];
        if y >= last.0 {
            return last.1 + (y - last.0) * self.slope;
        }
        let i = p.partition_point(|q| q.0 <= y); // p[i-1].0 <= y < p[i].0
        let (y0, t0) = p[i - 1];
        let (y1, t1) = p[i];
        t0 + (y - y0) * (t1 - t0) / (y1 - y0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anchored_hits_anchor_and_is_monotone() {
        for &r in &[0.5, 0.8, 1.0, 1.25, 2.0, 4.0] {
            let on = vec![1000i64, 1300, 5000, 5100, 20000];
            let m = TimeMap::anchored(r, &on, 256.0);
            for &t in &on {
                let y = (r * t as f64).round();
                assert!((m.eval(y) - t as f64).abs() < 1e-9, "r={r} t={t}");
            }
            let mut prev = f64::NEG_INFINITY;
            for y in -2000..60000 {
                let t = m.eval(y as f64);
                assert!(t > prev);
                prev = t;
            }
        }
    }
}
