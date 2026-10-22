//! Writing the engine's samples in an ASIO channel's sample type.

use super::sys::{Long, sample_type as types};
use crate::SampleFormat;
use crate::format::to_int;

/// How a channel stores samples, for the sample types we can write: every
/// one but DSD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Encoding {
    kind: Kind,
    big_endian: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int16,
    /// Packed in 3 bytes.
    Int24,
    /// A `bits`-bit integer in a 32-bit word: full scale at 32 bits, or a
    /// 16- to 24-bit sample in the low bits.
    Int32 {
        bits: u32,
    },
    Float32,
    Float64,
}

impl Encoding {
    /// The encoding of an `ASIOSampleType`, or `None` for DSD and types the
    /// specification doesn't have.
    pub fn from_type(sample_type: Long) -> Option<Encoding> {
        let (kind, big_endian) = match sample_type {
            types::INT16_LSB => (Kind::Int16, false),
            types::INT24_LSB => (Kind::Int24, false),
            types::INT32_LSB => (Kind::Int32 { bits: 32 }, false),
            types::FLOAT32_LSB => (Kind::Float32, false),
            types::FLOAT64_LSB => (Kind::Float64, false),
            types::INT32_LSB16 => (Kind::Int32 { bits: 16 }, false),
            types::INT32_LSB18 => (Kind::Int32 { bits: 18 }, false),
            types::INT32_LSB20 => (Kind::Int32 { bits: 20 }, false),
            types::INT32_LSB24 => (Kind::Int32 { bits: 24 }, false),
            types::INT16_MSB => (Kind::Int16, true),
            types::INT24_MSB => (Kind::Int24, true),
            types::INT32_MSB => (Kind::Int32 { bits: 32 }, true),
            types::FLOAT32_MSB => (Kind::Float32, true),
            types::FLOAT64_MSB => (Kind::Float64, true),
            types::INT32_MSB16 => (Kind::Int32 { bits: 16 }, true),
            types::INT32_MSB18 => (Kind::Int32 { bits: 18 }, true),
            types::INT32_MSB20 => (Kind::Int32 { bits: 20 }, true),
            types::INT32_MSB24 => (Kind::Int32 { bits: 24 }, true),
            _ => return None,
        };
        Some(Encoding { kind, big_endian })
    }

    /// Bytes per sample.
    pub fn bytes(self) -> usize {
        match self.kind {
            Kind::Int16 => 2,
            Kind::Int24 => 3,
            Kind::Int32 { .. } | Kind::Float32 => 4,
            Kind::Float64 => 8,
        }
    }

    /// The format, as [`StreamInfo`](crate::StreamInfo) says it.
    pub fn format(self) -> SampleFormat {
        match self.kind {
            Kind::Int16 => SampleFormat::I16,
            Kind::Int24 => SampleFormat::I24,
            Kind::Int32 { .. } => SampleFormat::I32,
            Kind::Float32 => SampleFormat::F32,
            Kind::Float64 => SampleFormat::F64,
        }
    }

    /// Writes `samples` into `out` in this encoding, clipping integers at
    /// full scale. Real-time safe.
    ///
    /// # Panics
    ///
    /// If `out` isn't exactly `samples.len() * self.bytes()` bytes long.
    pub fn write(self, samples: &[f32], out: &mut [u8]) {
        assert_eq!(out.len(), samples.len() * self.bytes(), "ASIO buffer size");
        let big_endian = self.big_endian;
        for (&sample, out) in samples.iter().zip(out.chunks_exact_mut(self.bytes())) {
            match self.kind {
                Kind::Int16 => put(out, &(to_int(sample, 16) as i16).to_le_bytes(), big_endian),
                Kind::Int24 => put(out, &to_int(sample, 24).to_le_bytes()[..3], big_endian),
                Kind::Int32 { bits } => put(out, &to_int(sample, bits).to_le_bytes(), big_endian),
                Kind::Float32 => put(out, &sample.to_le_bytes(), big_endian),
                Kind::Float64 => put(out, &f64::from(sample).to_le_bytes(), big_endian),
            }
        }
    }
}

/// Writes a sample's little-endian bytes, reversed for big-endian.
fn put(out: &mut [u8], little_endian: &[u8], big_endian: bool) {
    if big_endian {
        out.iter_mut()
            .zip(little_endian.iter().rev())
            .for_each(|(out, byte)| *out = *byte);
    } else {
        out.copy_from_slice(little_endian);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(sample_type: Long, samples: &[f32]) -> Vec<u8> {
        let encoding = Encoding::from_type(sample_type).unwrap();
        let mut out = vec![0; samples.len() * encoding.bytes()];
        encoding.write(samples, &mut out);
        out
    }

    fn words(bytes: &[u8], read: fn([u8; 4]) -> i32) -> Vec<i32> {
        bytes
            .chunks(4)
            .map(|word| read(word.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn most_drivers_take_32_bit_integers() {
        let out = written(types::INT32_LSB, &[1.0, -1.0, 0.0, 2.0]);
        assert_eq!(
            words(&out, i32::from_le_bytes),
            [i32::MAX, -i32::MAX, 0, i32::MAX]
        );
    }

    #[test]
    fn narrow_samples_sit_in_the_low_bits_of_32() {
        let out = written(types::INT32_LSB24, &[1.0, -1.0, 0.5]);
        assert_eq!(
            words(&out, i32::from_le_bytes),
            [(1 << 23) - 1, -((1 << 23) - 1), 1 << 22]
        );
        let out = written(types::INT32_LSB16, &[1.0, -0.5]);
        assert_eq!(words(&out, i32::from_le_bytes), [32767, -16384]);
        let out = written(types::INT32_MSB20, &[1.0]);
        assert_eq!(words(&out, i32::from_be_bytes), [(1 << 19) - 1]);
    }

    #[test]
    fn big_endian_types_reverse_the_bytes() {
        assert_eq!(written(types::INT16_MSB, &[1.0]), [0x7F, 0xFF]);
        assert_eq!(written(types::INT24_MSB, &[1.0]), [0x7F, 0xFF, 0xFF]);
        assert_eq!(written(types::INT24_LSB, &[-1.0]), [0x01, 0x00, 0x80]);
        assert_eq!(written(types::FLOAT32_MSB, &[0.25]), 0.25f32.to_be_bytes());
        assert_eq!(
            written(types::FLOAT64_LSB, &[-0.5]),
            (-0.5f64).to_le_bytes()
        );
    }

    #[test]
    fn dsd_and_unknown_types_are_refused() {
        for sample_type in [32, 33, 40, 5, 99, -1] {
            assert_eq!(Encoding::from_type(sample_type), None, "{sample_type}");
        }
    }

    #[test]
    fn formats_and_sizes() {
        let encoding = |sample_type| Encoding::from_type(sample_type).unwrap();
        assert_eq!(encoding(types::INT32_LSB24).format(), SampleFormat::I32);
        assert_eq!(encoding(types::INT32_LSB24).bytes(), 4);
        assert_eq!(encoding(types::FLOAT64_MSB).format(), SampleFormat::F64);
        assert_eq!(encoding(types::FLOAT64_MSB).bytes(), 8);
        assert_eq!(encoding(types::INT24_LSB).bytes(), 3);
    }

    #[test]
    fn writing_uses_no_heap() {
        let samples = [0.1f32; 64];
        let mut out = [0u8; 64 * 4];
        let encoding = Encoding::from_type(types::INT32_MSB).unwrap();
        musicable_engine::rt::scope(|| encoding.write(&samples, &mut out));
    }
}
