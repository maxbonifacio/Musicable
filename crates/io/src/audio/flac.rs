//! FLAC files. Claxon decodes the frames; finding them is up to this
//! module: reading the metadata, and seeking by searching the file for
//! frame headers.
//!
//! FLAC has no index a reader can count on (seek tables are optional), so
//! seeking searches: it halves the stretch of the file the wanted sample
//! must be in until it's short, checking each frame it lands on by decoding
//! it, so a stray run of bytes that looks like a frame header doesn't lead
//! it astray, then decodes straight on from there.

use std::io::{self, Seek, SeekFrom};
use std::mem;

use claxon::frame::{Block, FrameReader};
use claxon::input::BufferedReader;

use super::{AudioError, Format, Info, Source, invalid, read_up_to, unsupported};

/// The most bytes a frame header takes: the sync code and three bytes of
/// fields, a coded number of up to seven bytes, a block size and a sample
/// rate of up to two bytes each, and the CRC.
const MAX_HEADER: usize = 16;
/// Bytes read at a time while looking for a frame.
const SCAN: usize = 16 * 1024;
/// Seeking narrows down where the wanted frame starts to this many bytes,
/// then decodes straight on.
const WINDOW: u64 = 64 * 1024;
/// A read this many samples or fewer past the last block decodes straight
/// on rather than seeking.
const STRAIGHT_ON: u64 = 1 << 16;

type Frames = FrameReader<BufferedReader<Box<dyn Source>>>;

/// What a FLAC stream is: what STREAMINFO says, and what its first frame
/// adds.
struct Stream {
    channels: usize,
    sample_rate: u32,
    bits: u32,
    /// The stream's length in samples, per channel.
    frames: u64,
    /// The largest block a frame can hold.
    max_block: u32,
    /// Whether frames carry their number rather than their first sample's,
    /// as they do in streams of blocks of one size.
    fixed: bool,
    /// The size of every block but the last in a stream that numbers its
    /// frames.
    block: u64,
}

impl Stream {
    /// Whether `bytes` start with a frame header that fits this stream: the
    /// sync code, no reserved values, the stream's channels, sample size and
    /// sample rate where the header gives them, a block no bigger than the
    /// stream's, a place within the stream, and a CRC that checks out.
    fn is_header(&self, bytes: &[u8]) -> bool {
        let sync = if self.fixed { 0xF8 } else { 0xF9 };
        if bytes.len() < 6 || bytes[0] != 0xFF || bytes[1] != sync || bytes[3] & 1 != 0 {
            return false;
        }
        let (size_code, rate_code) = (bytes[2] >> 4, bytes[2] & 0x0F);
        let channels = match bytes[3] >> 4 {
            n @ 0..=7 => usize::from(n) + 1,
            8..=10 => 2,
            _ => return false,
        };
        let bits = match (bytes[3] >> 1) & 0x07 {
            0 => self.bits,
            1 => 8,
            2 => 12,
            4 => 16,
            5 => 20,
            6 => 24,
            _ => return false,
        };
        let rate = match rate_code {
            0 => self.sample_rate,
            1 => 88_200,
            2 => 176_400,
            3 => 192_000,
            4 => 8_000,
            5 => 16_000,
            6 => 22_050,
            7 => 24_000,
            8 => 32_000,
            9 => 44_100,
            10 => 48_000,
            11 => 96_000,
            // At the end of the header.
            12..=14 => self.sample_rate,
            _ => return false,
        };
        if size_code == 0
            || channels != self.channels
            || bits != self.bits
            || rate != self.sample_rate
        {
            return false;
        }
        let Some((number, length)) = coded_number(&bytes[4..]) else {
            return false;
        };
        let mut at = 4 + length;
        let mut field = |length: usize| {
            let value = bytes.get(at..at + length)?;
            at += length;
            Some(value.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b)))
        };
        let block = match size_code {
            1 => Some(192),
            2..=5 => Some(576 << (size_code - 2)),
            6 => field(1).map(|n| n + 1),
            7 => field(2).map(|n| n + 1),
            _ => Some(256 << (size_code - 8)),
        };
        let rate = match rate_code {
            12 => field(1).map(|khz| khz * 1000),
            13 => field(2),
            14 => field(2).map(|tens| tens * 10),
            _ => Some(rate),
        };
        let (Some(block), Some(rate)) = (block, rate) else {
            return false;
        };
        let Some(&crc) = bytes.get(at) else {
            return false;
        };
        // Numbered frames have 31 bits for the number.
        let first = if !self.fixed {
            number
        } else if number <= 0x7FFF_FFFF {
            number * self.block
        } else {
            return false;
        };
        rate == self.sample_rate
            && block <= self.max_block
            && (self.frames == 0 || first < self.frames)
            && crc8(&bytes[..at]) == crc
    }
}

/// A number coded as FLAC frame headers code them: like UTF-8, but up to
/// 36 bits. Returns the number and how many bytes it takes.
fn coded_number(bytes: &[u8]) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let extra = match first.leading_ones() {
        0 => return Some((u64::from(first), 1)),
        // A byte that continues a number can't start one.
        1 | 8 => return None,
        ones => ones as usize - 1,
    };
    let mut number = u64::from(first & (0x3F >> extra));
    for &byte in bytes.get(1..=extra)? {
        if byte & 0xC0 != 0x80 {
            return None;
        }
        number = (number << 6) | u64::from(byte & 0x3F);
    }
    Some((number, extra + 1))
}

/// The CRC frame headers end with: polynomial x⁸ + x² + x + 1.
fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |crc, &byte| {
        (0..8).fold(crc ^ byte, |crc, _| {
            if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            }
        })
    })
}

fn decode_error(error: claxon::Error) -> AudioError {
    match error {
        claxon::Error::IoError(error) => AudioError::Io(error),
        claxon::Error::FormatError(what) => AudioError::Invalid(what.into()),
        claxon::Error::Unsupported(what) => AudioError::Unsupported(what.into()),
    }
}

/// Reads a FLAC file, from any sample.
pub(super) struct Flac {
    stream: Stream,
    /// The file, while claxon's reader doesn't have it.
    file: Option<Box<dyn Source>>,
    /// Claxon's reader, at the frame after `block`.
    frames: Option<Frames>,
    /// Where the first frame starts.
    first: u64,
    /// The file's length, in bytes.
    length: u64,
    /// How near seeking gets before decoding straight on: [`WINDOW`],
    /// smaller in tests.
    window: u64,
    /// The block decoded last, and its first sample.
    block: Option<(u64, Block)>,
    /// A buffer for claxon to decode into, while `block` doesn't hold it.
    spare: Vec<i32>,
    scan: Vec<u8>,
}

impl Flac {
    pub(super) fn new(mut file: Box<dyn Source>) -> Result<Self, AudioError> {
        let length = file.seek(SeekFrom::End(0))?;
        // Past "fLaC", to the metadata blocks.
        file.seek(SeekFrom::Start(4))?;
        let mut streaminfo = None;
        loop {
            let mut header = [0; 4];
            if read_up_to(file.as_mut(), &mut header)? < 4 {
                return invalid("the metadata runs past the end of the file");
            }
            let size = u32::from_be_bytes([0, header[1], header[2], header[3]]);
            match header[0] & 0x7F {
                0 if size >= 34 => {
                    let mut body = [0; 34];
                    if read_up_to(file.as_mut(), &mut body)? < 34 {
                        return invalid("a short STREAMINFO block");
                    }
                    file.seek(SeekFrom::Current(i64::from(size - 34)))?;
                    streaminfo = Some(body);
                }
                0 => return invalid("a short STREAMINFO block"),
                127 => return invalid("a metadata block of type 127"),
                _ => {
                    file.seek(SeekFrom::Current(i64::from(size)))?;
                }
            }
            if header[0] & 0x80 != 0 {
                break;
            }
        }
        let first = file.stream_position()?;
        let Some(info) = streaminfo else {
            return invalid("no STREAMINFO block");
        };
        if first > length {
            return invalid("the metadata runs past the end of the file");
        }
        let fields = u64::from_be_bytes(info[10..18].try_into().expect("eight bytes"));
        let bits = ((fields >> 36) & 0x1F) as u32 + 1;
        match bits {
            ..4 => return invalid(format!("{bits}-bit samples")),
            25.. => return unsupported(format!("{bits}-bit FLAC")),
            _ => {}
        }
        let max_block = u32::from(u16::from_be_bytes([info[2], info[3]]));
        let mut flac = Self {
            stream: Stream {
                channels: ((fields >> 41) & 0x07) as usize + 1,
                sample_rate: (fields >> 44) as u32,
                bits,
                frames: fields & 0xF_FFFF_FFFF,
                // Blocks are at least 16 samples, so this is a STREAMINFO
                // that doesn't know.
                max_block: if max_block < 16 { 65_536 } else { max_block },
                fixed: true,
                block: 0,
            },
            file: Some(file),
            frames: None,
            first,
            length,
            window: WINDOW,
            block: None,
            spare: Vec::new(),
            scan: vec![0; SCAN],
        };
        // The first frame says how the stream counts its frames, and, if
        // it numbers them, how long they are.
        let mut sync = [0; 2];
        let file = flac.file.as_mut().expect("the file, before any decoding");
        file.seek(SeekFrom::Start(first))?;
        if read_up_to(file.as_mut(), &mut sync)? < 2 {
            return match flac.stream.frames {
                0 => Ok(flac),
                _ => invalid("no frames"),
            };
        }
        if sync[0] != 0xFF || sync[1] & 0xFE != 0xF8 {
            return invalid("no frame after the metadata");
        }
        flac.stream.fixed = sync[1] == 0xF8;
        flac.reader_at(first)?;
        if flac.next()?.is_none() {
            return invalid("no frames");
        }
        let (_, block) = flac.block.as_ref().expect("the first frame");
        flac.stream.block = u64::from(block.duration());
        if flac.stream.fixed {
            flac.stream.max_block = flac.stream.max_block.max(block.duration());
        }
        if flac.stream.frames == 0 {
            flac.stream.frames = flac.end()?;
        }
        Ok(flac)
    }

    pub(super) fn info(&self) -> Info {
        Info {
            format: Format::Flac,
            channels: self.stream.channels,
            sample_rate: self.stream.sample_rate,
            frames: self.stream.frames,
        }
    }

    /// Reads from sample `start` on into `out`, interleaved, until it's
    /// full.
    pub(super) fn read(&mut self, start: u64, out: &mut [f32]) -> Result<(), AudioError> {
        let channels = self.stream.channels;
        let scale = 1.0 / (1u32 << (self.stream.bits - 1)) as f32;
        let frames = out.len() / channels;
        let mut done = 0;
        while done < frames {
            let at = start + done as u64;
            if !self.holds(at) {
                self.decode_to(at)?;
            }
            let (first, block) = self.block.as_ref().expect("the block holding `at`");
            let from = (at - first) as usize;
            let count = (block.duration() as usize - from).min(frames - done);
            for c in 0..channels {
                let samples = &block.channel(c as u32)[from..from + count];
                for (i, &sample) in samples.iter().enumerate() {
                    out[(done + i) * channels + c] = sample as f32 * scale;
                }
            }
            done += count;
        }
        Ok(())
    }

    /// Whether the block decoded last holds sample `at`.
    fn holds(&self, at: u64) -> bool {
        self.block.as_ref().is_some_and(|(start, block)| {
            (*start..start + u64::from(block.duration())).contains(&at)
        })
    }

    /// Decodes the block holding sample `at`: straight on, if it's just
    /// ahead, or else from where seeking finds.
    fn decode_to(&mut self, at: u64) -> Result<(), AudioError> {
        let ahead = match (&self.frames, &self.block) {
            (Some(_), Some((start, block))) => {
                *start <= at && at < start + u64::from(block.duration()) + STRAIGHT_ON
            }
            _ => false,
        };
        if !ahead {
            self.seek(at)?;
        }
        while !self.holds(at) {
            match self.next()? {
                Some(start) if start > at => return invalid("a frame missing from the stream"),
                Some(_) => {}
                None => return invalid("the file ends before the stream does"),
            }
        }
        Ok(())
    }

    /// Decodes the block holding sample `target`, or puts claxon's reader
    /// at a frame before it, close enough to decode straight on from.
    fn seek(&mut self, target: u64) -> Result<(), AudioError> {
        // The frame holding the target starts at `lo` or after it, and
        // before `hi`. `lo` is always a frame's start.
        let (mut lo, mut hi) = (self.first, self.length);
        while hi - lo > self.window {
            let mid = lo + (hi - lo) / 2;
            match self.find(mid, hi)? {
                Some(_) if self.holds(target) => return Ok(()),
                Some((offset, start)) if start <= target => lo = offset,
                _ => hi = mid,
            }
        }
        self.reader_at(lo)
    }

    /// Finds the first frame starting at byte `from` or after it, and
    /// before byte `to`, that decodes. Returns where it starts and its
    /// first sample; claxon's reader is left after it, and `block` holds it.
    fn find(&mut self, from: u64, to: u64) -> Result<Option<(u64, u64)>, AudioError> {
        let mut at = from;
        while at < to {
            self.release();
            let file = self.file.as_mut().expect("the file, after releasing it");
            file.seek(SeekFrom::Start(at))?;
            let read = read_up_to(file.as_mut(), &mut self.scan)?;
            let before = usize::try_from(to - at).map_or(read, |to| to.min(read));
            let header = (0..before).find(|&i| self.stream.is_header(&self.scan[i..read]));
            match header {
                Some(i) => {
                    let offset = at + i as u64;
                    self.reader_at(offset)?;
                    match self.next() {
                        Ok(Some(start)) => return Ok(Some((offset, start))),
                        Err(AudioError::Io(error))
                            if error.kind() != io::ErrorKind::UnexpectedEof =>
                        {
                            return Err(AudioError::Io(error));
                        }
                        // Bytes that only looked like a frame header.
                        Ok(None) | Err(_) => at = offset + 1,
                    }
                }
                None if read < self.scan.len() => return Ok(None),
                // A header can straddle the end of what was read.
                None => at += (read - MAX_HEADER) as u64,
            }
        }
        Ok(None)
    }

    /// The end of the stream, for a file whose STREAMINFO doesn't say how
    /// long it is: the end of its last frame.
    fn end(&mut self) -> Result<u64, AudioError> {
        let mut window = self.window;
        loop {
            let from = self.length.saturating_sub(window).max(self.first);
            if let Some((_, start)) = self.find(from, self.length)? {
                let (_, block) = self.block.as_ref().expect("the frame found");
                let mut end = start + u64::from(block.duration());
                // Whatever follows the last frame and doesn't decode, such
                // as a tag, ends the stream too.
                while let Ok(Some(start)) = self.next() {
                    let (_, block) = self.block.as_ref().expect("the frame decoded");
                    end = start + u64::from(block.duration());
                }
                return Ok(end);
            }
            if from == self.first {
                return invalid("no frames");
            }
            window *= 4;
        }
    }

    /// Puts claxon's reader at byte `offset`.
    fn reader_at(&mut self, offset: u64) -> Result<(), AudioError> {
        self.release();
        if let Some((_, block)) = self.block.take() {
            self.spare = block.into_buffer();
        }
        let file = self.file.as_mut().expect("the file, after releasing it");
        file.seek(SeekFrom::Start(offset))?;
        let file = self.file.take().expect("the file");
        self.frames = Some(FrameReader::new(BufferedReader::new(file)));
        Ok(())
    }

    /// Takes the file back from claxon's reader.
    fn release(&mut self) {
        if let Some(frames) = self.frames.take() {
            self.file = Some(frames.into_inner().into_inner());
        }
    }

    /// Decodes the frame claxon's reader is at into `block`, and returns its
    /// first sample, or `None` at the end of the file.
    fn next(&mut self) -> Result<Option<u64>, AudioError> {
        let buffer = match self.block.take() {
            Some((_, block)) => block.into_buffer(),
            None => mem::take(&mut self.spare),
        };
        let frames = self.frames.as_mut().expect("claxon's reader at a frame");
        match frames.read_next_or_eof(buffer) {
            Ok(Some(block)) if block.channels() as usize == self.stream.channels => {
                let start = if self.stream.fixed {
                    // Claxon multiplies the frame's number by its own block
                    // size, which is wrong for the last frame, the one
                    // shorter than the rest.
                    block.time() / u64::from(block.duration()) * self.stream.block
                } else {
                    block.time()
                };
                self.block = Some((start, block));
                Ok(Some(start))
            }
            Ok(Some(block)) => {
                self.spare = block.into_buffer();
                self.release();
                invalid("a frame with a different number of channels")
            }
            Ok(None) => Ok(None),
            Err(error) => {
                // Mid-frame, the reader can't go on.
                self.release();
                Err(decode_error(error))
            }
        }
    }
}

/// Writes FLAC files for tests, uncompressed: each subframe verbatim, or
/// constant where a channel holds one value through a block, so frames
/// still differ in size.
#[cfg(test)]
pub(super) mod write {
    use super::crc8;

    const RATE: u32 = 48_000;

    /// How a test file numbers its frames, and how long they are.
    pub struct Blocks<'a> {
        /// Each block's size in turn, the last repeating.
        pub sizes: &'a [usize],
        /// Numbers frames rather than their first samples, as encoders do
        /// for streams of one block size.
        pub fixed: bool,
        /// Whether STREAMINFO says how long the stream is.
        pub known: bool,
    }

    /// A FLAC file of `channels` at `bits` bits: 8, 16 or 24.
    pub fn flac(bits: u32, channels: &[Vec<i32>], blocks: Blocks<'_>) -> Vec<u8> {
        let frames = channels[0].len();
        let bytes = bits as usize / 8;
        let bits_code = match bits {
            8 => 1,
            16 => 4,
            24 => 6,
            _ => panic!("{bits}-bit samples aren't whole bytes"),
        };
        let mut out = b"fLaC".to_vec();
        out.extend_from_slice(&[0, 0, 0, 34]);
        let largest = *blocks.sizes.iter().max().unwrap();
        out.extend_from_slice(&16u16.to_be_bytes());
        out.extend_from_slice(&(largest as u16).to_be_bytes());
        // The frames' sizes, unknown.
        out.extend_from_slice(&[0; 6]);
        let length = if blocks.known { frames as u64 } else { 0 };
        let fields = (u64::from(RATE) << 44)
            | ((channels.len() as u64 - 1) << 41)
            | (u64::from(bits - 1) << 36)
            | length;
        out.extend_from_slice(&fields.to_be_bytes());
        // No MD5.
        out.extend_from_slice(&[0; 16]);
        // The last block: padding.
        out.extend_from_slice(&[0x81, 0, 0, 10]);
        out.extend_from_slice(&[0; 10]);
        let (mut at, mut number) = (0, 0);
        while at < frames {
            let size = blocks.sizes[number.min(blocks.sizes.len() - 1)].min(frames - at);
            let start = out.len();
            out.extend_from_slice(&header(
                &blocks,
                number,
                at,
                size,
                channels.len(),
                bits_code,
            ));
            for channel in channels {
                let block = &channel[at..at + size];
                if block.iter().all(|&s| s == block[0]) {
                    out.push(0x00);
                    out.extend_from_slice(&block[0].to_be_bytes()[4 - bytes..]);
                } else {
                    out.push(0x02);
                    for sample in block {
                        out.extend_from_slice(&sample.to_be_bytes()[4 - bytes..]);
                    }
                }
            }
            let crc = crc16(&out[start..]);
            out.extend_from_slice(&crc.to_be_bytes());
            at += size;
            number += 1;
        }
        out
    }

    /// The header of frame `number`, which starts at sample `at` and holds
    /// `size`.
    pub fn header(
        blocks: &Blocks<'_>,
        number: usize,
        at: usize,
        size: usize,
        channels: usize,
        bits_code: u8,
    ) -> Vec<u8> {
        let mut header = vec![0xFF, if blocks.fixed { 0xF8 } else { 0xF9 }];
        // The block size after the number, the sample rate from
        // STREAMINFO.
        header.push(0x70);
        header.push(((channels as u8 - 1) << 4) | (bits_code << 1));
        let n = if blocks.fixed { number } else { at } as u64;
        if n < 0x80 {
            header.push(n as u8);
        } else {
            let mut extra = 1;
            while n >> (6 * extra) >= 1 << (6 - extra) {
                extra += 1;
            }
            header.push((0xFF00u16 >> (extra + 1)) as u8 | (n >> (6 * extra)) as u8);
            for i in (0..extra).rev() {
                header.push(0x80 | ((n >> (6 * i)) & 0x3F) as u8);
            }
        }
        header.extend_from_slice(&((size - 1) as u16).to_be_bytes());
        header.push(crc8(&header));
        header
    }

    /// The CRC frames end with: polynomial x¹⁶ + x¹⁵ + x² + 1.
    fn crc16(bytes: &[u8]) -> u16 {
        bytes.iter().fold(0, |crc, &byte| {
            (0..8).fold(crc ^ (u16::from(byte) << 8), |crc, _| {
                if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x8005
                } else {
                    crc << 1
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::super::Decoder;
    use super::write::{Blocks, flac, header};
    use super::*;

    /// The signal the files from ffmpeg hold, as `tests/audio/README.md`
    /// says how they were made: triangle waves with a little noise.
    fn ffmpeg_signal(channel: usize, frame: usize, bits: u32) -> i32 {
        let noise = |n: usize| (((n as u64 * 1_103_515_245 + 12_345) % (1 << 31)) >> 16) as i32;
        let sixteen = |c: usize, i: usize| {
            let period = 200 + 64 * c;
            let (phase, half) = ((i + 31 * c) % period, period / 2);
            let triangle = if phase < half { phase } else { period - phase };
            let value = (triangle * 60_000 / half) as i32 - 30_000 + noise(2 * i + c) % 32 - 16;
            value.clamp(-32_768, 32_767)
        };
        match bits {
            16 => sixteen(channel, frame),
            _ => (sixteen(0, frame) * 256 + noise(frame + 7) % 64 - 32)
                .clamp(-(1 << 23), (1 << 23) - 1),
        }
    }

    const STEREO_16: &[u8] = include_bytes!("../../tests/audio/ffmpeg-stereo-16.flac");
    const MONO_24: &[u8] = include_bytes!("../../tests/audio/ffmpeg-mono-24.flac");

    fn open(bytes: &[u8]) -> Flac {
        Flac::new(Box::new(Cursor::new(bytes.to_vec()))).unwrap()
    }

    /// Reads `frames` frames from `start` and checks them against `expected`.
    fn check(flac: &mut Flac, start: u64, frames: usize, expected: impl Fn(usize, usize) -> i32) {
        let channels = flac.stream.channels;
        let scale = (1u32 << (flac.stream.bits - 1)) as f32;
        let mut out = vec![0.0; frames * channels];
        flac.read(start, &mut out).unwrap();
        for (i, frame) in out.chunks_exact(channels).enumerate() {
            for (c, &sample) in frame.iter().enumerate() {
                let want = expected(c, start as usize + i);
                assert_eq!(
                    sample * scale,
                    want as f32,
                    "channel {c}, frame {}",
                    start as usize + i
                );
            }
        }
    }

    #[test]
    fn files_from_ffmpeg_decode_exactly() {
        let mut stereo = Decoder::new(Box::new(Cursor::new(STEREO_16.to_vec()))).unwrap();
        let info = stereo.info();
        assert_eq!(
            info,
            Info {
                format: Format::Flac,
                channels: 2,
                sample_rate: 44_100,
                frames: 10_000
            }
        );
        let all = stereo.read_all().unwrap();
        for (c, channel) in all.iter().enumerate() {
            for (i, &sample) in channel.iter().enumerate() {
                assert_eq!(
                    sample * 32_768.0,
                    ffmpeg_signal(c, i, 16) as f32,
                    "{c}, {i}"
                );
            }
        }
        let mut mono = open(MONO_24);
        assert_eq!(mono.info().frames, 5_000);
        assert_eq!(mono.info().sample_rate, 96_000);
        check(&mut mono, 0, 5_000, |_, i| ffmpeg_signal(0, i, 24));
    }

    #[test]
    fn seeking_lands_on_the_sample_asked_for() {
        for window in [WINDOW, 16] {
            let mut flac = open(STEREO_16);
            flac.window = window;
            // Within blocks of 1,152 and across them, back and forth, and
            // into the last block, which is shorter.
            for (start, frames) in [
                (9_990, 10),
                (0, 1),
                (5_000, 1_500),
                (1_151, 2),
                (9_216, 784),
                (3, 4_000),
            ] {
                check(&mut flac, start, frames, |c, i| ffmpeg_signal(c, i, 16));
            }
        }
    }

    /// Two channels: a ramp, and a block of constant samples here and there.
    fn test_signal(frames: usize) -> Vec<Vec<i32>> {
        let ramp = (0..frames)
            .map(|i| (i as i32 * 37) % 60_000 - 30_000)
            .collect();
        let steps = (0..frames)
            .map(|i| (i / 3_000) as i32 * 1_000 - 5_000)
            .collect();
        vec![ramp, steps]
    }

    #[test]
    fn blocks_of_any_size_and_streams_of_unknown_length() {
        let signal = test_signal(50_000);
        let sizes = [4_096, 100, 7, 1_000, 16, 4_608, 2_000];
        for known in [true, false] {
            let bytes = flac(
                16,
                &signal,
                Blocks {
                    sizes: &sizes,
                    fixed: false,
                    known,
                },
            );
            let mut file = open(&bytes);
            file.window = 64;
            assert_eq!(
                file.info().frames,
                50_000,
                "STREAMINFO knows the length: {known}"
            );
            for (start, frames) in [
                (49_000, 1_000),
                (4_096, 107),
                (0, 50_000),
                (4_200, 3),
                (30_001, 9_000),
            ] {
                check(&mut file, start, frames, |c, i| signal[c][i]);
            }
        }
    }

    #[test]
    fn numbered_frames_end_with_a_shorter_one() {
        let signal = test_signal(10_000);
        let bytes = flac(
            24,
            &signal,
            Blocks {
                sizes: &[1_024],
                fixed: true,
                known: true,
            },
        );
        let mut file = open(&bytes);
        file.window = 64;
        // Claxon puts the last block at 9 × 784 rather than 9 × 1,024.
        for (start, frames) in [(9_300, 700), (9_215, 2), (0, 9_999)] {
            check(&mut file, start, frames, |c, i| signal[c][i]);
        }
    }

    #[test]
    fn bytes_that_look_like_a_frame_header_dont_mislead_seeking() {
        let frames = 20_000;
        let mut signal = test_signal(frames);
        let blocks = Blocks {
            sizes: &[2_000, 1_500],
            fixed: false,
            known: true,
        };
        // In the middle of the block from 5,000 on, a header for a frame
        // at 12,345, with a valid CRC, spelled out in 16-bit samples.
        let mut fake = header(&blocks, 0, 12_345, 1_500, 2, 4);
        fake.push(0x02);
        for (i, pair) in fake.chunks(2).enumerate() {
            let word = u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]);
            signal[0][5_500 + i] = i32::from(word as i16);
        }
        let bytes = flac(16, &signal, blocks);
        let mut file = open(&bytes);
        let at = bytes.windows(fake.len()).position(|w| w == fake).unwrap();
        assert!(
            file.stream.is_header(&bytes[at..]),
            "the fake header passes for a real one"
        );
        // Looking from just before it finds the next real frame, at 6,500.
        let (offset, start) = file
            .find(at as u64 - 8, bytes.len() as u64)
            .unwrap()
            .unwrap();
        assert!(offset > at as u64);
        assert_eq!(start, 6_500);
        file.window = 32;
        for start in (0..frames as u64).step_by(997) {
            check(
                &mut file,
                start,
                100.min(frames - start as usize),
                |c, i| signal[c][i],
            );
        }
    }

    #[test]
    fn coded_numbers_take_one_to_seven_bytes() {
        let blocks = Blocks {
            sizes: &[16],
            fixed: false,
            known: true,
        };
        for n in [
            0,
            0x7F,
            0x80,
            0x7FF,
            0x800,
            0xFFFF,
            0x1_0000,
            0x3F_FFFF,
            0x7FF_FFFF,
            0xF_FFFF_FFFF,
        ] {
            let header = header(&blocks, 0, n, 16, 1, 4);
            let (number, length) = coded_number(&header[4..]).unwrap();
            assert_eq!(number, n as u64);
            // The block size and the CRC follow.
            assert_eq!(4 + length + 3, header.len());
        }
        assert_eq!(coded_number(&[0x80]), None);
        assert_eq!(coded_number(&[0xC2, 0x41]), None);
        assert_eq!(coded_number(&[0xFF, 0x80]), None);
    }

    #[test]
    fn broken_and_unsupported_files_are_refused() {
        let signal = test_signal(1_000);
        let good = flac(
            16,
            &signal,
            Blocks {
                sizes: &[256],
                fixed: true,
                known: true,
            },
        );
        let refuse = |bytes: Vec<u8>| Decoder::new(Box::new(Cursor::new(bytes))).unwrap_err();
        // 32-bit samples, which claxon doesn't decode.
        let mut wide = good.clone();
        wide[8 + 12] |= 0x01;
        wide[8 + 13] |= 0xF0;
        assert!(
            matches!(refuse(wide), AudioError::Unsupported(_)),
            "32 bits"
        );
        // No STREAMINFO: the first block is padding.
        let mut no_info = good.clone();
        no_info[4] = 0x01;
        assert!(
            matches!(refuse(no_info), AudioError::Invalid(_)),
            "no STREAMINFO"
        );
        // Metadata that never ends.
        assert!(
            matches!(refuse(good[..30].to_vec()), AudioError::Invalid(_)),
            "cut short"
        );
        // A first frame that doesn't decode.
        let first = 4 + 38 + 14;
        let mut broken = good.clone();
        broken[first + 10] ^= 0xFF;
        assert!(
            matches!(refuse(broken), AudioError::Invalid(_)),
            "a broken frame"
        );
    }

    #[test]
    fn a_file_cut_short_reads_up_to_where_it_ends() {
        let signal = test_signal(4_000);
        let bytes = flac(
            16,
            &signal,
            Blocks {
                sizes: &[1_000],
                fixed: true,
                known: true,
            },
        );
        let mut file = open(&bytes[..bytes.len() - 100]);
        assert_eq!(file.info().frames, 4_000);
        check(&mut file, 0, 3_000, |c, i| signal[c][i]);
        let mut out = vec![0.0; 2 * 1_000];
        assert!(file.read(3_000, &mut out).is_err());
        // It still reads what's there.
        check(&mut file, 1_500, 100, |c, i| signal[c][i]);
    }
}
