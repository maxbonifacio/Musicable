//! Finding commands by what the user types, in the command palette and the
//! keyboard shortcuts window.

use musicable_commands::KeyChord;

/// How well `query` matches a command, or `None` if it doesn't. Higher is
/// better. `label` is the command's area and title, as the palette shows it
/// ("Transport: Toggle Loop"), and `chords` are its keys.
///
/// Every word of the query has to be in the label or the command's id. A
/// word counts most at the start of a word of the label ("loop", "tog"), less
/// inside one or as the initials of the label's words ("tl"), and least in
/// the id. A query that names a key chord, such as "ctrl+l" or "space",
/// matches the commands with exactly that chord too, ahead of the rest. Part
/// of a chord, with a `+` in it, matches the chords it starts, so
/// "ctrl+shift" finds every Ctrl+Shift chord.
pub(crate) fn score(query: &str, label: &str, id: &str, chords: &[KeyChord]) -> Option<u32> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let keys = match query.parse::<KeyChord>() {
        Ok(chord) => chords.contains(&chord).then_some(10),
        Err(_) => {
            let prefix: String = query.split_whitespace().collect();
            let starts = |chord: &KeyChord| chord.to_string().to_lowercase().starts_with(&prefix);
            (query.contains('+') && chords.iter().any(starts)).then_some(5)
        }
    };
    let text = text_score(&query, &label.to_lowercase(), &id.to_lowercase());
    match (keys, text) {
        (None, None) => None,
        (keys, text) => Some(keys.unwrap_or(0) + text.unwrap_or(0)),
    }
}

/// How well the words of `query` match the label and the id, all in lower
/// case, or `None` if a word isn't in either.
fn text_score(query: &str, label: &str, id: &str) -> Option<u32> {
    let words: Vec<&str> = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let initials: String = words
        .iter()
        .filter_map(|word| word.chars().next())
        .collect();
    query
        .split_whitespace()
        .map(|term| {
            if words.iter().any(|word| word.starts_with(term)) {
                Some(3)
            } else if label.contains(term) || (term.len() > 1 && initials.contains(term)) {
                Some(2)
            } else if id.contains(term) {
                Some(1)
            } else {
                None
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chords(texts: &[&str]) -> Vec<KeyChord> {
        texts.iter().map(|text| text.parse().unwrap()).collect()
    }

    fn loop_score(query: &str) -> Option<u32> {
        let keys = chords(&["Ctrl+L"]);
        score(
            query,
            "Transport: Toggle Loop",
            "transport.toggle_loop",
            &keys,
        )
    }

    #[test]
    fn every_word_has_to_match() {
        assert_eq!(loop_score(""), Some(0));
        assert_eq!(loop_score("  "), Some(0));
        assert_eq!(loop_score("loop"), Some(3));
        assert_eq!(loop_score("Toggle LOOP"), Some(6));
        assert_eq!(loop_score("transport loop"), Some(6));
        assert_eq!(loop_score("loop metronome"), None);
        assert_eq!(loop_score("stop"), None);
    }

    #[test]
    fn word_starts_count_most() {
        // The start of a word, then inside one or the initials, then the id.
        assert_eq!(loop_score("tog"), Some(3));
        assert_eq!(loop_score("oop"), Some(2));
        assert_eq!(loop_score("tl"), Some(2));
        assert_eq!(loop_score("ttl"), Some(2));
        assert_eq!(loop_score("_lo"), Some(1));
        assert_eq!(loop_score("toggle_loop"), Some(1));
        // A single letter isn't initials: every label has some.
        let score = score("x", "File: Save Project As...", "file.save_as", &[]);
        assert_eq!(score, None);
    }

    #[test]
    fn chords_find_their_commands() {
        assert_eq!(loop_score("ctrl+l"), Some(10));
        assert_eq!(loop_score("Ctrl + L"), Some(10));
        assert_eq!(loop_score("control+L"), Some(10));
        assert_eq!(loop_score("ctrl"), None);
        assert_eq!(loop_score("ctrl+k"), None);
        // Part of a chord, as it's typed.
        assert_eq!(loop_score("ctrl+"), Some(5));
        assert_eq!(loop_score("ctrl+shift"), None);
        let tempo = chords(&["Ctrl+Shift+Up"]);
        let up = |query| score(query, "Transport: Tempo Up", "transport.tempo_up", &tempo);
        assert_eq!(up("ctrl+shift"), Some(5));
        assert_eq!(up("ctrl+shift+up"), Some(10));
        // A whole chord is only itself, though it starts others.
        assert_eq!(up("ctrl+s"), None);
        // A chord that's also a word of the title counts both ways.
        assert_eq!(up("up"), Some(3));
        let metronome = chords(&["O"]);
        let o = score(
            "o",
            "Transport: Toggle Metronome",
            "transport.toggle_metronome",
            &metronome,
        );
        assert_eq!(o, Some(12));
    }
}
