//! Writing FLAC: integer samples in blocks of 4,096 frames, each channel
//! predicted by the best of FLAC's fixed predictors and what's left coded
//! with partitioned Rice codes, stereo as left and right, mid and side or
//! either with side, whichever is smallest. That compresses about as well
//! as the reference encoder's fastest settings: it has no linear
//! prediction yet.
//!
//! The file starts with STREAMINFO and a VORBIS_COMMENT block naming the
//! encoder. STREAMINFO's length, frame sizes and MD5 are filled in when the
//! file finishes.

use std::io::{self, Seek, SeekFrom, Write};

use super::flac::crc8;
use super::md5::Md5;

/// Frames per block, as the reference encoder has them.
const BLOCK: usize = 4096;
/// The finest partitioning of a block's residual tried: 64 partitions.
const MAX_PARTITION_ORDER: u32 = 6;
/// Where STREAMINFO's fields start: after `fLaC` and the block's header.
const STREAMINFO_AT: u64 = 8;
const VENDOR: &str = concat!("Musicable ", env!("CARGO_PKG_VERSION"));

/// Writes a FLAC file of integer samples as they come.
#[derive(Debug)]
pub(super) struct FlacEncoder<W: Write + Seek> {
    out: W,
    channels: usize,
    bits: u32,
    sample_rate: u32,
    /// The block being filled, channel by channel.
    block: Vec<Vec<i64>>,
    /// Frames written, counting the ones waiting in `block`.
    frames: u64,
    /// The number of the next frame.
    number: u64,
    /// The smallest and largest frames so far, in bytes.
    sizes: Option<(u32, u32)>,
    md5: Md5,
    /// Scratch space for encoding a frame.
    scratch: Scratch,
}

impl<W: Write + Seek> FlacEncoder<W> {
    /// Starts a file of `channels` channels of `bits`-bit samples at
    /// `sample_rate`, writing its metadata. FLAC holds 1 to 8 channels, of
    /// 4 to 24 bits here, at up to 655,350 Hz.
    pub(super) fn new(out: W, sample_rate: u32, channels: u16, bits: u32) -> io::Result<Self> {
        let refuse = |what: &str| Err(io::Error::new(io::ErrorKind::InvalidInput, what.to_owned()));
        if !(1..=8).contains(&channels) {
            return refuse("FLAC files hold 1 to 8 channels");
        }
        if !(4..=24).contains(&bits) {
            return refuse("FLAC files are written with 4- to 24-bit samples");
        }
        if sample_rate == 0 || sample_rate > 655_350 {
            return refuse("FLAC files are at most 655,350 Hz");
        }
        let channels = usize::from(channels);
        let mut encoder = Self {
            out,
            channels,
            bits,
            sample_rate,
            block: vec![Vec::with_capacity(BLOCK); channels],
            frames: 0,
            number: 0,
            sizes: None,
            md5: Md5::new(),
            scratch: Scratch::default(),
        };
        let mut head = b"fLaC".to_vec();
        // STREAMINFO, then the last block: a VORBIS_COMMENT.
        head.extend_from_slice(&[0, 0, 0, 34]);
        head.extend_from_slice(&encoder.streaminfo([0; 16]));
        let comment = 4 + VENDOR.len() + 4;
        head.push(0x84);
        head.extend_from_slice(&(comment as u32).to_be_bytes()[1..]);
        head.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
        head.extend_from_slice(VENDOR.as_bytes());
        head.extend_from_slice(&0u32.to_le_bytes());
        encoder.out.seek(SeekFrom::Start(0))?;
        encoder.out.write_all(&head)?;
        Ok(encoder)
    }

    /// Writes `samples`, whole frames of interleaved channels, each within
    /// the samples' bits.
    ///
    /// # Panics
    ///
    /// If `samples` isn't whole frames.
    pub(super) fn write(&mut self, samples: &[i32]) -> io::Result<()> {
        assert!(
            samples.len().is_multiple_of(self.channels),
            "{} samples aren't whole frames of {} channels",
            samples.len(),
            self.channels
        );
        let bytes = self.bits.div_ceil(8) as usize;
        let mut hashed = [0u8; 4 * 8];
        for frame in samples.chunks_exact(self.channels) {
            for (c, &sample) in frame.iter().enumerate() {
                self.block[c].push(i64::from(sample));
                hashed[c * bytes..(c + 1) * bytes].copy_from_slice(&sample.to_le_bytes()[..bytes]);
            }
            self.md5.update(&hashed[..self.channels * bytes]);
            self.frames += 1;
            if self.block[0].len() == BLOCK {
                self.encode_block()?;
            }
        }
        Ok(())
    }

    /// Writes the last block, fills in STREAMINFO, and hands back `out`.
    pub(super) fn finish(mut self) -> io::Result<W> {
        if !self.block[0].is_empty() {
            self.encode_block()?;
        }
        let md5 = std::mem::replace(&mut self.md5, Md5::new()).finish();
        let info = self.streaminfo(md5);
        let end = self.out.stream_position()?;
        self.out.seek(SeekFrom::Start(STREAMINFO_AT))?;
        self.out.write_all(&info)?;
        self.out.seek(SeekFrom::Start(end))?;
        self.out.flush()?;
        Ok(self.out)
    }

    /// STREAMINFO's 34 bytes, as of the frames so far.
    fn streaminfo(&self, md5: [u8; 16]) -> Vec<u8> {
        let mut info = Vec::with_capacity(34);
        info.extend_from_slice(&(BLOCK as u16).to_be_bytes());
        info.extend_from_slice(&(BLOCK as u16).to_be_bytes());
        let (smallest, largest) = self.sizes.unwrap_or((0, 0));
        info.extend_from_slice(&smallest.to_be_bytes()[1..]);
        info.extend_from_slice(&largest.to_be_bytes()[1..]);
        let fields = (u64::from(self.sample_rate) << 44)
            | ((self.channels as u64 - 1) << 41)
            | (u64::from(self.bits - 1) << 36)
            | (self.frames & ((1 << 36) - 1));
        info.extend_from_slice(&fields.to_be_bytes());
        info.extend_from_slice(&md5);
        info
    }

    /// Encodes the block as a frame, writes it and empties the block.
    fn encode_block(&mut self) -> io::Result<()> {
        let mut scratch = std::mem::take(&mut self.scratch);
        let bits = self.bits;
        let block = &self.block;
        let mut planned = |samples: &[i64], bits: u32| {
            let plan = plan(samples, bits, &mut scratch);
            (samples.to_vec(), bits, plan)
        };
        // The channels as the frame stores them, with their sample sizes
        // and how each is coded, and the channel assignment's code.
        let (code, stored) = if self.channels == 2 {
            let (mid, side) = (mid(block), side(block));
            let [left, right, mid, side] = [
                planned(&block[0], bits),
                planned(&block[1], bits),
                planned(&mid, bits),
                planned(&side, bits + 1),
            ];
            let size = |a: &Stored, b: &Stored| a.2.size + b.2.size;
            let options = [
                (size(&left, &right), 1),
                (size(&left, &side), 8),
                (size(&side, &right), 9),
                (size(&mid, &side), 10),
            ];
            let (_, code) = options
                .into_iter()
                .min_by_key(|&(size, _)| size)
                .expect("four");
            let stored = match code {
                1 => vec![left, right],
                8 => vec![left, side],
                9 => vec![side, right],
                _ => vec![mid, side],
            };
            (code, stored)
        } else {
            let stored = block.iter().map(|c| planned(c, bits)).collect();
            (self.channels as u64 - 1, stored)
        };
        let mut out = Bits::default();
        let header = Header {
            block: block[0].len(),
            sample_rate: self.sample_rate,
            channels: code,
            bits,
            number: self.number,
        };
        header.write(&mut out);
        for (samples, bits, plan) in &stored {
            plan.write(&mut out, samples, *bits, &mut scratch);
        }
        out.align();
        let crc = crc16(&out.bytes);
        out.put(16, u64::from(crc));
        self.scratch = scratch;
        let size = out.bytes.len() as u32;
        self.sizes = Some(match self.sizes {
            Some((smallest, largest)) => (smallest.min(size), largest.max(size)),
            None => (size, size),
        });
        self.out.write_all(&out.bytes)?;
        self.number += 1;
        for channel in &mut self.block {
            channel.clear();
        }
        Ok(())
    }
}

/// A channel as a frame stores it: its samples, their size in bits, and
/// how they're coded.
type Stored = (Vec<i64>, u32, Plan);

/// Left minus right.
fn side(block: &[Vec<i64>]) -> Vec<i64> {
    block[0].iter().zip(&block[1]).map(|(l, r)| l - r).collect()
}

/// Left plus right, halved and rounded down, as FLAC has it: the side
/// channel's lowest bit says what was lost.
fn mid(block: &[Vec<i64>]) -> Vec<i64> {
    block[0]
        .iter()
        .zip(&block[1])
        .map(|(l, r)| (l + r) >> 1)
        .collect()
}

/// Buffers an encoder reuses from frame to frame.
#[derive(Debug, Default)]
struct Scratch {
    residual: Vec<i64>,
    /// The residual's Rice-coded values, by `zigzag`.
    folded: Vec<u64>,
    sums: Vec<u64>,
}

/// How a channel's subframe is coded, and how many bits it takes, about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    kind: Kind,
    size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Constant,
    Verbatim,
    Fixed {
        order: usize,
        partition_order: u32,
        /// Each partition's Rice parameter.
        parameters: Vec<u32>,
    },
}

/// The smallest way to code `samples` of `bits` bits: a constant, the
/// samples as they are, or the best fixed predictor, judged by the sum of
/// the residual it leaves, with the residual partitioned the best way.
fn plan(samples: &[i64], bits: u32, scratch: &mut Scratch) -> Plan {
    let header = 8;
    if samples.iter().all(|&s| s == samples[0]) {
        return Plan {
            kind: Kind::Constant,
            size: header + u64::from(bits),
        };
    }
    let verbatim = Plan {
        kind: Kind::Verbatim,
        size: header + samples.len() as u64 * u64::from(bits),
    };
    let n = samples.len();
    let orders = 0..=4.min(n - 1);
    let best = orders
        .map(|order| (residual_sum(samples, order), order))
        .min()
        .expect("order 0 at least");
    let order = best.1;
    residual(samples, order, &mut scratch.residual);
    // A residual too big for 32 bits isn't allowed, and won't be smaller
    // than the samples anyway.
    if scratch.residual.iter().any(|&r| i32::try_from(r).is_err()) {
        return verbatim;
    }
    scratch.folded.clear();
    scratch
        .folded
        .extend(scratch.residual.iter().map(|&r| zigzag(r)));
    let (partition_order, parameters, coded) =
        partition(&scratch.folded, n, order, &mut scratch.sums);
    let fixed = Plan {
        size: header + order as u64 * u64::from(bits) + 2 + 4 + coded,
        kind: Kind::Fixed {
            order,
            partition_order,
            parameters,
        },
    };
    if fixed.size < verbatim.size {
        fixed
    } else {
        verbatim
    }
}

impl Plan {
    /// Writes the subframe for `samples` of `bits` bits.
    fn write(&self, out: &mut Bits, samples: &[i64], bits: u32, scratch: &mut Scratch) {
        match &self.kind {
            Kind::Constant => {
                out.put(8, 0);
                out.put_signed(bits, samples[0]);
            }
            Kind::Verbatim => {
                out.put(8, 0b0000_0010);
                for &sample in samples {
                    out.put_signed(bits, sample);
                }
            }
            Kind::Fixed {
                order,
                partition_order,
                parameters,
            } => {
                // A zero, type 001 and the order, no wasted bits.
                out.put(8, (0b00_1000 | *order as u64) << 1);
                for &sample in &samples[..*order] {
                    out.put_signed(bits, sample);
                }
                residual(samples, *order, &mut scratch.residual);
                let wide = parameters.iter().any(|&k| k > 14);
                out.put(2, u64::from(wide));
                out.put(4, u64::from(*partition_order));
                let partitions = 1 << partition_order;
                let length = samples.len() >> partition_order;
                let mut at = 0;
                for (p, &k) in parameters.iter().enumerate().take(partitions) {
                    let count = if p == 0 { length - order } else { length };
                    out.put(if wide { 5 } else { 4 }, u64::from(k));
                    for &r in &scratch.residual[at..at + count] {
                        out.put_rice(zigzag(r), k);
                    }
                    at += count;
                }
            }
        }
    }
}

/// The sum of the residual's magnitudes that FLAC's fixed predictor of
/// `order` leaves: a guide to how well it predicts.
fn residual_sum(samples: &[i64], order: usize) -> u64 {
    let mut sum: u64 = 0;
    for i in order..samples.len() {
        sum = sum.saturating_add(predict_error(samples, order, i).unsigned_abs());
    }
    sum
}

/// What FLAC's fixed predictor of `order` leaves of sample `i`, from the
/// samples before it.
fn predict_error(s: &[i64], order: usize, i: usize) -> i64 {
    match order {
        0 => s[i],
        1 => s[i] - s[i - 1],
        2 => s[i] - 2 * s[i - 1] + s[i - 2],
        3 => s[i] - 3 * s[i - 1] + 3 * s[i - 2] - s[i - 3],
        _ => s[i] - 4 * s[i - 1] + 6 * s[i - 2] - 4 * s[i - 3] + s[i - 4],
    }
}

/// The residual of `order` for the samples after its warm-up ones.
fn residual(samples: &[i64], order: usize, out: &mut Vec<i64>) {
    out.clear();
    out.extend((order..samples.len()).map(|i| predict_error(samples, order, i)));
}

/// A signed residual as the unsigned value Rice codes take: 0, -1, 1, -2…
/// become 0, 1, 2, 3…
fn zigzag(r: i64) -> u64 {
    ((r << 1) ^ (r >> 63)) as u64
}

/// The best partitioning of a block's `folded` residual, for a block of
/// `n` samples and a predictor of `order`: its order, each partition's Rice
/// parameter, and the bits the partitions take, about. Each partition
/// order is judged from the sums of the values in each partition, merged
/// from the finest one.
fn partition(folded: &[u64], n: usize, order: usize, sums: &mut Vec<u64>) -> (u32, Vec<u32>, u64) {
    let mut finest = 0;
    while finest < MAX_PARTITION_ORDER
        && n.is_multiple_of(1 << (finest + 1))
        && (n >> (finest + 1)) > order
    {
        finest += 1;
    }
    sums.clear();
    let length = n >> finest;
    let mut at = 0;
    for p in 0..1usize << finest {
        let count = if p == 0 { length - order } else { length };
        sums.push(folded[at..at + count].iter().sum());
        at += count;
    }
    let mut best: Option<(u64, u32, Vec<u32>)> = None;
    let mut partition_order = finest;
    loop {
        let length = n >> partition_order;
        let mut parameters = Vec::with_capacity(sums.len());
        let mut coded = 0;
        for (p, &sum) in sums.iter().enumerate() {
            let count = if p == 0 { length - order } else { length } as u64;
            let (k, bits) = rice_parameter(sum, count);
            parameters.push(k);
            coded += bits;
        }
        let wide = parameters.iter().any(|&k| k > 14);
        coded += sums.len() as u64 * if wide { 5 } else { 4 };
        if best.as_ref().is_none_or(|(bits, ..)| coded < *bits) {
            best = Some((coded, partition_order, parameters));
        }
        if partition_order == 0 {
            break;
        }
        partition_order -= 1;
        let merged: Vec<u64> = sums.as_chunks::<2>().0.iter().map(|[a, b]| a + b).collect();
        *sums = merged;
    }
    let (coded, partition_order, parameters) = best.expect("order 0 at least");
    (partition_order, parameters, coded)
}

/// The Rice parameter for `count` values summing to `sum`, and the bits
/// they'd take with it, about: each value takes a stop bit and the
/// parameter's low bits, and its quotient in unary.
fn rice_parameter(sum: u64, count: u64) -> (u32, u64) {
    if count == 0 {
        return (0, 0);
    }
    let cost = |k: u32| count * (u64::from(k) + 1) + (sum >> k);
    let mean = sum / count;
    let guess = if mean == 0 {
        0
    } else {
        63 - mean.leading_zeros()
    };
    let low = guess.saturating_sub(1);
    let best = (low..=(guess + 1).min(30))
        .map(|k| (cost(k), k))
        .min()
        .expect("a parameter at least");
    (best.1, best.0)
}

/// What a frame header says.
struct Header {
    block: usize,
    sample_rate: u32,
    /// The channel assignment: the channels less one, if they're stored
    /// as they are, or 8 to 10 for stereo stored with a side channel.
    channels: u64,
    bits: u32,
    number: u64,
}

impl Header {
    /// Writes the header, up to and including its CRC.
    fn write(&self, out: &mut Bits) {
        let block = self.block;
        // The sync code, and blocks of one size.
        out.put(16, 0xFFF8);
        let (size_code, size_extra) = match block {
            192 => (1, None),
            576 | 1152 | 2304 | 4608 => (2 + (block / 576).trailing_zeros() as u64, None),
            256 | 512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768 => {
                (8 + (block / 256).trailing_zeros() as u64, None)
            }
            1..=256 => (6, Some((8, block as u64 - 1))),
            _ => (7, Some((16, block as u64 - 1))),
        };
        let (rate_code, rate_extra) = match self.sample_rate {
            88_200 => (1, None),
            176_400 => (2, None),
            192_000 => (3, None),
            8_000 => (4, None),
            16_000 => (5, None),
            22_050 => (6, None),
            24_000 => (7, None),
            32_000 => (8, None),
            44_100 => (9, None),
            48_000 => (10, None),
            96_000 => (11, None),
            rate if rate % 1000 == 0 && rate / 1000 <= 255 => {
                (12, Some((8, u64::from(rate / 1000))))
            }
            rate if rate <= 65_535 => (13, Some((16, u64::from(rate)))),
            rate if rate % 10 == 0 && rate / 10 <= 65_535 => (14, Some((16, u64::from(rate / 10)))),
            // From STREAMINFO.
            _ => (0, None),
        };
        out.put(4, size_code);
        out.put(4, rate_code);
        out.put(4, self.channels);
        let bits_code = match self.bits {
            8 => 1,
            12 => 2,
            16 => 4,
            20 => 5,
            24 => 6,
            // From STREAMINFO.
            _ => 0,
        };
        out.put(3, bits_code);
        out.put(1, 0);
        out.put_coded_number(self.number);
        for (width, value) in [size_extra, rate_extra].into_iter().flatten() {
            out.put(width, value);
        }
        let crc = crc8(&out.bytes);
        out.put(8, u64::from(crc));
    }
}

/// Bits, written most significant first.
#[derive(Debug, Default)]
struct Bits {
    bytes: Vec<u8>,
    /// Bits not yet in `bytes`, in the low `pending` bits.
    held: u64,
    pending: u32,
}

impl Bits {
    /// Writes the low `width` bits of `value`, at most 32.
    fn put(&mut self, width: u32, value: u64) {
        debug_assert!(width <= 32);
        if width == 0 {
            return;
        }
        let mask = (1u64 << width) - 1;
        self.held = (self.held << width) | (value & mask);
        self.pending += width;
        while self.pending >= 8 {
            self.pending -= 8;
            self.bytes.push((self.held >> self.pending) as u8);
        }
    }

    /// Writes `value` as a `width`-bit two's complement number.
    fn put_signed(&mut self, width: u32, value: i64) {
        self.put(width, value as u64);
    }

    /// Writes `value` Rice-coded with parameter `k`: its quotient in unary,
    /// zeros ended by a one, then its low `k` bits.
    fn put_rice(&mut self, value: u64, k: u32) {
        let mut quotient = value >> k;
        while quotient >= 32 {
            self.put(32, 0);
            quotient -= 32;
        }
        self.put(quotient as u32 + 1, 1);
        self.put(k, value);
    }

    /// Writes a frame number as FLAC codes it: as UTF-8 codes characters,
    /// up to 36 bits.
    fn put_coded_number(&mut self, number: u64) {
        if number < 0x80 {
            self.put(8, number);
            return;
        }
        let mut extra = 1;
        while number >> (6 * extra) >= 1 << (6 - extra) {
            extra += 1;
        }
        let lead = (0xFF00u64 >> (extra + 1)) & 0xFF;
        self.put(8, lead | (number >> (6 * extra)));
        for i in (0..extra).rev() {
            self.put(8, 0x80 | ((number >> (6 * i)) & 0x3F));
        }
    }

    /// Pads with zeros to a whole byte.
    fn align(&mut self) {
        if self.pending > 0 {
            self.put(8 - self.pending, 0);
        }
    }
}

/// The CRC frames end with: polynomial x¹⁶ + x¹⁵ + x² + 1.
fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0, |crc, &byte| {
        (crc << 8) ^ CRC16[usize::from((crc >> 8) as u8 ^ byte)]
    })
}

const CRC16: [u16; 256] = {
    let mut table = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::super::Decoder;
    use super::*;

    /// Encodes `channels`, interleaved, as a FLAC file.
    fn encode(rate: u32, bits: u32, channels: &[Vec<i32>]) -> Vec<u8> {
        let mut encoder =
            FlacEncoder::new(Cursor::new(Vec::new()), rate, channels.len() as u16, bits).unwrap();
        let frames = channels[0].len();
        let mut interleaved = Vec::with_capacity(frames * channels.len());
        for i in 0..frames {
            interleaved.extend(channels.iter().map(|c| c[i]));
        }
        // In uneven pieces, as an export writes them.
        let mut at = 0;
        for piece in [1, 1000, 5000, 3].iter().cycle() {
            let end = (at + piece * channels.len()).min(interleaved.len());
            encoder.write(&interleaved[at..end]).unwrap();
            at = end;
            if at == interleaved.len() {
                break;
            }
        }
        encoder.finish().unwrap().into_inner()
    }

    /// Decodes a FLAC file into integers of `bits` bits, by channel.
    fn decode(bytes: Vec<u8>, bits: u32) -> Vec<Vec<i32>> {
        let mut decoder = Decoder::new(Box::new(Cursor::new(bytes))).unwrap();
        let scale = f64::from(1u32 << (bits - 1));
        let planar = decoder.read_all().unwrap();
        planar
            .into_iter()
            .map(|c| {
                c.into_iter()
                    .map(|s| (f64::from(s) * scale).round() as i32)
                    .collect()
            })
            .collect()
    }

    /// Music-like test signals: a mix of sines, with noise.
    fn signal(channels: usize, frames: usize, bits: u32) -> Vec<Vec<i32>> {
        let full = f64::from(1u32 << (bits - 1)) - 1.0;
        let mut noise = 12_345u64;
        (0..channels)
            .map(|c| {
                (0..frames)
                    .map(|i| {
                        noise = noise
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1);
                        let t = i as f64 / 48_000.0;
                        let tone = 0.5 * (t * 440.0 * std::f64::consts::TAU).sin()
                            + 0.3 * (t * (97.0 + 30.0 * c as f64) * std::f64::consts::TAU).sin();
                        let hiss = ((noise >> 40) as f64 / f64::from(1 << 24) - 0.5) * 0.01;
                        ((tone + hiss) * full * 0.9).round() as i32
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn files_decode_to_the_samples_written() {
        for bits in [16, 24] {
            for channels in [1, 2, 3] {
                for frames in [0, 1, 15, 4096, 4097, 10_000] {
                    let signal = signal(channels, frames, bits);
                    let bytes = encode(48_000, bits, &signal);
                    if frames == 0 {
                        // Claxon wants a frame, so look at STREAMINFO alone.
                        assert_eq!(&bytes[..4], b"fLaC");
                        continue;
                    }
                    assert_eq!(
                        decode(bytes, bits),
                        signal,
                        "{bits} bits, {channels} channels, {frames} frames"
                    );
                }
            }
        }
    }

    #[test]
    fn music_compresses() {
        let signal = signal(2, 48_000, 16);
        let bytes = encode(48_000, 16, &signal);
        let raw = 2 * 2 * 48_000;
        assert!(bytes.len() < raw * 3 / 4, "{} of {raw} bytes", bytes.len());
        assert_eq!(decode(bytes, 16), signal);
    }

    #[test]
    fn stereo_takes_the_smallest_assignment() {
        // The fourth byte of the first frame's header has the assignment.
        let first_frame = |bytes: &[u8]| {
            let at = 4 + 4 + 34 + 4 + 4 + VENDOR.len() + 4;
            assert_eq!(bytes[at..at + 2], [0xFF, 0xF8]);
            bytes[at + 3] >> 4
        };
        let tone = signal(1, 4096, 16).remove(0);
        // The same in both channels: the side is silent.
        let same = vec![tone.clone(), tone.clone()];
        let bytes = encode(48_000, 16, &same);
        assert!((8..=10).contains(&first_frame(&bytes)));
        assert_eq!(decode(bytes, 16), same);
        // A loud tone and quiet noise that's nothing like it: the side is
        // as loud as the tone, so it doesn't help.
        let noise: Vec<i32> = (0..4096).map(|i| ((i * 7919) % 641) - 320).collect();
        let apart = vec![tone, noise];
        let bytes = encode(48_000, 16, &apart);
        assert_eq!(first_frame(&bytes), 1);
        assert_eq!(decode(bytes, 16), apart);
    }

    #[test]
    fn silence_and_full_scale_steps() {
        for bits in [16, 24] {
            let max = (1 << (bits - 1)) - 1;
            let min = -(1 << (bits - 1));
            let mut steps = vec![0; 5000];
            for (i, s) in steps.iter_mut().enumerate() {
                *s = if (i / 37) % 2 == 0 { max } else { min };
            }
            let signal = vec![
                vec![0; 5000],
                steps.clone(),
                steps.iter().map(|s| -s - 1).collect(),
            ];
            assert_eq!(
                decode(encode(44_100, bits, &signal), bits),
                signal,
                "{bits}"
            );
        }
    }

    #[test]
    fn any_sample_rate() {
        for rate in [44_100, 48_000, 96_000, 11_025, 37_800, 384_000, 1_234] {
            let signal = signal(2, 500, 16);
            let bytes = encode(rate, 16, &signal);
            let decoder = Decoder::new(Box::new(Cursor::new(bytes.clone()))).unwrap();
            assert_eq!(decoder.info().sample_rate, rate);
            assert_eq!(decode(bytes, 16), signal, "{rate}");
        }
    }

    #[test]
    fn streaminfo_has_the_length_and_the_md5() {
        let signal = signal(2, 9000, 24);
        let bytes = encode(48_000, 24, &signal);
        let fields = u64::from_be_bytes(bytes[18..26].try_into().unwrap());
        assert_eq!(fields & ((1 << 36) - 1), 9000);
        assert_eq!((fields >> 44) as u32, 48_000);
        let mut md5 = Md5::new();
        for i in 0..9000 {
            for channel in &signal {
                md5.update(&channel[i].to_le_bytes()[..3]);
            }
        }
        assert_eq!(bytes[26..42], md5.finish());
        let vendor = &bytes[42 + 4 + 4..][..VENDOR.len()];
        assert_eq!(vendor, VENDOR.as_bytes());
    }

    #[test]
    fn frame_numbers_take_more_bytes_as_they_grow() {
        for (number, coded) in [
            (0u64, &[0x00][..]),
            (0x7F, &[0x7F]),
            (0x80, &[0xC2, 0x80]),
            (0x7FF, &[0xDF, 0xBF]),
            (0x800, &[0xE0, 0xA0, 0x80]),
            (0x1_0000, &[0xF0, 0x90, 0x80, 0x80]),
        ] {
            let mut bits = Bits::default();
            bits.put_coded_number(number);
            assert_eq!(bits.bytes, coded, "{number:#x}");
        }
    }

    #[test]
    fn rice_codes_put_the_quotient_in_unary() {
        let mut bits = Bits::default();
        // 13 with k = 2: quotient 3, remainder 1: 0001 01.
        bits.put_rice(13, 2);
        bits.put_rice(0, 0);
        bits.align();
        assert_eq!(bits.bytes, [0b0001_0110]);
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(-1), 1);
        assert_eq!(zigzag(1), 2);
        assert_eq!(zigzag(-2), 3);
    }
}
