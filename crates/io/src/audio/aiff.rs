//! AIFF and AIFF-C files: IFF chunks, big-endian.

use std::io::{self, Read, Seek, SeekFrom, Write};

use super::{AudioError, Layout, Pcm, SampleFormat, Source, frames_of, invalid, unsupported};

/// The AIFF-C version its FVER chunk names: May 23, 1990, 2:40 pm.
const AIFC_VERSION: u32 = 0xA280_5140;

/// What a COMM chunk says.
struct Common {
    channels: usize,
    frames: u64,
    sample_rate: u32,
    layout: Layout,
}

/// Finds the format and the samples of an AIFF or AIFF-C file.
pub(super) fn open(mut source: Box<dyn Source>) -> Result<Pcm, AudioError> {
    let mut form = [0; 12];
    source.read_exact(&mut form)?;
    let compressed = &form[8..] == b"AIFC";
    let mut common = None;
    let mut sound = None;
    loop {
        let mut header = [0; 8];
        if super::read_up_to(source.as_mut(), &mut header)? < 8 {
            break;
        }
        let size = u32::from_be_bytes(header[4..].try_into().expect("four bytes"));
        let next = source.stream_position()? + u64::from(size) + u64::from(size % 2);
        match &header[..4] {
            b"COMM" => {
                let mut body = vec![0; size as usize];
                source
                    .read_exact(&mut body)
                    .map_err(|_| AudioError::Invalid("a short COMM chunk".into()))?;
                common = Some(parse_common(&body, compressed)?);
            }
            b"SSND" => {
                let mut offsets = [0; 8];
                source.read_exact(&mut offsets)?;
                let offset = u32::from_be_bytes(offsets[..4].try_into().expect("four bytes"));
                let start = source.stream_position()? + u64::from(offset);
                let bytes = u64::from(size).saturating_sub(8 + u64::from(offset));
                sound = Some((start, bytes));
            }
            _ => {}
        }
        source.seek(SeekFrom::Start(next))?;
    }
    let Some(common) = common else {
        return invalid("no COMM chunk");
    };
    let (start, bytes) = match sound {
        Some(sound) => sound,
        // No samples at all is allowed, if there are none to have.
        None if common.frames == 0 => (0, 0),
        None => return invalid("no SSND chunk"),
    };
    let frame = (common.channels * common.layout.bytes()) as u64;
    let bytes = bytes.min(common.frames.saturating_mul(frame));
    Pcm::new(
        source,
        start,
        bytes,
        common.channels,
        common.sample_rate,
        common.layout,
    )
}

fn parse_common(body: &[u8], compressed: bool) -> Result<Common, AudioError> {
    if body.len() < 18 || (compressed && body.len() < 22) {
        return invalid("a short COMM chunk");
    }
    let channels = usize::try_from(i16::from_be_bytes([body[0], body[1]]))
        .map_err(|_| AudioError::Invalid("a negative number of channels".into()))?;
    let frames = u64::from(u32::from_be_bytes(
        body[2..6].try_into().expect("four bytes"),
    ));
    let bits = i16::from_be_bytes([body[6], body[7]]);
    let sample_rate = extended_to_rate(body[8..18].try_into().expect("ten bytes"))?;
    let compression: [u8; 4] = match compressed {
        true => body[18..22].try_into().expect("four bytes"),
        false => *b"NONE",
    };
    let integer = |big: bool| match bits {
        1..=8 => Ok(Layout::I8),
        9..=16 => Ok(Layout::I16 { big }),
        17..=24 => Ok(Layout::I24 { big }),
        25..=32 => Ok(Layout::I32 { big }),
        _ => unsupported(format!("{bits}-bit AIFF samples")),
    };
    let layout = match &compression {
        b"NONE" | b"twos" => integer(true)?,
        b"sowt" => integer(false)?,
        b"fl32" | b"FL32" => Layout::F32 { big: true },
        b"fl64" | b"FL64" => Layout::F64 { big: true },
        other => {
            let name = String::from_utf8_lossy(other);
            return unsupported(format!("AIFF-C compression {name:?}"));
        }
    };
    Ok(Common {
        channels,
        frames,
        sample_rate,
        layout,
    })
}

/// An 80-bit extended float, as AIFF stores the sample rate, to the nearest
/// whole number of Hz.
fn extended_to_rate(bytes: [u8; 10]) -> Result<u32, AudioError> {
    let exponent = i32::from(u16::from_be_bytes([bytes[0], bytes[1]]) & 0x7FFF);
    let negative = bytes[0] & 0x80 != 0;
    let mantissa = u64::from_be_bytes(bytes[2..].try_into().expect("eight bytes"));
    if mantissa == 0 {
        return Ok(0);
    }
    let rate = mantissa as f64 * 2f64.powi(exponent - 16_383 - 63);
    if negative || !(1.0..=f64::from(u32::MAX)).contains(&rate) {
        return invalid("a sample rate out of range");
    }
    Ok(rate.round() as u32)
}

/// A whole number as an 80-bit extended float.
fn rate_to_extended(rate: u32) -> [u8; 10] {
    let mut bytes = [0; 10];
    if rate == 0 {
        return bytes;
    }
    let shift = u64::from(rate).leading_zeros();
    let exponent = (16_383 + 63 - shift) as u16;
    bytes[..2].copy_from_slice(&exponent.to_be_bytes());
    bytes[2..].copy_from_slice(&(u64::from(rate) << shift).to_be_bytes());
    bytes
}

/// The most bytes [`header`] writes.
pub(super) const HEADER: usize = 84;

/// The header of an AIFF file, or of an AIFF-C file for floats, with
/// `frames` frames of `channels` channels of `format` samples at
/// `sample_rate`, up to the start of its samples. Its chunks' sizes
/// can't say more than 4 GiB.
pub(super) fn header(
    sample_rate: u32,
    format: SampleFormat,
    channels: u16,
    frames: u64,
) -> io::Result<Vec<u8>> {
    let size = format.bytes() as u64;
    let float = format.is_float();
    let data = u64::from(channels) * frames * size;
    let too_long = || io::Error::new(io::ErrorKind::InvalidInput, "too long for an AIFF file");
    let frame_count = u32::try_from(frames).map_err(|_| too_long())?;
    let mut chunks = Vec::with_capacity(HEADER);
    if float {
        chunks.extend_from_slice(b"FVER");
        chunks.extend_from_slice(&4u32.to_be_bytes());
        chunks.extend_from_slice(&AIFC_VERSION.to_be_bytes());
    }
    chunks.extend_from_slice(b"COMM");
    // The compression name is a Pascal string: a length byte, and padding
    // to an even length.
    let (compression, name): (&[u8; 4], &[u8]) = match format {
        SampleFormat::Float32 => (b"fl32", b"\x0c32-bit float\0"),
        SampleFormat::Float64 => (b"fl64", b"\x0c64-bit float\0"),
        _ => (b"NONE", b""),
    };
    let common = 18 + if float { 4 + name.len() } else { 0 };
    chunks.extend_from_slice(&(common as u32).to_be_bytes());
    chunks.extend_from_slice(&(channels as i16).to_be_bytes());
    chunks.extend_from_slice(&frame_count.to_be_bytes());
    chunks.extend_from_slice(&(size as i16 * 8).to_be_bytes());
    chunks.extend_from_slice(&rate_to_extended(sample_rate));
    if float {
        chunks.extend_from_slice(compression);
        chunks.extend_from_slice(name);
    }
    chunks.extend_from_slice(b"SSND");
    let sound = u32::try_from(8 + data).map_err(|_| too_long())?;
    chunks.extend_from_slice(&sound.to_be_bytes());
    chunks.extend_from_slice(&[0; 8]);
    let form = 4 + chunks.len() as u64 + data + data % 2;
    let form = u32::try_from(form).map_err(|_| too_long())?;
    let mut header = Vec::with_capacity(HEADER);
    header.extend_from_slice(b"FORM");
    header.extend_from_slice(&form.to_be_bytes());
    header.extend_from_slice(if float { b"AIFC" } else { b"AIFF" });
    header.extend_from_slice(&chunks);
    Ok(header)
}

pub(super) fn write(
    out: &mut impl Write,
    sample_rate: u32,
    format: SampleFormat,
    channels: &[&[f32]],
) -> io::Result<()> {
    let frames = frames_of(channels)?;
    let count = u16::try_from(channels.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many channels"))?;
    let mut bytes = header(sample_rate, format, count, frames as u64)?;
    let data = channels.len() * frames * format.bytes();
    bytes.reserve(data + 1);
    let mut sample = Vec::with_capacity(4);
    for i in 0..frames {
        for channel in channels {
            sample.clear();
            format.encode(f64::from(channel[i]), &mut sample);
            sample.reverse();
            bytes.extend_from_slice(&sample);
        }
    }
    if data % 2 == 1 {
        bytes.push(0);
    }
    out.write_all(&bytes)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::super::tests::decode;
    use super::super::{Decoder, Format};
    use super::*;

    #[test]
    fn sample_rates_go_both_ways() {
        for rate in [1, 8_000, 22_050, 44_100, 48_000, 96_000, 192_000, 384_000] {
            assert_eq!(extended_to_rate(rate_to_extended(rate)).unwrap(), rate);
        }
        // 44.1 kHz as other programs write it.
        let written = [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0];
        assert_eq!(rate_to_extended(44_100), written);
    }

    /// An AIFF-C file with `compression` and `samples`, 16 bits.
    fn aifc(compression: &[u8; 4], bits: i16, samples: &[u8], frames: u32) -> Vec<u8> {
        let mut chunks = b"COMM".to_vec();
        chunks.extend_from_slice(&24u32.to_be_bytes());
        chunks.extend_from_slice(&1i16.to_be_bytes());
        chunks.extend_from_slice(&frames.to_be_bytes());
        chunks.extend_from_slice(&bits.to_be_bytes());
        chunks.extend_from_slice(&rate_to_extended(48_000));
        chunks.extend_from_slice(compression);
        chunks.extend_from_slice(&[0, 0]);
        chunks.extend_from_slice(b"SSND");
        chunks.extend_from_slice(&(8 + samples.len() as u32).to_be_bytes());
        chunks.extend_from_slice(&[0; 8]);
        chunks.extend_from_slice(samples);
        let mut bytes = b"FORM".to_vec();
        bytes.extend_from_slice(&(4 + chunks.len() as u32).to_be_bytes());
        bytes.extend_from_slice(b"AIFC");
        bytes.extend_from_slice(&chunks);
        bytes
    }

    #[test]
    fn either_byte_order() {
        let big = decode(aifc(b"twos", 16, &[0x40, 0, 0xC0, 0], 2))
            .read_all()
            .unwrap();
        let little = decode(aifc(b"sowt", 16, &[0, 0x40, 0, 0xC0], 2))
            .read_all()
            .unwrap();
        assert_eq!(big[0], [0.5, -0.5]);
        assert_eq!(little, big);
    }

    #[test]
    fn floats_and_eight_bit_integers() {
        let samples: Vec<u8> = [0.75f32, -0.25]
            .iter()
            .flat_map(|s| s.to_be_bytes())
            .collect();
        let mut floats = decode(aifc(b"fl32", 32, &samples, 2));
        assert_eq!(floats.read_all().unwrap()[0], [0.75, -0.25]);
        // AIFF's 8-bit samples are signed.
        let mut bytes = decode(aifc(b"NONE", 8, &[0x40, 0x80], 2));
        assert_eq!(bytes.read_all().unwrap()[0], [0.5, -1.0]);
    }

    #[test]
    fn the_frame_count_and_the_samples_both_limit_the_length() {
        assert_eq!(decode(aifc(b"NONE", 16, &[0; 8], 3)).info().frames, 3);
        assert_eq!(decode(aifc(b"NONE", 16, &[0; 4], 3)).info().frames, 2);
    }

    #[test]
    fn compressed_files_are_refused() {
        let refused = Decoder::new(Box::new(Cursor::new(aifc(b"ima4", 16, &[0; 4], 2))));
        assert!(matches!(refused, Err(AudioError::Unsupported(_))));
    }

    #[test]
    fn written_floats_are_aiff_c() {
        for (format, compression) in [
            (SampleFormat::Float32, b"fl32\x0c32-bit float\0"),
            (SampleFormat::Float64, b"fl64\x0c64-bit float\0"),
        ] {
            let mut bytes = Vec::new();
            write(&mut bytes, 48_000, format, &[&[0.5, 0.25]]).unwrap();
            assert_eq!(&bytes[8..12], b"AIFC");
            // After FVER and COMM's first 18 bytes.
            assert_eq!(&bytes[50..68], compression);
            assert_eq!(bytes.len(), HEADER + 2 * format.bytes());
            let mut decoder = decode(bytes);
            assert_eq!(decoder.info().format, Format::Aiff);
            assert_eq!(decoder.encoding(), format.encoding());
            assert_eq!(decoder.read_all().unwrap()[0], [0.5, 0.25]);
        }
    }
}
