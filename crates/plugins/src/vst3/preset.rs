//! VST3's preset files, `.vstpreset`, which are also how a project keeps a
//! VST3 plugin's state: the component's state and the controller's, and
//! the class they're for.
//!
//! A file is a header, the chunks, then a list of where the chunks are,
//! all little-endian. The header is `VST3`, version 1, the class id as 32
//! hex digits, and where the list starts. The list is `List`, how many
//! chunks there are, and for each, its id, where it starts and its size.

/// The header's size.
const HEADER: usize = 48;
/// One entry in the list: the chunk's id, where it starts, and its size.
const ENTRY: usize = 20;
const COMPONENT: &[u8; 4] = b"Comp";
const CONTROLLER: &[u8; 4] = b"Cont";

/// What's in a preset file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Preset<'a> {
    /// As [`class_id_string`](super::module::class_id_string) writes it.
    pub(crate) class_id: &'a str,
    pub(crate) component: &'a [u8],
    pub(crate) controller: Option<&'a [u8]>,
}

/// A preset file holding `preset`.
pub(crate) fn write(preset: &Preset<'_>) -> Vec<u8> {
    let controller = preset.controller.unwrap_or_default();
    let mut file = Vec::with_capacity(HEADER + preset.component.len() + controller.len() + 48);
    file.extend_from_slice(b"VST3");
    file.extend_from_slice(&1i32.to_le_bytes());
    let mut class_id = [b'0'; 32];
    for (to, from) in class_id.iter_mut().zip(preset.class_id.bytes()) {
        *to = from;
    }
    file.extend_from_slice(&class_id);
    file.extend_from_slice(&0i64.to_le_bytes());
    let mut chunks = Vec::new();
    let all = [
        (COMPONENT, Some(preset.component)),
        (CONTROLLER, preset.controller),
    ];
    for (id, data) in all {
        if let Some(data) = data {
            chunks.push((id, file.len(), data.len()));
            file.extend_from_slice(data);
        }
    }
    let list = file.len() as i64;
    file[40..HEADER].copy_from_slice(&list.to_le_bytes());
    file.extend_from_slice(b"List");
    file.extend_from_slice(&(chunks.len() as i32).to_le_bytes());
    for (id, at, size) in chunks {
        file.extend_from_slice(id);
        file.extend_from_slice(&(at as i64).to_le_bytes());
        file.extend_from_slice(&(size as i64).to_le_bytes());
    }
    file
}

/// What's in a preset file, unless it isn't one, or has no component
/// state.
pub(crate) fn read(file: &[u8]) -> Option<Preset<'_>> {
    let int = |at: usize| Some(i32::from_le_bytes(file.get(at..at + 4)?.try_into().ok()?));
    let long = |at: usize| {
        let value = i64::from_le_bytes(file.get(at..at + 8)?.try_into().ok()?);
        usize::try_from(value).ok()
    };
    if file.get(..4)? != b"VST3" || int(4)? < 1 {
        return None;
    }
    let class_id = std::str::from_utf8(file.get(8..40)?).ok()?;
    let list = long(40)?;
    if file.get(list..list.checked_add(4)?)? != b"List" {
        return None;
    }
    let count = usize::try_from(int(list + 4)?).ok()?;
    let mut component = None;
    let mut controller = None;
    for i in 0..count {
        let entry = list + 8 + i.checked_mul(ENTRY)?;
        let id = file.get(entry..entry + 4)?;
        let (at, size) = (long(entry + 4)?, long(entry + 12)?);
        let data = file.get(at..at.checked_add(size)?)?;
        if id == COMPONENT {
            component = Some(data);
        } else if id == CONTROLLER {
            controller = Some(data);
        }
    }
    Some(Preset {
        class_id,
        component: component?,
        controller,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_read_as_they_are_written() {
        let preset = Preset {
            class_id: "123456789ABCDEF00F1E2D3C4B5A6978",
            component: b"gain=0.5",
            controller: Some(b"zoom=2"),
        };
        let file = write(&preset);
        assert_eq!(&file[..8], b"VST3\x01\x00\x00\x00");
        assert_eq!(&file[8..40], preset.class_id.as_bytes());
        assert_eq!(read(&file), Some(preset));
        let alone = Preset {
            controller: None,
            ..preset
        };
        assert_eq!(read(&write(&alone)), Some(alone));
        // Anything else isn't a preset, and neither is a cut-off one.
        assert_eq!(read(b"nonsense"), None);
        assert_eq!(read(&file[..file.len() - 1]), None);
    }
}
