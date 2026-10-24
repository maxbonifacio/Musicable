//! How theme files write a theme's values: colours as `"#rrggbb"`, or
//! `"#rrggbbaa"` with an opacity, sizes as numbers of points, and pairs of
//! sizes, across and down, as `[8, 3]`.

use egui::{Color32, Vec2, vec2};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `colour` as a theme file writes it: `"#rrggbb"` when it's opaque, and
/// `"#rrggbbaa"` otherwise, with its opacity last. The red, green and blue
/// are as they'd look opaque, as in CSS.
pub(crate) fn to_hex(colour: Color32) -> String {
    let [r, g, b, a] = colour.to_srgba_unmultiplied();
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}

/// The colour `text` writes as [`to_hex`] does, in either case. `None` if it
/// isn't one.
pub(crate) fn parse_colour(text: &str) -> Option<Color32> {
    let hex = text.strip_prefix('#')?;
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    match hex.len() {
        6 => Some(Color32::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color32::from_rgba_unmultiplied(
            byte(0)?,
            byte(2)?,
            byte(4)?,
            byte(6)?,
        )),
        _ => None,
    }
}

/// A colour field, as `#[serde(with = "colour")]`.
pub(crate) mod colour {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(colour: &Color32, s: S) -> Result<S::Ok, S::Error> {
        to_hex(*colour).serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color32, D::Error> {
        let text = String::deserialize(d)?;
        parse_colour(&text).ok_or_else(|| D::Error::custom(format!("\"{text}\" isn't a colour")))
    }
}

/// A list of colours of a fixed length, such as the clips', as
/// `#[serde(with = "colours")]`.
pub(crate) mod colours {
    use super::*;

    pub(crate) fn serialize<S: Serializer, const N: usize>(
        colours: &[Color32; N],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let hex: Vec<String> = colours.iter().map(|&colour| to_hex(colour)).collect();
        hex.serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<[Color32; N], D::Error> {
        let texts = Vec::<String>::deserialize(d)?;
        let colours: Vec<Color32> = texts
            .iter()
            .map(|text| {
                parse_colour(text)
                    .ok_or_else(|| D::Error::custom(format!("\"{text}\" isn't a colour")))
            })
            .collect::<Result<_, _>>()?;
        let count = colours.len();
        colours
            .try_into()
            .map_err(|_| D::Error::custom(format!("{count} colours where there should be {N}")))
    }
}

/// A pair of sizes, across and down, as `#[serde(with = "pair")]`.
pub(crate) mod pair {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(pair: &Vec2, s: S) -> Result<S::Ok, S::Error> {
        [pair.x, pair.y].serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec2, D::Error> {
        let [x, y] = <[f32; 2]>::deserialize(d)?;
        Ok(vec2(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_written_in_hex() {
        assert_eq!(to_hex(Color32::from_rgb(255, 136, 0)), "#ff8800");
        // The opacity is multiplied in, so a faint colour comes back a
        // little off.
        let shade = Color32::from_rgba_unmultiplied(215, 160, 60, 14);
        assert_eq!(to_hex(shade), "#dba4370e");
        assert_eq!(to_hex(Color32::TRANSPARENT), "#00000000");
        assert_eq!(parse_colour("#ffffff00"), Some(Color32::TRANSPARENT));
        assert_eq!(
            parse_colour("#FF8800"),
            Some(Color32::from_rgb(255, 136, 0))
        );
        assert_eq!(parse_colour("#ff880080").map(|c| c.a()), Some(128));
        for text in [
            "ff8800",
            "#ff88",
            "#ff88000",
            "#gg8800",
            "#ff8800ff00",
            "",
            "#",
        ] {
            assert_eq!(parse_colour(text), None, "{text:?}");
        }
    }

    #[test]
    fn every_colour_survives_being_written_and_read() {
        // Colours keep their opacity multiplied in, so writing them as they'd
        // look opaque and reading them back must land on the same colour.
        for a in 0..=255u8 {
            for value in (0..=255u8).step_by(5) {
                let colour = Color32::from_rgba_unmultiplied(value, 255 - value, value / 2, a);
                assert_eq!(parse_colour(&to_hex(colour)), Some(colour), "{colour:?}");
            }
        }
    }
}
