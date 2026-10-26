//! WAV files: RIFF chunks, little-endian.

use std::io::{self, Read, Seek, SeekFrom, Write};

use super::{AudioError, Layout, Pcm, SampleFormat, Source, frames_of, invalid, unsupported};

const PCM: u16 = 1;
const FLOAT: u16 = 3;
const EXTENSIBLE: u16 = 0xFFFE;

/// Finds the format and the samples of a WAV file, and the tempo its ACID
/// chunk gives, if it has one and isn't a one-shot.
pub(super) fn open(mut source: Box<dyn Source>) -> Result<(Pcm, Option<f64>), AudioError> {
    source.seek(SeekFrom::Start(12))?;
    let mut format = None;
    let mut acid = None;
    loop {
        let Some((id, size)) = chunk_header(source.as_mut())? else {
            return invalid("no data chunk");
        };
        match &id {
            b"fmt " => {
                let mut fmt = vec![0; size as usize];
                source
                    .read_exact(&mut fmt)
                    .map_err(|_| AudioError::Invalid("a short fmt chunk".into()))?;
                format = Some(parse_format(&fmt)?);
                // Chunks start on even bytes.
                source.seek(SeekFrom::Current(i64::from(size % 2)))?;
            }
            b"acid" => acid = read_acid(source.as_mut(), size)?,
            b"data" => {
                let Some((channels, sample_rate, layout)) = format else {
                    return invalid("the data chunk comes before the fmt chunk");
                };
                let start = source.stream_position()?;
                // Recorders that stop before finishing the file leave its
                // size at zero or as large as it goes: the samples run to
                // the end.
                let size = match size {
                    0 | u32::MAX => u64::MAX,
                    size => u64::from(size),
                };
                // The ACID chunk can come after the samples too.
                if acid.is_none() && size != u64::MAX {
                    source.seek(SeekFrom::Start(start + size + size % 2))?;
                    while let Some((id, size)) = chunk_header(source.as_mut())? {
                        if &id == b"acid" {
                            acid = read_acid(source.as_mut(), size)?;
                            break;
                        }
                        source.seek(SeekFrom::Current(i64::from(size) + i64::from(size % 2)))?;
                    }
                }
                let pcm = Pcm::new(source, start, size, channels, sample_rate, layout)?;
                let tempo = acid.and_then(|acid| acid.tempo(pcm.frames, sample_rate));
                return Ok((pcm, tempo));
            }
            _ => {
                source.seek(SeekFrom::Current(i64::from(size) + i64::from(size % 2)))?;
            }
        }
    }
}

/// The next chunk's id and size, if the file goes on to one.
fn chunk_header(source: &mut dyn Source) -> Result<Option<([u8; 4], u32)>, AudioError> {
    let mut header = [0; 8];
    if super::read_up_to(source, &mut header)? < 8 {
        return Ok(None);
    }
    let id = header[..4].try_into().expect("four bytes");
    let size = u32::from_le_bytes(header[4..].try_into().expect("four bytes"));
    Ok(Some((id, size)))
}

/// What an ACID chunk says about a loop that matters here.
#[derive(Debug, Clone, Copy)]
struct Acid {
    one_shot: bool,
    beats: u32,
    tempo: f32,
}

impl Acid {
    /// The loop's tempo, unless it's a one-shot: the chunk's, to a
    /// ten-thousandth of a beat a minute, which takes off what storing it in
    /// 32 bits added, or else the one its beats give the file's `frames`.
    fn tempo(self, frames: u64, sample_rate: u32) -> Option<f64> {
        if self.one_shot {
            return None;
        }
        let tempo = f64::from(self.tempo);
        if tempo.is_finite() && tempo > 0.0 {
            return Some((tempo * 1e4).round() / 1e4);
        }
        let seconds = frames as f64 / f64::from(sample_rate);
        (self.beats > 0 && seconds > 0.0).then(|| f64::from(self.beats) * 60.0 / seconds)
    }
}

/// Reads an ACID chunk of `size` bytes, which the source is at the start
/// of, leaving it at the next chunk: its flags, then the root note, two
/// fields no one uses, the loop's beats, its meter and its tempo.
fn read_acid(source: &mut dyn Source, size: u32) -> Result<Option<Acid>, AudioError> {
    let mut acid = [0; 24];
    let read = super::read_up_to(source, &mut acid[..size.min(24) as usize])?;
    source.seek(SeekFrom::Current(
        i64::from(size) + i64::from(size % 2) - read as i64,
    ))?;
    if read < 24 {
        return Ok(None);
    }
    let u32_at = |i: usize| u32::from_le_bytes(acid[i..i + 4].try_into().expect("four bytes"));
    Ok(Some(Acid {
        one_shot: u32_at(0) & 1 != 0,
        beats: u32_at(12),
        tempo: f32::from_bits(u32_at(20)),
    }))
}

/// The channels, sample rate and layout a fmt chunk gives.
fn parse_format(fmt: &[u8]) -> Result<(usize, u32, Layout), AudioError> {
    if fmt.len() < 16 {
        return invalid("a short fmt chunk");
    }
    let u16_at = |i: usize| u16::from_le_bytes([fmt[i], fmt[i + 1]]);
    let mut tag = u16_at(0);
    let channels = usize::from(u16_at(2));
    let sample_rate = u32::from_le_bytes(fmt[4..8].try_into().expect("four bytes"));
    let bits = u16_at(14);
    if tag == EXTENSIBLE {
        if fmt.len() < 26 {
            return invalid("a short extensible fmt chunk");
        }
        // The subformat GUID starts with the format tag.
        tag = u16_at(24);
    }
    let layout = match (tag, bits) {
        (PCM, 8) => Layout::U8,
        (PCM, 16) => Layout::I16 { big: false },
        (PCM, 24) => Layout::I24 { big: false },
        (PCM, 32) => Layout::I32 { big: false },
        (FLOAT, 32) => Layout::F32 { big: false },
        (FLOAT, 64) => Layout::F64 { big: false },
        (PCM | FLOAT, bits) => return unsupported(format!("{bits}-bit WAV samples")),
        (tag, _) => return unsupported(format!("WAV format {tag:#06x}")),
    };
    Ok((channels, sample_rate, layout))
}

/// The most bytes of samples a WAV file holds, with room for its header
/// within the 4 GiB its sizes can say.
pub(super) const MAX_DATA: u64 = u32::MAX as u64 - 64;

pub(super) fn write(
    out: &mut impl Write,
    sample_rate: u32,
    format: SampleFormat,
    channels: &[&[f32]],
) -> io::Result<()> {
    let frames = frames_of(channels)?;
    let count = channels.len();
    let block = count * format.bytes();
    let data = frames as u64 * block as u64;
    if data >= MAX_DATA {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too long for a WAV file",
        ));
    }
    let mut bytes = Vec::with_capacity(HEADER + data as usize + 1);
    header(&mut bytes, sample_rate, format, count as u16, frames as u64);
    for i in 0..frames {
        for channel in channels {
            encode(format, f64::from(channel[i]), &mut bytes);
        }
    }
    if data % 2 == 1 {
        bytes.push(0);
    }
    out.write_all(&bytes)
}

/// One sample as a WAV file stores it: little-endian, and 8-bit integers
/// unsigned.
fn encode(format: SampleFormat, sample: f64, out: &mut Vec<u8>) {
    format.encode(sample, out);
    if format == SampleFormat::Int8 {
        // Flipping the sign bit makes a signed byte unsigned.
        *out.last_mut().expect("a sample") ^= 0x80;
    }
}

/// The most bytes [`header`] writes.
pub(super) const HEADER: usize = 56;

/// Writes the header of a WAV file of `frames` frames of `channels`
/// channels of `format` samples at `sample_rate`, up to the start of its
/// samples.
pub(super) fn header(
    out: &mut Vec<u8>,
    sample_rate: u32,
    format: SampleFormat,
    channels: u16,
    frames: u64,
) {
    let block = u32::from(channels) * format.bytes() as u32;
    // Callers keep the samples within MAX_DATA.
    let data = (frames * u64::from(block)) as u32;
    let float = format.is_float();
    // Float files need a fact chunk with their length.
    let fact = if float { 12 } else { 0 };
    out.extend_from_slice(b"RIFF");
    let riff = 4 + 24 + fact + 8 + data + data % 2;
    out.extend_from_slice(&riff.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&(if float { FLOAT } else { PCM }).to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * block).to_le_bytes());
    out.extend_from_slice(&(block as u16).to_le_bytes());
    out.extend_from_slice(&(format.bytes() as u16 * 8).to_le_bytes());
    if float {
        out.extend_from_slice(b"fact");
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&(frames as u32).to_le_bytes());
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
}

/// Writes a WAV file of 32-bit float samples as they come, such as a
/// recording's. The header gives the file's length as of the last
/// [`flush`](Self::flush), and until the first, says the samples run to the
/// end, so a file cut off by a crash reads back as far as it got.
#[derive(Debug)]
pub struct WavWriter<W: Write + Seek> {
    out: W,
    sample_rate: u32,
    channels: u16,
    frames: u64,
    /// Bytes for the samples to come, before they go to `out`.
    buffer: Vec<u8>,
}

impl<W: Write + Seek> WavWriter<W> {
    /// Starts a file of `channels` channels at `sample_rate` at the start of
    /// `out`, writing its header.
    ///
    /// # Panics
    ///
    /// If `channels` is 0.
    pub fn new(mut out: W, sample_rate: u32, channels: u16) -> io::Result<Self> {
        assert!(channels > 0, "a WAV file needs a channel");
        let mut bytes = Vec::with_capacity(HEADER);
        header(&mut bytes, sample_rate, SampleFormat::Float32, channels, 0);
        out.seek(SeekFrom::Start(0))?;
        out.write_all(&bytes)?;
        Ok(Self {
            out,
            sample_rate,
            channels,
            frames: 0,
            buffer: Vec::new(),
        })
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

    /// How many more frames the file has room for: a WAV file holds at most
    /// 4 GiB.
    pub fn room(&self) -> u64 {
        MAX_DATA / (u64::from(self.channels) * 4) - self.frames
    }

    /// Writes `samples`, whole frames of interleaved channels, after the
    /// ones before.
    ///
    /// # Panics
    ///
    /// If `samples` isn't whole frames, or the file hasn't room for them.
    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        let channels = usize::from(self.channels);
        assert!(
            samples.len().is_multiple_of(channels),
            "{} samples aren't whole frames of {channels} channels",
            samples.len()
        );
        let frames = (samples.len() / channels) as u64;
        assert!(frames <= self.room(), "a WAV file holds 4 GiB at most");
        self.buffer.clear();
        self.buffer.reserve(samples.len() * 4);
        for sample in samples {
            self.buffer.extend_from_slice(&sample.to_le_bytes());
        }
        self.out.write_all(&self.buffer)?;
        self.frames += frames;
        Ok(())
    }

    /// Writes silence for `frames` frames.
    pub fn write_silence(&mut self, frames: u64) -> io::Result<()> {
        let chunk = vec![0.0; 4096 * usize::from(self.channels)];
        let mut left = frames;
        while left > 0 {
            let n = left.min(4096) as usize;
            self.write(&chunk[..n * usize::from(self.channels)])?;
            left -= n as u64;
        }
        Ok(())
    }

    /// Puts the length so far in the header, and flushes `out`.
    pub fn flush(&mut self) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(HEADER);
        header(
            &mut bytes,
            self.sample_rate,
            SampleFormat::Float32,
            self.channels,
            self.frames,
        );
        let end = self.out.stream_position()?;
        self.out.seek(SeekFrom::Start(0))?;
        self.out.write_all(&bytes)?;
        self.out.seek(SeekFrom::Start(end))?;
        self.out.flush()
    }

    /// Finishes the file and hands back `out`. Float samples are 4 bytes,
    /// so the samples never need a pad byte.
    pub fn finish(mut self) -> io::Result<W> {
        self.flush()?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::super::tests::{decode, signal};
    use super::super::{Decoder, Encoding, Format};
    use super::*;

    /// A WAV file of `samples`, with a fmt chunk of `fmt`, and `extra`
    /// chunks before the data. `data_size` overrides the data chunk's size.
    fn wav(
        fmt: &[u8],
        extra: &[(&[u8; 4], &[u8])],
        samples: &[u8],
        data_size: Option<u32>,
    ) -> Vec<u8> {
        let mut chunks = Vec::new();
        let mut chunk = |id: &[u8; 4], body: &[u8], size: Option<u32>| {
            chunks.extend_from_slice(id);
            chunks.extend_from_slice(&size.unwrap_or(body.len() as u32).to_le_bytes());
            chunks.extend_from_slice(body);
            if body.len() % 2 == 1 {
                chunks.push(0);
            }
        };
        for &(id, body) in extra {
            chunk(id, body, None);
        }
        chunk(b"fmt ", fmt, None);
        chunk(b"data", samples, data_size);
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(4 + chunks.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(&chunks);
        bytes
    }

    fn fmt(tag: u16, channels: u16, bits: u16) -> Vec<u8> {
        let block = channels * bits / 8;
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&44_100u32.to_le_bytes());
        fmt.extend_from_slice(&(44_100 * u32::from(block)).to_le_bytes());
        fmt.extend_from_slice(&block.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt
    }

    /// An ACID chunk's body, for a loop of `beats` beats in 4/4 at `tempo`.
    fn acid(flags: u32, beats: u32, tempo: f32) -> Vec<u8> {
        let mut acid = flags.to_le_bytes().to_vec();
        // The root note, and two fields no one uses.
        acid.extend_from_slice(&60u16.to_le_bytes());
        acid.extend_from_slice(&0x8000u16.to_le_bytes());
        acid.extend_from_slice(&0f32.to_le_bytes());
        acid.extend_from_slice(&beats.to_le_bytes());
        acid.extend_from_slice(&4u16.to_le_bytes());
        acid.extend_from_slice(&4u16.to_le_bytes());
        acid.extend_from_slice(&tempo.to_le_bytes());
        acid
    }

    #[test]
    fn a_loop_s_tempo_comes_from_its_acid_chunk() {
        // A second of mono 16-bit samples.
        let samples = vec![0; 2 * 44_100];
        let before = |body: &[u8]| {
            let file = wav(&fmt(PCM, 1, 16), &[(b"acid", body)], &samples, None);
            decode(file).tempo()
        };
        // As it's stored, to a ten-thousandth.
        assert_eq!(before(&acid(0, 2, 97.3)), Some(97.3));
        // A one-shot isn't a loop.
        assert_eq!(before(&acid(1, 2, 97.3)), None);
        // Without a tempo, two beats in a second are 120 BPM.
        assert_eq!(before(&acid(0, 2, 0.0)), Some(120.0));
        assert_eq!(before(&acid(0, 0, 0.0)), None);
        assert_eq!(before(&acid(0, 2, 97.3)[..20]), None, "a short chunk");
        // After the samples, as some editors put it.
        let mut file = wav(&fmt(PCM, 1, 16), &[(b"LIST", b"odd")], &samples, None);
        file.extend_from_slice(b"acid");
        file.extend_from_slice(&24u32.to_le_bytes());
        file.extend_from_slice(&acid(0, 8, 128.0));
        let mut decoder = decode(file);
        assert_eq!(decoder.tempo(), Some(128.0));
        assert_eq!(decoder.read_all().unwrap()[0].len(), 44_100);
        let plain = decode(wav(&fmt(PCM, 1, 16), &[], &samples, None));
        assert_eq!(plain.tempo(), None);
    }

    #[test]
    fn eight_bit_files_are_unsigned() {
        let mut decoder = decode(wav(&fmt(PCM, 1, 8), &[], &[0, 128, 255, 64], None));
        assert_eq!(
            decoder.read_all().unwrap()[0],
            [-1.0, 0.0, 127.0 / 128.0, -0.5]
        );
    }

    #[test]
    fn eight_bit_files_are_written_unsigned() {
        let mut bytes = Vec::new();
        write(
            &mut bytes,
            8000,
            SampleFormat::Int8,
            &[&[-1.0, 0.0, 0.5, 1.0]],
        )
        .unwrap();
        // After a 44-byte header.
        assert_eq!(bytes[44..], [0, 128, 192, 255]);
        let mut decoder = decode(bytes);
        assert_eq!(decoder.encoding(), Encoding::Int(8));
        assert_eq!(
            decoder.read_all().unwrap()[0],
            [-1.0, 0.0, 0.5, 127.0 / 128.0]
        );
    }

    #[test]
    fn extensible_files_and_odd_chunks_before_the_data() {
        let mut extensible = fmt(EXTENSIBLE, 2, 32);
        extensible.extend_from_slice(&22u16.to_le_bytes());
        extensible.extend_from_slice(&32u16.to_le_bytes());
        extensible.extend_from_slice(&3u32.to_le_bytes());
        // The float subformat GUID.
        extensible.extend_from_slice(&[3, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
        let samples: Vec<u8> = [0.25f32, -0.5, 1.0, 0.125]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let extra: [(&[u8; 4], &[u8]); 2] = [(b"LIST", b"odd"), (b"bext", &[0; 10])];
        let mut decoder = decode(wav(&extensible, &extra, &samples, None));
        assert_eq!(decoder.info().channels, 2);
        assert_eq!(
            decoder.read_all().unwrap(),
            [vec![0.25, 1.0], vec![-0.5, 0.125]]
        );
    }

    #[test]
    fn a_wav_writer_writes_as_the_samples_come() {
        let mut writer = WavWriter::new(Cursor::new(Vec::new()), 44_100, 2).unwrap();
        let samples: Vec<f32> = (0..200).map(|i| i as f32 / 256.0 - 0.5).collect();
        writer.write(&samples[..100]).unwrap();
        // Cut off before the length was written: it runs to the end.
        let cut = writer.out.get_ref().clone();
        let decoder = decode(cut);
        assert_eq!(decoder.info().frames, 50);
        assert_eq!(decoder.info().sample_rate, 44_100);
        writer.flush().unwrap();
        writer.write(&samples[100..]).unwrap();
        writer.write_silence(3).unwrap();
        assert_eq!(writer.frames(), 103);
        let bytes = writer.finish().unwrap().into_inner();
        let mut decoder = decode(bytes.clone());
        assert_eq!(decoder.info().frames, 103);
        let channels = decoder.read_all().unwrap();
        let left: Vec<f32> = samples.iter().step_by(2).copied().chain([0.0; 3]).collect();
        assert_eq!(channels[0], left);
        // What write_wav writes for the same samples.
        let right: Vec<f32> = samples
            .iter()
            .skip(1)
            .step_by(2)
            .copied()
            .chain([0.0; 3])
            .collect();
        let mut whole = Vec::new();
        write(
            &mut whole,
            44_100,
            SampleFormat::Float32,
            &[&left[..], &right[..]],
        )
        .unwrap();
        assert_eq!(bytes, whole);
    }

    #[test]
    fn a_wav_writer_has_room_for_4_gib() {
        let writer = WavWriter::new(Cursor::new(Vec::new()), 48_000, 2).unwrap();
        assert_eq!(writer.room(), (u64::from(u32::MAX) - 64) / 8);
    }

    #[test]
    fn a_data_chunk_that_runs_past_the_end_stops_there() {
        let samples: Vec<u8> = [1i16, 2, 3].iter().flat_map(|s| s.to_le_bytes()).collect();
        for size in [Some(0), Some(u32::MAX), Some(1000)] {
            let decoder = decode(wav(&fmt(PCM, 1, 16), &[], &samples, size));
            assert_eq!(decoder.info().frames, 3, "{size:?}");
        }
        // Half a frame at the end doesn't count.
        let decoder = decode(wav(&fmt(PCM, 2, 16), &[], &samples, None));
        assert_eq!(decoder.info().frames, 1);
    }

    #[test]
    fn twenty_four_and_thirty_two_bit_integers() {
        let samples = [0x7F_FF_FFu32, 0x80_00_00, 0x40_00_00];
        let bytes: Vec<u8> = samples
            .iter()
            .flat_map(|s| s.to_le_bytes()[..3].to_vec())
            .collect();
        let mut decoder = decode(wav(&fmt(PCM, 1, 24), &[], &bytes, None));
        assert_eq!(
            decoder.read_all().unwrap()[0],
            [8_388_607.0 / 8_388_608.0, -1.0, 0.5]
        );
        let bytes: Vec<u8> = [i32::MIN, 1 << 30]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let mut decoder = decode(wav(&fmt(PCM, 1, 32), &[], &bytes, None));
        assert_eq!(decoder.read_all().unwrap()[0], [-1.0, 0.5]);
    }

    #[test]
    fn compressed_and_broken_files_are_refused() {
        let adpcm = wav(&fmt(2, 1, 4), &[], &[0; 8], None);
        let refused = Decoder::new(Box::new(Cursor::new(adpcm)));
        assert!(matches!(refused, Err(AudioError::Unsupported(_))));
        // Without its fmt chunk: 12 bytes of header, then 8 + 16 of fmt.
        let whole = wav(&fmt(PCM, 1, 16), &[], &[0; 4], None);
        let no_fmt = [&whole[..12], &whole[36..]].concat();
        let refused = Decoder::new(Box::new(Cursor::new(no_fmt)));
        assert!(matches!(refused, Err(AudioError::Invalid(_))));
    }

    #[test]
    fn written_files_are_padded_to_even_chunks() {
        let signal = signal(1, 11);
        let mut bytes = Vec::new();
        write(&mut bytes, 8000, SampleFormat::Int24, &[&signal[0]]).unwrap();
        // 33 bytes of samples and a byte of padding.
        assert_eq!(bytes.len(), 44 + 34);
        let riff = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(riff as usize, bytes.len() - 8);
        assert_eq!(decode(bytes).info().format, Format::Wav);
    }
}
