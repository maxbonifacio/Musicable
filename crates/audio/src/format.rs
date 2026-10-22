//! Writing the engine's f32 samples in the device's format.

use crate::SampleFormat;

impl SampleFormat {
    /// Bytes per sample.
    pub(crate) fn bytes(self) -> usize {
        match self {
            Self::F64 => 8,
            Self::F32 | Self::I32 => 4,
            Self::I24 => 3,
            Self::I16 => 2,
        }
    }

    /// Writes interleaved `samples` into `out` as little-endian samples of
    /// this format, clipping at full scale. Real-time safe.
    ///
    /// # Panics
    ///
    /// If `out` isn't exactly `samples.len() * self.bytes()` bytes long.
    pub(crate) fn write(self, samples: &[f32], out: &mut [u8]) {
        assert_eq!(
            out.len(),
            samples.len() * self.bytes(),
            "device buffer size"
        );
        let pairs = samples.iter().zip(out.chunks_exact_mut(self.bytes()));
        match self {
            Self::F32 => pairs.for_each(|(s, o)| o.copy_from_slice(&s.to_le_bytes())),
            Self::F64 => pairs.for_each(|(s, o)| o.copy_from_slice(&f64::from(*s).to_le_bytes())),
            Self::I32 => pairs.for_each(|(s, o)| o.copy_from_slice(&to_i32(*s).to_le_bytes())),
            Self::I24 => pairs.for_each(|(s, o)| o.copy_from_slice(&to_i24(*s).to_le_bytes()[..3])),
            Self::I16 => pairs.for_each(|(s, o)| o.copy_from_slice(&to_i16(*s).to_le_bytes())),
        }
    }
}

// Full scale is the largest positive value, so +1.0 and -1.0 stay
// symmetrical. A NaN becomes zero: `as` saturates and maps NaN to 0.

/// `sample` as a `bits`-bit integer, from 2 to 32 bits, clipping at full
/// scale.
pub(crate) fn to_int(sample: f32, bits: u32) -> i32 {
    let full_scale = ((1_i64 << (bits - 1)) - 1) as f64;
    (f64::from(sample.clamp(-1.0, 1.0)) * full_scale).round() as i32
}

fn to_i32(sample: f32) -> i32 {
    to_int(sample, 32)
}

fn to_i24(sample: f32) -> i32 {
    to_int(sample, 24)
}

fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(format: SampleFormat, samples: &[f32]) -> Vec<u8> {
        let mut out = vec![0; samples.len() * format.bytes()];
        format.write(samples, &mut out);
        out
    }

    #[test]
    fn floats_pass_through() {
        let out = written(SampleFormat::F32, &[0.25, -1.5]);
        assert_eq!(out[..4], 0.25f32.to_le_bytes());
        assert_eq!(out[4..], (-1.5f32).to_le_bytes(), "floats aren't clipped");
        let out = written(SampleFormat::F64, &[0.25]);
        assert_eq!(out, 0.25f64.to_le_bytes());
    }

    #[test]
    fn integers_clip_at_full_scale() {
        let out = written(SampleFormat::I16, &[1.0, -1.0, 2.0, 0.5, f32::NAN]);
        let values: Vec<i16> = out
            .chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(values, [32767, -32767, 32767, 16384, 0]);

        let out = written(SampleFormat::I32, &[1.0, -1.0, 0.0]);
        let values: Vec<i32> = out
            .chunks(4)
            .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(values, [i32::MAX, -i32::MAX, 0]);
    }

    #[test]
    fn packed_24_bit_is_three_little_endian_bytes() {
        let out = written(SampleFormat::I24, &[1.0, -1.0, 0.5]);
        assert_eq!(out.len(), 9);
        assert_eq!(out[..3], [0xFF, 0xFF, 0x7F]);
        assert_eq!(out[3..6], [0x01, 0x00, 0x80]);
        // 0.5 * (2^23 - 1) rounds to 2^22.
        assert_eq!(out[6..], [0x00, 0x00, 0x40]);
    }

    #[test]
    fn writing_uses_no_heap() {
        let samples = [0.1f32; 64];
        let mut out = [0u8; 64 * 3];
        musicable_engine::rt::scope(|| SampleFormat::I24.write(&samples, &mut out));
    }
}
