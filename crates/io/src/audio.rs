//! Audio files: reading and writing WAV, AIFF and FLAC.
//!
//! A [`Decoder`] reads any frame of a file on request, so a clip can play
//! from anywhere in it and a stream can read ahead of the playhead in
//! chunks. Samples come out as floats from −1.0 to 1.0, interleaved: 32-bit
//! floats, as playback takes them, or 64-bit ones, which hold every
//! format's samples exactly, 32-bit integers and 64-bit floats too
//! ([`Decoder::read_f64`]). [`Decoder::encoding`] says how the file stores
//! them.
//!
//! Formats read:
//!
//! - WAV: 8-bit unsigned, 16-, 24- and 32-bit integer and 32- and 64-bit
//!   float PCM, plain or `WAVE_FORMAT_EXTENSIBLE`. A data chunk whose size
//!   runs past the end of the file, as a recorder that didn't finish leaves
//!   it, ends with the file. A loop's tempo comes from its ACID chunk.
//! - AIFF and AIFF-C: 8-, 16-, 24- and 32-bit integer PCM, big-endian or
//!   little-endian (`sowt`), and 32- and 64-bit float (`fl32`, `fl64`).
//! - FLAC: 8-, 12-, 16-, 20- and 24-bit, in blocks of any size. Reading
//!   from anywhere in a file searches it for the frame to start from.
//!
//! [`write_wav`] and [`write_aiff`] write 8-, 16-, 24- and 32-bit integer
//! and 32- and 64-bit float files, and a [`WavWriter`] writes a 32-bit
//! float WAV file as its samples come, as a recording does. A [`Writer`]
//! writes any of the three formats as the samples come, as an export does,
//! with dither if asked: FLAC in 8-, 16- and 24-bit integers, with an
//! encoder of our own. Written from 64-bit floats a file's own samples,
//! read with [`Decoder::read_f64`], come out exactly as they were, in a
//! [`SampleFormat`] that [holds](SampleFormat::holds) its encoding.
//!
//! [`SamplesMd5`] hashes samples as a FLAC file's STREAMINFO does, so files
//! of the same audio in different formats hash the same.

mod aiff;
mod flac;
mod flac_encoder;
mod md5;
mod wav;
mod writer;

pub use wav::WavWriter;
pub use writer::Writer;

use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// A file format this module reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Wav,
    Aiff,
    Flac,
}

impl Format {
    /// The extension files of the format usually have.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Aiff => "aif",
            Self::Flac => "flac",
        }
    }
}

/// How a file stores its samples, as [`Decoder::encoding`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Integers of this many bits, from 4 to 32. WAV files store 8-bit
    /// samples unsigned, which reads the same.
    Int(u8),
    Float32,
    Float64,
}

impl Encoding {
    /// Its name, such as `int24` or `float32`.
    pub fn name(self) -> String {
        match self {
            Self::Int(bits) => format!("int{bits}"),
            Self::Float32 => "float32".to_owned(),
            Self::Float64 => "float64".to_owned(),
        }
    }

    /// The narrowest sample format that holds its samples exactly.
    pub fn format(self) -> SampleFormat {
        match self {
            Self::Int(..=8) => SampleFormat::Int8,
            Self::Int(..=16) => SampleFormat::Int16,
            Self::Int(..=24) => SampleFormat::Int24,
            Self::Int(_) => SampleFormat::Int32,
            Self::Float32 => SampleFormat::Float32,
            Self::Float64 => SampleFormat::Float64,
        }
    }
}

/// What an audio file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub format: Format,
    pub channels: usize,
    pub sample_rate: u32,
    /// The file's length, in frames.
    pub frames: u64,
}

#[derive(Debug)]
pub enum AudioError {
    Io(io::Error),
    /// Not a WAV, AIFF or FLAC file.
    UnknownFormat,
    /// A file this module doesn't read, such as a compressed WAV.
    Unsupported(String),
    /// A file that breaks its format's rules.
    Invalid(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::UnknownFormat => write!(f, "not a WAV, AIFF or FLAC file"),
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
            Self::Invalid(what) => write!(f, "invalid file: {what}"),
        }
    }
}

impl std::error::Error for AudioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for AudioError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<AudioError> for io::Error {
    fn from(error: AudioError) -> Self {
        match error {
            AudioError::Io(error) => error,
            other => io::Error::new(io::ErrorKind::InvalidData, other),
        }
    }
}

fn invalid<T>(what: impl Into<String>) -> Result<T, AudioError> {
    Err(AudioError::Invalid(what.into()))
}

fn unsupported<T>(what: impl Into<String>) -> Result<T, AudioError> {
    Err(AudioError::Unsupported(what.into()))
}

/// Something a decoder can read from: a file, or bytes in memory.
pub trait Source: Read + Seek + Send {}

impl<T: Read + Seek + Send> Source for T {}

/// Reads an audio file, from any frame.
pub struct Decoder {
    info: Info,
    tempo: Option<f64>,
    inner: Inner,
}

enum Inner {
    Pcm(Pcm),
    Flac(Box<flac::Flac>),
}

impl Decoder {
    /// Opens the file at `path`, finding its format from what's in it, not
    /// from its name.
    pub fn open(path: &Path) -> Result<Self, AudioError> {
        Self::new(Box::new(File::open(path)?))
    }

    /// Reads a file from `source`.
    pub fn new(mut source: Box<dyn Source>) -> Result<Self, AudioError> {
        let mut magic = [0; 12];
        let read = read_up_to(source.as_mut(), &mut magic)?;
        source.seek(SeekFrom::Start(0))?;
        let magic = &magic[..read];
        let (info, tempo, inner) = if magic.starts_with(b"fLaC") {
            let flac = flac::Flac::new(source)?;
            (flac.info(), None, Inner::Flac(Box::new(flac)))
        } else if magic.len() == 12 && &magic[..4] == b"RIFF" && &magic[8..] == b"WAVE" {
            let (pcm, tempo) = wav::open(source)?;
            (pcm.info(Format::Wav), tempo, Inner::Pcm(pcm))
        } else if magic.len() == 12
            && &magic[..4] == b"FORM"
            && (&magic[8..] == b"AIFF" || &magic[8..] == b"AIFC")
        {
            let pcm = aiff::open(source)?;
            (pcm.info(Format::Aiff), None, Inner::Pcm(pcm))
        } else {
            return Err(AudioError::UnknownFormat);
        };
        if info.channels == 0 || info.sample_rate == 0 {
            return invalid("no channels or no sample rate");
        }
        Ok(Self { info, tempo, inner })
    }

    pub fn info(&self) -> Info {
        self.info
    }

    /// The tempo the file's loop data gives, in quarter notes a minute, if
    /// it has some and isn't a one-shot: a WAV file's ACID chunk, so far.
    pub fn tempo(&self) -> Option<f64> {
        self.tempo
    }

    /// How the file stores its samples.
    pub fn encoding(&self) -> Encoding {
        match &self.inner {
            Inner::Pcm(pcm) => pcm.layout.encoding(),
            Inner::Flac(flac) => Encoding::Int(flac.bits()),
        }
    }

    /// Reads frames from frame `start` on into `out`, interleaved, as many
    /// as fit or as the file has left, and returns how many.
    pub fn read(&mut self, start: u64, out: &mut [f32]) -> Result<usize, AudioError> {
        self.read_into(start, out)
    }

    /// Reads as [`read`](Self::read) does, into 64-bit floats, which hold
    /// every sample exactly as the file has it.
    pub fn read_f64(&mut self, start: u64, out: &mut [f64]) -> Result<usize, AudioError> {
        self.read_into(start, out)
    }

    fn read_into<S: Sample>(&mut self, start: u64, out: &mut [S]) -> Result<usize, AudioError> {
        let channels = self.info.channels;
        let frames = (out.len() / channels).min(self.info.frames.saturating_sub(start) as usize);
        if frames == 0 {
            return Ok(0);
        }
        let out = &mut out[..frames * channels];
        match &mut self.inner {
            Inner::Pcm(pcm) => pcm.read(start, out),
            Inner::Flac(flac) => flac.read(start, out),
        }?;
        Ok(frames)
    }

    /// Reads the whole file, a channel at a time.
    pub fn read_all(&mut self) -> Result<Vec<Vec<f32>>, AudioError> {
        let channels = self.info.channels;
        let frames = usize::try_from(self.info.frames)
            .map_err(|_| AudioError::Unsupported("a file too long to load".into()))?;
        let mut planar: Vec<Vec<f32>> = (0..channels).map(|_| Vec::with_capacity(frames)).collect();
        let mut buffer = vec![0.0; channels * 16_384];
        let mut at = 0;
        while at < self.info.frames {
            let read = self.read(at, &mut buffer)?;
            if read == 0 {
                break;
            }
            for frame in buffer[..read * channels].chunks_exact(channels) {
                for (channel, &sample) in planar.iter_mut().zip(frame) {
                    channel.push(sample);
                }
            }
            at += read as u64;
        }
        Ok(planar)
    }
}

impl fmt::Debug for Decoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decoder")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// Reads as much of `buffer` as `source` has, and returns how much.
fn read_up_to(source: &mut dyn Source, buffer: &mut [u8]) -> io::Result<usize> {
    let mut done = 0;
    while done < buffer.len() {
        match source.read(&mut buffer[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

/// What samples are read as: 32-bit floats, as playback takes them, or
/// 64-bit ones, which hold every format's exactly. Each sample is read as
/// a 64-bit float first, which converts to the nearest 32-bit one: the
/// same one reading it as a 32-bit float straight away gives.
trait Sample: Copy {
    fn from_f64(sample: f64) -> Self;
}

impl Sample for f32 {
    #[inline]
    fn from_f64(sample: f64) -> Self {
        sample as f32
    }
}

impl Sample for f64 {
    #[inline]
    fn from_f64(sample: f64) -> Self {
        sample
    }
}

/// How a PCM file lays out a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Unsigned 8-bit, as WAV has it.
    U8,
    /// Signed 8-bit, as AIFF has it.
    I8,
    I16 {
        big: bool,
    },
    I24 {
        big: bool,
    },
    I32 {
        big: bool,
    },
    F32 {
        big: bool,
    },
    F64 {
        big: bool,
    },
}

impl Layout {
    fn bytes(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::I16 { .. } => 2,
            Self::I24 { .. } => 3,
            Self::I32 { .. } | Self::F32 { .. } => 4,
            Self::F64 { .. } => 8,
        }
    }

    fn encoding(self) -> Encoding {
        match self {
            Self::U8 | Self::I8 => Encoding::Int(8),
            Self::I16 { .. } => Encoding::Int(16),
            Self::I24 { .. } => Encoding::Int(24),
            Self::I32 { .. } => Encoding::Int(32),
            Self::F32 { .. } => Encoding::Float32,
            Self::F64 { .. } => Encoding::Float64,
        }
    }

    /// The sample in `b`, exactly: integers divided by a power of two.
    #[inline]
    fn decode(self, b: &[u8]) -> f64 {
        fn order<const N: usize>(b: &[u8], big: bool) -> [u8; N] {
            let mut bytes: [u8; N] = b[..N].try_into().expect("a whole sample");
            if big {
                bytes.reverse();
            }
            bytes
        }
        match self {
            Self::U8 => (f64::from(b[0]) - 128.0) / 128.0,
            Self::I8 => f64::from(b[0] as i8) / 128.0,
            Self::I16 { big } => f64::from(i16::from_le_bytes(order(b, big))) / 32_768.0,
            Self::I24 { big } => {
                let [x, y, z] = order::<3>(b, big);
                // Into the top three bytes, so the sign comes along.
                let sample = i32::from_le_bytes([0, x, y, z]) >> 8;
                f64::from(sample) / 8_388_608.0
            }
            Self::I32 { big } => f64::from(i32::from_le_bytes(order(b, big))) / 2_147_483_648.0,
            Self::F32 { big } => f64::from(f32::from_le_bytes(order(b, big))),
            Self::F64 { big } => f64::from_le_bytes(order(b, big)),
        }
    }
}

/// An uncompressed file's samples: where they start and how they're
/// stored.
struct Pcm {
    source: Box<dyn Source>,
    /// The first frame's byte offset in the file.
    start: u64,
    frames: u64,
    channels: usize,
    sample_rate: u32,
    layout: Layout,
    /// The byte offset the next read starts from, so reading straight on
    /// doesn't seek.
    position: Option<u64>,
    bytes: Vec<u8>,
}

impl Pcm {
    /// The samples from byte `start` of `source`: `bytes` bytes of them, or
    /// as many as the file has, whichever is less.
    fn new(
        mut source: Box<dyn Source>,
        start: u64,
        bytes: u64,
        channels: usize,
        sample_rate: u32,
        layout: Layout,
    ) -> Result<Self, AudioError> {
        if channels == 0 {
            return invalid("no channels");
        }
        let length = source.seek(SeekFrom::End(0))?;
        let frame = (channels * layout.bytes()) as u64;
        let available = length.saturating_sub(start) / frame;
        Ok(Self {
            source,
            start,
            frames: (bytes / frame).min(available),
            channels,
            sample_rate,
            layout,
            position: None,
            bytes: Vec::new(),
        })
    }

    fn info(&self, format: Format) -> Info {
        Info {
            format,
            channels: self.channels,
            sample_rate: self.sample_rate,
            frames: self.frames,
        }
    }

    fn read<S: Sample>(&mut self, start: u64, out: &mut [S]) -> Result<(), AudioError> {
        let size = self.layout.bytes();
        let offset = self.start + start * (self.channels * size) as u64;
        if self.position != Some(offset) {
            self.source.seek(SeekFrom::Start(offset))?;
        }
        self.bytes.resize(out.len() * size, 0);
        self.position = None;
        self.source.read_exact(&mut self.bytes)?;
        self.position = Some(offset + self.bytes.len() as u64);
        for (sample, bytes) in out.iter_mut().zip(self.bytes.chunks_exact(size)) {
            *sample = S::from_f64(self.layout.decode(bytes));
        }
        Ok(())
    }
}

/// How [`write_wav`], [`write_aiff`] and a [`Writer`] store samples.
/// Integers are rounded and clipped at full scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 8-bit integers: unsigned in WAV files, as WAV has them, and signed in
    /// AIFF and FLAC files.
    Int8,
    Int16,
    Int24,
    /// 32-bit integers. FLAC files don't take them.
    Int32,
    /// 32-bit floats, which go over full scale rather than clip. FLAC files
    /// don't take them.
    Float32,
    /// 64-bit floats. FLAC files don't take them.
    Float64,
}

impl SampleFormat {
    /// Its name, as [`Encoding::name`] has it, such as `int24`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Int8 => "int8",
            Self::Int16 => "int16",
            Self::Int24 => "int24",
            Self::Int32 => "int32",
            Self::Float32 => "float32",
            Self::Float64 => "float64",
        }
    }

    /// The bits of each sample.
    pub fn bits(self) -> u32 {
        match self {
            Self::Int8 => 8,
            Self::Int16 => 16,
            Self::Int24 => 24,
            Self::Int32 | Self::Float32 => 32,
            Self::Float64 => 64,
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, Self::Float32 | Self::Float64)
    }

    /// The encoding of a file written in it.
    pub fn encoding(self) -> Encoding {
        match self {
            Self::Float32 => Encoding::Float32,
            Self::Float64 => Encoding::Float64,
            int => Encoding::Int(int.bits() as u8),
        }
    }

    /// Whether it holds every sample of `encoding` exactly: integers in
    /// integers as wide or wider, or in floats with as many bits of
    /// mantissa, and floats in floats as wide or wider.
    pub fn holds(self, encoding: Encoding) -> bool {
        match (self, encoding) {
            (Self::Float64, _) => true,
            (Self::Float32, Encoding::Float32) => true,
            (Self::Float32, Encoding::Int(bits)) => bits <= 24,
            (_, Encoding::Float32 | Encoding::Float64) => false,
            (int, Encoding::Int(bits)) => u32::from(bits) <= int.bits(),
        }
    }

    fn bytes(self) -> usize {
        self.bits() as usize / 8
    }

    /// One sample, little-endian, 8-bit integers signed.
    fn encode(self, sample: f64, out: &mut Vec<u8>) {
        match self {
            Self::Float32 => out.extend_from_slice(&(sample as f32).to_le_bytes()),
            Self::Float64 => out.extend_from_slice(&sample.to_le_bytes()),
            int => {
                let bits = int.bits();
                let integer = quantize(sample, bits, None);
                out.extend_from_slice(&integer.to_le_bytes()[..bits as usize / 8]);
            }
        }
    }
}

/// `sample` as a `bits`-bit integer: rounded, after dither from `dither` if
/// it's there and the sample isn't silent, and clipped at full scale.
fn quantize(sample: f64, bits: u32, dither: Option<&mut writer::Random>) -> i32 {
    let scale = f64::from(1u32 << (bits - 1));
    let mut scaled = sample * scale;
    if let Some(random) = dither
        && sample != 0.0
    {
        // The difference of two uniform numbers is triangular.
        scaled += random.uniform() - random.uniform();
    }
    scaled.round().clamp(-scale, scale - 1.0) as i32
}

/// The MD5 of samples as a FLAC file's STREAMINFO keeps it: each sample a
/// signed integer of its encoding's bits, little-endian in whole bytes, the
/// channels interleaved. Files of the same audio in WAV, AIFF and FLAC hash
/// the same. Floats, which FLAC doesn't hold, are hashed as their
/// little-endian bytes.
#[derive(Debug)]
pub struct SamplesMd5 {
    md5: md5::Md5,
    encoding: Encoding,
    bytes: Vec<u8>,
}

impl SamplesMd5 {
    /// Hashes samples of `encoding`.
    pub fn new(encoding: Encoding) -> Self {
        Self {
            md5: md5::Md5::new(),
            encoding,
            bytes: Vec::new(),
        }
    }

    /// Adds `samples`, as [`Decoder::read_f64`] reads them.
    pub fn update(&mut self, samples: &[f64]) {
        self.bytes.clear();
        for &sample in samples {
            match self.encoding {
                Encoding::Int(bits) => {
                    let integer = quantize(sample, u32::from(bits), None);
                    let bytes = usize::from(bits).div_ceil(8);
                    self.bytes
                        .extend_from_slice(&integer.to_le_bytes()[..bytes]);
                }
                Encoding::Float32 => self.bytes.extend_from_slice(&(sample as f32).to_le_bytes()),
                Encoding::Float64 => self.bytes.extend_from_slice(&sample.to_le_bytes()),
            }
        }
        self.md5.update(&self.bytes);
    }

    /// The hash, as 32 hexadecimal digits.
    pub fn finish(self) -> String {
        let digest = self.md5.finish();
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// Checks the channels a writer gets: at least one, all the same length.
fn frames_of(channels: &[&[f32]]) -> io::Result<usize> {
    let frames = channels.first().map_or(0, |c| c.len());
    if channels.is_empty() || channels.iter().any(|c| c.len() != frames) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "audio needs at least one channel, and channels of the same length",
        ));
    }
    Ok(frames)
}

/// Writes a WAV file of `channels`, one slice per channel, at `sample_rate`.
pub fn write_wav(
    out: &mut impl Write,
    sample_rate: u32,
    format: SampleFormat,
    channels: &[&[f32]],
) -> io::Result<()> {
    wav::write(out, sample_rate, format, channels)
}

/// Writes an AIFF file of `channels`, or an AIFF-C file for floats.
pub fn write_aiff(
    out: &mut impl Write,
    sample_rate: u32,
    format: SampleFormat,
    channels: &[&[f32]],
) -> io::Result<()> {
    aiff::write(out, sample_rate, format, channels)
}

/// [`write_wav`] to a new file at `path`, replacing any there.
pub fn save_wav(
    path: &Path,
    sample_rate: u32,
    format: SampleFormat,
    channels: &[&[f32]],
) -> io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    write_wav(&mut out, sample_rate, format, channels)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// A test signal: a different ramp in each channel, spanning full scale.
    pub(super) fn signal(channels: usize, frames: usize) -> Vec<Vec<f32>> {
        (0..channels)
            .map(|c| {
                (0..frames)
                    .map(|i| ((i * 7 + c * 1000) % 2000) as f32 / 1000.0 - 1.0)
                    .collect()
            })
            .collect()
    }

    pub(super) fn decode(bytes: Vec<u8>) -> Decoder {
        Decoder::new(Box::new(Cursor::new(bytes))).unwrap()
    }

    fn views(channels: &[Vec<f32>]) -> Vec<&[f32]> {
        channels.iter().map(Vec::as_slice).collect()
    }

    /// Every sample format.
    pub(super) const FORMATS: [SampleFormat; 6] = [
        SampleFormat::Int8,
        SampleFormat::Int16,
        SampleFormat::Int24,
        SampleFormat::Int32,
        SampleFormat::Float32,
        SampleFormat::Float64,
    ];

    /// The largest step `format` rounds to.
    pub(super) fn step(format: SampleFormat) -> f32 {
        match format {
            SampleFormat::Float32 | SampleFormat::Float64 => 0.0,
            int => 1.0 / (1u64 << (int.bits() - 1)) as f32,
        }
    }

    #[test]
    fn written_files_read_back() {
        let signal = signal(3, 5000);
        for format in FORMATS {
            for aiff in [false, true] {
                let mut bytes = Vec::new();
                let write = if aiff { write_aiff } else { write_wav };
                write(&mut bytes, 44_100, format, &views(&signal)).unwrap();
                let mut decoder = decode(bytes);
                let info = decoder.info();
                let expected = if aiff { Format::Aiff } else { Format::Wav };
                assert_eq!(
                    info,
                    Info {
                        format: expected,
                        channels: 3,
                        sample_rate: 44_100,
                        frames: 5000
                    }
                );
                let back = decoder.read_all().unwrap();
                for (c, channel) in back.iter().enumerate() {
                    for (i, &s) in channel.iter().enumerate() {
                        let error = (s - signal[c][i]).abs();
                        assert!(error <= step(format), "{format:?}, AIFF {aiff}: {error}");
                    }
                }
            }
        }
    }

    #[test]
    fn reads_start_anywhere() {
        let signal = signal(2, 3000);
        let mut bytes = Vec::new();
        write_wav(&mut bytes, 48_000, SampleFormat::Float32, &views(&signal)).unwrap();
        let mut decoder = decode(bytes);
        let mut out = vec![0.0; 2 * 100];
        for start in [2900, 0, 1234, 2999] {
            let read = decoder.read(start, &mut out).unwrap();
            assert_eq!(read, 100.min(3000 - start as usize));
            for i in 0..read {
                assert_eq!(out[2 * i], signal[0][start as usize + i]);
                assert_eq!(out[2 * i + 1], signal[1][start as usize + i]);
            }
        }
        assert_eq!(decoder.read(3000, &mut out).unwrap(), 0);
    }

    #[test]
    fn integers_round_and_clip_at_full_scale() {
        let mut bytes = Vec::new();
        let loud = [1.5, -1.5, 1.0, -1.0, 0.5];
        write_wav(&mut bytes, 48_000, SampleFormat::Int16, &[&loud]).unwrap();
        let back = decode(bytes).read_all().unwrap();
        assert_eq!(
            back[0],
            [32_767.0 / 32_768.0, -1.0, 32_767.0 / 32_768.0, -1.0, 0.5]
        );
    }

    /// Samples on `format`'s steps, from full scale down, or floats that
    /// narrower floats don't hold.
    pub(super) fn exact_samples(format: SampleFormat) -> Vec<f64> {
        match format {
            SampleFormat::Float64 => vec![0.1, -1.0 / 3.0, 1e-300, 2.5, -7.25, 0.0],
            SampleFormat::Float32 => [0.1f32, -1.0 / 3.0, 1e-30, 2.5, -7.25, 0.0]
                .map(f64::from)
                .to_vec(),
            int => {
                let scale = (1u64 << (int.bits() - 1)) as f64;
                let odd = (scale / 3.0).floor();
                [-scale, scale - 1.0, 1.0, -1.0, 0.0, odd, -odd]
                    .map(|step| step / scale)
                    .to_vec()
            }
        }
    }

    #[test]
    fn samples_read_as_64_bit_floats_come_back_exactly() {
        for format in FORMATS {
            let left = exact_samples(format);
            let right: Vec<f64> = left.iter().rev().copied().collect();
            let samples: Vec<f64> = left
                .iter()
                .zip(&right)
                .flat_map(|(l, r)| [*l, *r])
                .collect();
            for container in [Format::Wav, Format::Aiff, Format::Flac] {
                let bits = format.bits();
                if container == Format::Flac && (format.is_float() || bits > 24) {
                    continue;
                }
                let out = Cursor::new(Vec::new());
                let mut writer = Writer::new(out, container, format, 44_100, 2).unwrap();
                writer.write_f64(&samples).unwrap();
                let mut decoder = decode(writer.finish().unwrap().into_inner());
                assert_eq!(decoder.encoding(), format.encoding(), "{container:?}");
                let mut back = vec![0.0; samples.len()];
                assert_eq!(decoder.read_f64(0, &mut back).unwrap(), left.len());
                assert_eq!(back, samples, "{format:?} in {container:?}");
            }
        }
    }

    #[test]
    fn formats_hold_encodings_as_wide_or_narrower() {
        assert_eq!(Encoding::Int(12).format(), SampleFormat::Int16);
        assert_eq!(Encoding::Int(20).format(), SampleFormat::Int24);
        assert_eq!(Encoding::Int(24).name(), "int24");
        for format in FORMATS {
            assert_eq!(format.encoding().format(), format);
            assert_eq!(format.encoding().name(), format.name());
            assert!(format.holds(format.encoding()));
            assert!(SampleFormat::Float64.holds(format.encoding()));
        }
        assert!(SampleFormat::Float32.holds(Encoding::Int(24)));
        assert!(!SampleFormat::Float32.holds(Encoding::Int(32)));
        assert!(!SampleFormat::Float32.holds(Encoding::Float64));
        assert!(SampleFormat::Int16.holds(Encoding::Int(12)));
        assert!(!SampleFormat::Int16.holds(Encoding::Int(20)));
        assert!(!SampleFormat::Int32.holds(Encoding::Float32));
    }

    #[test]
    fn samples_hash_the_same_in_every_format_and_as_flac_has_it() {
        let signal = signal(2, 3000);
        let views = views(&signal);
        let hash = |bytes: Vec<u8>| {
            let mut decoder = decode(bytes);
            let mut samples = vec![0.0; 2 * 3000];
            decoder.read_f64(0, &mut samples).unwrap();
            let mut md5 = SamplesMd5::new(decoder.encoding());
            // In two pieces, which hash as one.
            md5.update(&samples[..1001 * 2]);
            md5.update(&samples[1001 * 2..]);
            md5.finish()
        };
        let mut wav = Vec::new();
        write_wav(&mut wav, 44_100, SampleFormat::Int16, &views).unwrap();
        let mut aiff = Vec::new();
        write_aiff(&mut aiff, 44_100, SampleFormat::Int16, &views).unwrap();
        let out = Cursor::new(Vec::new());
        let mut writer = Writer::new(out, Format::Flac, SampleFormat::Int16, 44_100, 2).unwrap();
        let interleaved: Vec<f32> = (0..3000)
            .flat_map(|i| [signal[0][i], signal[1][i]])
            .collect();
        writer.write(&interleaved).unwrap();
        let flac = writer.finish().unwrap().into_inner();
        // STREAMINFO's MD5, after the stream marker, a block header and 18
        // bytes of STREAMINFO.
        let streaminfo: String = flac[26..42].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hash(wav), streaminfo);
        assert_eq!(hash(aiff), streaminfo);
        assert_eq!(hash(flac), streaminfo);
        assert_eq!(streaminfo.len(), 32);
    }

    #[test]
    fn other_files_are_refused() {
        let error = Decoder::new(Box::new(Cursor::new(b"ID3 not audio".to_vec())));
        assert!(matches!(error, Err(AudioError::UnknownFormat)));
        let empty = Decoder::new(Box::new(Cursor::new(Vec::new())));
        assert!(matches!(empty, Err(AudioError::UnknownFormat)));
    }
}
