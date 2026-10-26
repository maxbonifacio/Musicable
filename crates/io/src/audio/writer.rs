//! Writing an audio file as its samples come: [`Writer`].

use std::io::{self, Seek, SeekFrom, Write};

use super::flac_encoder::FlacEncoder;
use super::{AudioError, Format, SampleFormat, aiff, quantize, unsupported, wav};

/// The random numbers TPDF dither starts from, whatever the file: the same
/// samples always dither the same way.
const DITHER_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// Writes a WAV, AIFF or FLAC file as its samples come, such as an
/// export's. Integer samples are rounded, and clip at full scale; with
/// [`dither`](Self::with_dither), TPDF dither goes in first, except where
/// the audio is silent. The file's header gets its length when it
/// [finishes](Self::finish).
///
/// Samples come as 32-bit floats, as an export renders them, or as 64-bit
/// ones ([`write_f64`](Self::write_f64)), which carry a file's samples
/// exactly, as [`Decoder::read_f64`](super::Decoder::read_f64) reads them:
/// written undithered in a [`SampleFormat`] that
/// [holds](SampleFormat::holds) their encoding, they come out unchanged.
///
/// WAV and AIFF files hold up to 4 GiB of samples.
#[derive(Debug)]
pub struct Writer<W: Write + Seek> {
    out: Out<W>,
    sample_format: SampleFormat,
    sample_rate: u32,
    channels: u16,
    frames: u64,
    dither: Option<Random>,
    /// Scratch space for a write's samples.
    bytes: Vec<u8>,
    integers: Vec<i32>,
}

#[derive(Debug)]
enum Out<W: Write + Seek> {
    Wav(W),
    Aiff(W),
    Flac(Box<FlacEncoder<W>>),
}

impl<W: Write + Seek> Writer<W> {
    /// Starts a `format` file of `channels` channels of `sample_format`
    /// samples at `sample_rate` at the start of `out`, writing its header.
    /// FLAC files take integers of up to 24 bits only.
    pub fn new(
        mut out: W,
        format: Format,
        sample_format: SampleFormat,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, AudioError> {
        if channels == 0 {
            return unsupported("a file without channels");
        }
        if sample_rate == 0 {
            return unsupported("a sample rate of 0 Hz");
        }
        let out = match format {
            Format::Wav => {
                let mut header = Vec::with_capacity(wav::HEADER);
                wav::header(&mut header, sample_rate, sample_format, channels, 0);
                out.seek(SeekFrom::Start(0))?;
                out.write_all(&header)?;
                Out::Wav(out)
            }
            Format::Aiff => {
                let header = aiff::header(sample_rate, sample_format, channels, 0)?;
                out.seek(SeekFrom::Start(0))?;
                out.write_all(&header)?;
                Out::Aiff(out)
            }
            Format::Flac => {
                let bits = match sample_format {
                    SampleFormat::Int8 => 8,
                    SampleFormat::Int16 => 16,
                    SampleFormat::Int24 => 24,
                    SampleFormat::Int32 => return unsupported("FLAC files of 32-bit integers"),
                    SampleFormat::Float32 | SampleFormat::Float64 => {
                        return unsupported("FLAC files of floats");
                    }
                };
                Out::Flac(Box::new(FlacEncoder::new(
                    out,
                    sample_rate,
                    channels,
                    bits,
                )?))
            }
        };
        Ok(Self {
            out,
            sample_format,
            sample_rate,
            channels,
            frames: 0,
            dither: None,
            bytes: Vec::new(),
            integers: Vec::new(),
        })
    }

    /// Dithers integer samples: adds triangular noise of up to one step
    /// either way before rounding, so what rounding loses is noise, not
    /// distortion. Exact silence stays silent. Floats aren't dithered.
    pub fn with_dither(mut self) -> Self {
        self.dither = Some(Random(DITHER_SEED));
        self
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// How many more frames the file has room for.
    pub fn room(&self) -> u64 {
        let frame = u64::from(self.channels) * self.sample_format.bytes() as u64;
        let most = match self.out {
            Out::Wav(_) => wav::MAX_DATA / frame,
            Out::Aiff(_) => (wav::MAX_DATA / frame).min(u64::from(u32::MAX)),
            // STREAMINFO counts them in 36 bits.
            Out::Flac(_) => (1 << 36) - 1,
        };
        most.saturating_sub(self.frames)
    }

    /// Writes `samples`, whole frames of interleaved channels, after the
    /// ones before.
    ///
    /// # Panics
    ///
    /// If `samples` isn't whole frames.
    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        self.write_samples(samples.len(), |i| f64::from(samples[i]))
    }

    /// Writes `samples` as [`write`](Self::write) does, from 64-bit floats.
    ///
    /// # Panics
    ///
    /// If `samples` isn't whole frames.
    pub fn write_f64(&mut self, samples: &[f64]) -> io::Result<()> {
        self.write_samples(samples.len(), |i| samples[i])
    }

    /// Writes `count` samples, the `i`th of which `sample(i)` gives.
    fn write_samples(&mut self, count: usize, sample: impl Fn(usize) -> f64) -> io::Result<()> {
        let channels = usize::from(self.channels);
        assert!(
            count.is_multiple_of(channels),
            "{count} samples aren't whole frames of {channels} channels",
        );
        let frames = (count / channels) as u64;
        if frames > self.room() {
            let format = match self.out {
                Out::Wav(_) => "a WAV file",
                Out::Aiff(_) => "an AIFF file",
                Out::Flac(_) => "a FLAC file",
            };
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("too long for {format}"),
            ));
        }
        let format = self.sample_format;
        let bits = format.bits();
        let size = format.bytes();
        let big = matches!(self.out, Out::Aiff(_));
        self.bytes.clear();
        self.integers.clear();
        for i in 0..count {
            let sample = sample(i);
            match format {
                SampleFormat::Float32 => {
                    let sample = sample as f32;
                    let bytes = if big {
                        sample.to_be_bytes()
                    } else {
                        sample.to_le_bytes()
                    };
                    self.bytes.extend_from_slice(&bytes);
                    continue;
                }
                SampleFormat::Float64 => {
                    let bytes = if big {
                        sample.to_be_bytes()
                    } else {
                        sample.to_le_bytes()
                    };
                    self.bytes.extend_from_slice(&bytes);
                    continue;
                }
                _ => {}
            }
            let integer = quantize(sample, bits, self.dither.as_mut());
            match self.out {
                // WAV's 8-bit samples are unsigned.
                Out::Wav(_) if bits == 8 => self.bytes.push((integer + 128) as u8),
                Out::Wav(_) => self.bytes.extend_from_slice(&integer.to_le_bytes()[..size]),
                Out::Aiff(_) => self
                    .bytes
                    .extend_from_slice(&integer.to_be_bytes()[4 - size..]),
                Out::Flac(_) => self.integers.push(integer),
            }
        }
        match &mut self.out {
            Out::Wav(out) | Out::Aiff(out) => out.write_all(&self.bytes)?,
            Out::Flac(encoder) => encoder.write(&self.integers)?,
        }
        self.frames += frames;
        Ok(())
    }

    /// Finishes the file, putting its length in its header, and hands back
    /// `out`.
    pub fn finish(self) -> io::Result<W> {
        let data = self.frames * u64::from(self.channels) * self.sample_format.bytes() as u64;
        let (mut out, header) = match self.out {
            Out::Flac(encoder) => return encoder.finish(),
            Out::Wav(out) => {
                let mut header = Vec::with_capacity(wav::HEADER);
                let (rate, format) = (self.sample_rate, self.sample_format);
                wav::header(&mut header, rate, format, self.channels, self.frames);
                (out, header)
            }
            Out::Aiff(out) => {
                let header = aiff::header(
                    self.sample_rate,
                    self.sample_format,
                    self.channels,
                    self.frames,
                )?;
                (out, header)
            }
        };
        // Chunks end on even bytes.
        if data % 2 == 1 {
            out.write_all(&[0])?;
        }
        let end = out.stream_position()?;
        out.seek(SeekFrom::Start(0))?;
        out.write_all(&header)?;
        out.seek(SeekFrom::Start(end))?;
        out.flush()?;
        Ok(out)
    }
}

/// Random numbers for dither: SplitMix64.
#[derive(Debug, Clone)]
pub(super) struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// From 0 up to 1.
    pub(super) fn uniform(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::super::tests::{FORMATS, decode, signal, step};
    use super::super::{Decoder, Info};
    use super::*;

    /// Writes `channels` in uneven pieces, as an export does.
    fn write(
        format: Format,
        sample_format: SampleFormat,
        dither: bool,
        channels: &[Vec<f32>],
    ) -> Vec<u8> {
        let count = channels.len() as u16;
        let mut writer = Writer::new(
            Cursor::new(Vec::new()),
            format,
            sample_format,
            44_100,
            count,
        )
        .unwrap();
        if dither {
            writer = writer.with_dither();
        }
        let frames = channels[0].len();
        let mut done = 0;
        for piece in [1, 700, 4096, 13].into_iter().cycle() {
            let piece = piece.min(frames - done);
            let mut samples = Vec::with_capacity(piece * channels.len());
            for i in done..done + piece {
                samples.extend(channels.iter().map(|c| c[i]));
            }
            writer.write(&samples).unwrap();
            done += piece;
            if done == frames {
                break;
            }
        }
        assert_eq!(writer.frames(), frames as u64);
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn written_files_read_back() {
        let signal = signal(2, 9001);
        for format in [Format::Wav, Format::Aiff, Format::Flac] {
            for sample_format in FORMATS {
                if format == Format::Flac && sample_format.bits() > 24 {
                    continue;
                }
                let bytes = write(format, sample_format, false, &signal);
                let mut decoder = decode(bytes);
                assert_eq!(
                    decoder.info(),
                    Info {
                        format,
                        channels: 2,
                        sample_rate: 44_100,
                        frames: 9001
                    }
                );
                let back = decoder.read_all().unwrap();
                let step = step(sample_format);
                for (c, channel) in back.iter().enumerate() {
                    for (i, &s) in channel.iter().enumerate() {
                        // Integers clip a step below full scale.
                        let error = (s - signal[c][i].min(1.0 - step)).abs();
                        assert!(
                            error <= step / 2.0 + f32::EPSILON,
                            "{format:?} {sample_format:?}: {error}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn they_match_files_written_whole() {
        let signal = signal(3, 1001);
        let views: Vec<&[f32]> = signal.iter().map(Vec::as_slice).collect();
        for sample_format in FORMATS {
            let mut whole = Vec::new();
            super::super::write_wav(&mut whole, 44_100, sample_format, &views).unwrap();
            assert_eq!(write(Format::Wav, sample_format, false, &signal), whole);
            let mut whole = Vec::new();
            super::super::write_aiff(&mut whole, 44_100, sample_format, &views).unwrap();
            assert_eq!(write(Format::Aiff, sample_format, false, &signal), whole);
        }
    }

    #[test]
    fn dither_is_a_step_of_noise_at_most_and_silence_stays_silent() {
        let quiet: Vec<f32> = (0..20_000)
            .map(|i| if i < 10_000 { 0.25 / 32_768.0 } else { 0.0 })
            .collect();
        let channels = vec![quiet.clone()];
        let plain = decode(write(Format::Wav, SampleFormat::Int16, false, &channels))
            .read_all()
            .unwrap();
        let dithered = decode(write(Format::Wav, SampleFormat::Int16, true, &channels))
            .read_all()
            .unwrap();
        // A quarter of a step rounds away without dither, and comes through
        // on average with it.
        assert!(plain[0].iter().all(|&s| s == 0.0));
        let steps: Vec<f32> = dithered[0].iter().map(|s| s * 32_768.0).collect();
        assert!(steps.iter().all(|s| s.abs() <= 1.0));
        let mean = steps[..10_000].iter().sum::<f32>() / 10_000.0;
        assert!((mean - 0.25).abs() < 0.05, "{mean}");
        assert!(steps[10_000..].iter().all(|&s| s == 0.0));
        // The same samples dither the same way every time.
        let again = decode(write(Format::Wav, SampleFormat::Int16, true, &channels))
            .read_all()
            .unwrap();
        assert_eq!(again, dithered);
    }

    #[test]
    fn floats_go_over_full_scale_and_integers_clip() {
        let loud = vec![vec![1.5, -1.5, 0.5]];
        let floats = decode(write(Format::Aiff, SampleFormat::Float32, true, &loud))
            .read_all()
            .unwrap();
        assert_eq!(floats, loud);
        let integers = decode(write(Format::Flac, SampleFormat::Int24, false, &loud))
            .read_all()
            .unwrap();
        assert_eq!(integers[0], [8_388_607.0 / 8_388_608.0, -1.0, 0.5]);
    }

    #[test]
    fn flac_takes_integers_of_up_to_24_bits_only() {
        for sample_format in FORMATS {
            let writer = Writer::new(
                Cursor::new(Vec::new()),
                Format::Flac,
                sample_format,
                48_000,
                2,
            );
            if sample_format.bits() > 24 {
                assert!(
                    matches!(writer, Err(AudioError::Unsupported(_))),
                    "{sample_format:?}"
                );
            } else {
                assert!(writer.is_ok(), "{sample_format:?}");
            }
        }
    }

    #[test]
    fn an_odd_number_of_bytes_is_padded() {
        let odd = vec![vec![0.1, 0.2, 0.3]];
        for format in [Format::Wav, Format::Aiff] {
            let bytes = write(format, SampleFormat::Int24, false, &odd);
            assert_eq!(bytes.len() % 2, 0, "{format:?}");
            let decoder = Decoder::new(Box::new(Cursor::new(bytes))).unwrap();
            assert_eq!(decoder.info().frames, 3);
        }
    }
}
