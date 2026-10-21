//! Upgrading files saved in older versions of the format.
//!
//! Each step takes a file up one version, working on the parsed JSON, so old
//! versions need no Rust types of their own. Steps are kept forever: a file
//! from any version goes through every step from its version to the current
//! one, and then reads like a file of the current version.

use serde_json::{Map, Value};

use super::{Invalid, VERSION};

/// Upgrades a file from one version to the next. It gets the whole file and
/// leaves its `version` field to [`upgrade`]. It returns an error only for a
/// file that is already broken, naming the field.
type Step = fn(&mut Map<String, Value>) -> Result<(), Invalid>;

/// One step for each version after the first, oldest first: `STEPS[0]` takes
/// version 1 files to version 2. Raising [`VERSION`] without adding its step
/// doesn't compile.
const STEPS: [Step; VERSION as usize - 1] = [];

/// Upgrades `document`, a file of version `from`, to [`VERSION`].
pub(super) fn upgrade(document: &mut Value, from: u32) -> Result<(), Invalid> {
    run(&STEPS, document, from)
}

fn run(steps: &[Step], document: &mut Value, from: u32) -> Result<(), Invalid> {
    let file = document
        .as_object_mut()
        .expect("the header was checked, so the file is an object");
    let first = from as usize - 1;
    for (step, to) in steps[first..].iter().zip(from + 1..) {
        step(file)?;
        file.insert("version".to_owned(), to.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A made-up version 2 that renamed `metronome` to `click`.
    fn rename_metronome(file: &mut Map<String, Value>) -> Result<(), Invalid> {
        let on = file
            .remove("metronome")
            .ok_or_else(|| Invalid::new("metronome", "is missing"))?;
        file.insert("click".to_owned(), on);
        Ok(())
    }

    /// And a made-up version 3 that added a swing amount to every track.
    fn add_swing(file: &mut Map<String, Value>) -> Result<(), Invalid> {
        let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
        for track in tracks.into_iter().flatten() {
            if let Some(track) = track.as_object_mut() {
                track.insert("swing".to_owned(), 0.into());
            }
        }
        Ok(())
    }

    const MADE_UP: [Step; 2] = [rename_metronome, add_swing];

    #[test]
    fn files_go_through_every_step_after_their_version() {
        let mut v1 = json!({"version": 1, "metronome": true, "tracks": [{"id": 0}]});
        run(&MADE_UP, &mut v1, 1).unwrap();
        assert_eq!(
            v1,
            json!({"version": 3, "click": true, "tracks": [{"id": 0, "swing": 0}]})
        );

        let mut v2 = json!({"version": 2, "click": false, "tracks": [{"id": 4}]});
        run(&MADE_UP, &mut v2, 2).unwrap();
        assert_eq!(
            v2,
            json!({"version": 3, "click": false, "tracks": [{"id": 4, "swing": 0}]})
        );

        let v3 = json!({"version": 3, "click": false, "tracks": []});
        let mut unchanged = v3.clone();
        run(&MADE_UP, &mut unchanged, 3).unwrap();
        assert_eq!(unchanged, v3);
    }

    #[test]
    fn a_step_can_refuse_a_broken_file() {
        let mut broken = json!({"version": 1, "tracks": []});
        assert_eq!(
            run(&MADE_UP, &mut broken, 1),
            Err(Invalid::new("metronome", "is missing"))
        );
    }

    #[test]
    fn current_files_need_no_steps() {
        let current = json!({"version": VERSION, "tracks": []});
        let mut upgraded = current.clone();
        upgrade(&mut upgraded, VERSION).unwrap();
        assert_eq!(upgraded, current);
    }
}
