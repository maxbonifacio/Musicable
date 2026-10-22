//! Audio files: reading WAV, AIFF and FLAC, and writing WAV and AIFF.
//!
//! A [`Decoder`] reads any frame of a file on request, so a clip can play
//! from anywhere in it and a stream can read ahead of the playhead in
//! chunks. Samples come out as floats from −1.0 to 1.0, interleaved.
//!
//! Formats read:
//!
//! - WAV: 8-bit unsigned, 16-, 24- and 32-bit integer and 32- and 64-bit
//!   float PCM, plain or `WAVE_FORMAT_EXTENSIBLE`. A data chunk whose size
//!   runs past the end of the file, as a recorder that didn't finish leaves
//!   it, ends with the file.
//! - AIFF and AIFF-C: 8-, 16-, 24- and 32-bit integer PCM, big-endian or
//!   little-endian (`sowt`), and 32- and 64-bit float (`fl32`, `fl64`).
//! - FLAC: 8-, 12-, 16-, 20- and 24-bit, in blocks of any size. Reading
//!   from anywhere in a file searches it for the frame to start from.
//!
//! [`write_wav`] and [`write_aiff`] write 16- and 24-bit integer and 32-bit
//! float files, and a [`WavWriter`] writes a 32-bit float WAV file as its
//! samples come, as a recording does. Writing FLAC comes with exporting.

mod aiff;
mod flac;
mod wav;

pub use wav::WavWriter;

use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// A file format this module reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Wav,
    Aiff,
    Flac,
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
        let (info, inner) = if magic.starts_with(b"fLaC") {
            let flac = flac::Flac::new(source)?;
            (flac.info(), Inner::Flac(Box::new(flac)))
        } else if magic.len() == 12 && &magic[..4] == b"RIFF" && &magic[8..] == b"WAVE" {
            let pcm = wav::open(source)?;
            (pcm.info(Format::Wav), Inner::Pcm(pcm))
        } else if magic.len() == 12
            && &magic[..4] == b"FORM"
            && (&magic[8..] == b"AIFF" || &magic[8..] == b"AIFC")
        {
            let pcm = aiff::open(source)?;
            (pcm.info(Format::Aiff), Inner::Pcm(pcm))
        } else {
            return Err(AudioError::UnknownFormat);
        };
        if info.channels == 0 || info.sample_rate == 0 {
            return invalid("no channels or no sample rate");
        }
        Ok(Self { info, inner })
    }

    pub fn info(&self) -> Info {
        self.info
    }

    /// Reads frames from frame `start` on into `out`, interleaved, as many
    /// as fit or as the file has left, and returns how many.
    pub fn read(&mut self, start: u64, out: &mut [f32]) -> Result<usize, AudioError> {
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

/// How a PCM file stores a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
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

impl Encoding {
    fn bytes(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::I16 { .. } => 2,
            Self::I24 { .. } => 3,
            Self::I32 { .. } | Self::F32 { .. } => 4,
            Self::F64 { .. } => 8,
        }
    }

    fn decode(self, b: &[u8]) -> f32 {
        fn order<const N: usize>(b: &[u8], big: bool) -> [u8; N] {
            let mut bytes: [u8; N] = b[..N].try_into().expect("a whole sample");
            if big {
                bytes.reverse();
            }
            bytes
        }
        match self {
            Self::U8 => (f32::from(b[0]) - 128.0) / 128.0,
            Self::I8 => f32::from(b[0] as i8) / 128.0,
            Self::I16 { big } => f32::from(i16::from_le_bytes(order(b, big))) / 32_768.0,
            Self::I24 { big } => {
                let [x, y, z] = order::<3>(b, big);
                // Into the top three bytes, so the sign comes along.
                let sample = i32::from_le_bytes([0, x, y, z]) >> 8;
                sample as f32 / 8_388_608.0
            }
            Self::I32 { big } => {
                (f64::from(i32::from_le_bytes(order(b, big))) / 2_147_483_648.0) as f32
            }
            Self::F32 { big } => f32::from_le_bytes(order(b, big)),
            Self::F64 { big } => f64::from_le_bytes(order(b, big)) as f32,
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
    encoding: Encoding,
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
        encoding: Encoding,
    ) -> Result<Self, AudioError> {
        if channels == 0 {
            return invalid("no channels");
        }
        let length = source.seek(SeekFrom::End(0))?;
        let frame = (channels * encoding.bytes()) as u64;
        let available = length.saturating_sub(start) / frame;
        Ok(Self {
            source,
            start,
            frames: (bytes / frame).min(available),
            channels,
            sample_rate,
            encoding,
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

    fn read(&mut self, start: u64, out: &mut [f32]) -> Result<(), AudioError> {
        let size = self.encoding.bytes();
        let offset = self.start + start * (self.channels * size) as u64;
        if self.position != Some(offset) {
            self.source.seek(SeekFrom::Start(offset))?;
        }
        self.bytes.resize(out.len() * size, 0);
        self.position = None;
        self.source.read_exact(&mut self.bytes)?;
        self.position = Some(offset + self.bytes.len() as u64);
        for (sample, bytes) in out.iter_mut().zip(self.bytes.chunks_exact(size)) {
            *sample = self.encoding.decode(bytes);
        }
        Ok(())
    }
}

/// How [`write_wav`] and [`write_aiff`] store samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 16-bit integers, rounded and clipped at full scale.
    Int16,
    /// 24-bit integers, rounded and clipped at full scale.
    Int24,
    /// 32-bit floats, exactly.
    Float32,
}

impl SampleFormat {
    fn bytes(self) -> usize {
        match self {
            Self::Int16 => 2,
            Self::Int24 => 3,
            Self::Float32 => 4,
        }
    }

    /// One sample, little-endian.
    fn encode(self, sample: f32, out: &mut Vec<u8>) {
        let int = |scale: f64| {
            (f64::from(sample) * scale)
                .round()
                .clamp(-scale, scale - 1.0) as i32
        };
        match self {
            Self::Int16 => out.extend_from_slice(&(int(32_768.0) as i16).to_le_bytes()),
            Self::Int24 => out.extend_from_slice(&int(8_388_608.0).to_le_bytes()[..3]),
            Self::Float32 => out.extend_from_slice(&sample.to_le_bytes()),
        }
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

    /// The largest step `format` rounds to.
    fn step(format: SampleFormat) -> f32 {
        match format {
            SampleFormat::Int16 => 1.0 / 32_768.0,
            SampleFormat::Int24 => 1.0 / 8_388_608.0,
            SampleFormat::Float32 => 0.0,
        }
    }

    #[test]
    fn written_files_read_back() {
        let signal = signal(3, 5000);
        for format in [
            SampleFormat::Int16,
            SampleFormat::Int24,
            SampleFormat::Float32,
        ] {
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

    #[test]
    fn other_files_are_refused() {
        let error = Decoder::new(Box::new(Cursor::new(b"ID3 not audio".to_vec())));
        assert!(matches!(error, Err(AudioError::UnknownFormat)));
        let empty = Decoder::new(Box::new(Cursor::new(Vec::new())));
        assert!(matches!(empty, Err(AudioError::UnknownFormat)));
    }
}
