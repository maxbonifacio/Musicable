//! Multichannel audio buffer and WAV I/O (hound).

#[derive(Clone, Debug)]
pub struct Audio {
    pub sr: u32,
    /// One Vec per channel, equal lengths.
    pub ch: Vec<Vec<f32>>,
}

impl Audio {
    pub fn new(sr: u32, ch: Vec<Vec<f32>>) -> Self {
        Audio { sr, ch }
    }
    pub fn len(&self) -> usize {
        self.ch.first().map(|c| c.len()).unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn channels(&self) -> usize {
        self.ch.len()
    }

    pub fn read_wav(path: &std::path::Path) -> Result<Audio, String> {
        let mut r = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let spec = r.spec();
        let nch = spec.channels as usize;
        let inter: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => r
                .samples::<f32>()
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?,
            hound::SampleFormat::Int => {
                let s = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
                r.samples::<i32>()
                    .map(|v| v.map(|v| v as f32 * s))
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?
            }
        };
        let n = inter.len() / nch;
        let mut ch = vec![Vec::with_capacity(n); nch];
        for f in 0..n {
            for (c, chan) in ch.iter_mut().enumerate() {
                chan.push(inter[f * nch + c]);
            }
        }
        Ok(Audio { sr: spec.sample_rate, ch })
    }

    /// Write 32-bit float WAV.
    pub fn write_wav(&self, path: &std::path::Path) -> Result<(), String> {
        let spec = hound::WavSpec {
            channels: self.ch.len() as u16,
            sample_rate: self.sr,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
        for f in 0..self.len() {
            for c in &self.ch {
                w.write_sample(c[f]).map_err(|e| e.to_string())?;
            }
        }
        w.finalize().map_err(|e| e.to_string())
    }
}
