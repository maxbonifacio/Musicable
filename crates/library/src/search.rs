//! Finding files by what the user types, and finding files like one.
//!
//! A search goes through every file of every place, so it needs no index of
//! words to keep up to date. A [`Searcher`] keeps each place's file names
//! and folders in lower case, one after the other in one string, with where
//! the names' words start, and looks for each word of the query in the whole
//! string at once. Every word has to be in the file's name, its folders or
//! its tags. A word counts most in the name, least in a folder, and more
//! where a word of the name starts: "kick" matches "Kick_01" better than
//! "Sidekick", and "808" starts a word of "Kick808" too.

use std::cmp::Reverse;

use memchr::memmem::Finder;

use crate::index::{FileId, Index, Place, Record};

/// What the user typed: its words, in lower case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    words: Vec<String>,
}

impl Query {
    pub fn new(text: &str) -> Self {
        Self {
            words: text.split_whitespace().map(str::to_lowercase).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn words(&self) -> &[String] {
        &self.words
    }

    /// How well `name` and `folder`, such as a preset's name and its
    /// device's, match: as a file's name and folder do in a search. `None`
    /// if a word is in neither, or the query is empty.
    pub fn score(&self, name: &str, folder: &str) -> Option<u32> {
        if self.is_empty() {
            return None;
        }
        let mut text = String::from("\n");
        let starts = lower(name, &mut text);
        text.push('\n');
        let folder_at = text.len();
        lower(folder, &mut text);
        let key = Key {
            name: 1,
            folder: folder_at,
            end: text.len(),
            starts,
        };
        let mut score = 0;
        for word in &self.words {
            score += Finder::new(word.as_bytes())
                .find_iter(text.as_bytes())
                .map(|at| key.class(&text, at))
                .max()?;
        }
        Some(score)
    }
}

/// A file a search found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub file: FileId,
    /// How well it matched: higher is better.
    pub score: u32,
}

/// What a search found: the best hits, best first, and how many files
/// matched in all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub hits: Vec<Hit>,
    pub total: usize,
}

/// A word in the name where one of the name's words starts, inside a word
/// of the name, in a folder's name where one of its words starts, inside
/// one, and in a tag.
const NAME_START: u32 = 6;
const NAME: u32 = 4;
const FOLDER_START: u32 = 3;
const FOLDER: u32 = 2;
const TAG: u32 = 3;

/// The places' names and folders, prepared for searching.
#[derive(Debug, Default)]
pub struct Searcher {
    places: Vec<Prepared>,
}

/// One place's files, prepared for searching: see [`Searcher::prepare`].
#[derive(Debug, Default)]
pub struct Prepared {
    /// The [`Place::generation`] they were prepared from, or 0 before
    /// they're put in a searcher.
    generation: u64,
    /// Each file's name, without its extension, then its folder, in lower
    /// case, each after a line break, which no word of a query has.
    text: String,
    /// Where each file's are in `text`, by the index of the file.
    keys: Vec<Key>,
}

#[derive(Debug, Clone, Copy)]
struct Key {
    name: usize,
    folder: usize,
    end: usize,
    /// Where the name's words start, as a bit for each of its first 64
    /// bytes in lower case.
    starts: u64,
}

impl Key {
    /// How a word found at `at` in `text`, the file's, counts.
    fn class(&self, text: &str, at: usize) -> u32 {
        let after_break = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        if at < self.folder {
            let offset = at - self.name;
            let start = after_break || (offset < 64 && self.starts & (1 << offset) != 0);
            if start { NAME_START } else { NAME }
        } else if after_break {
            FOLDER_START
        } else {
            FOLDER
        }
    }

    /// The name's length, in bytes in lower case.
    fn name_length(&self) -> usize {
        self.folder - 1 - self.name
    }
}

/// Marks a file a word of the query isn't in.
const OUT: u32 = u32::MAX;

impl Searcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Prepares `files`, a place's, for searching, which takes a while for
    /// a large place: a thread of the background queue can do it, and then
    /// [`put`](Self::put) it in the searcher.
    pub fn prepare(files: &[Record]) -> Prepared {
        let mut text = String::with_capacity(files.iter().map(|r| r.path.len() + 2).sum());
        let keys = files
            .iter()
            .map(|record| {
                text.push('\n');
                let name = text.len();
                let starts = lower(record.stem(), &mut text);
                text.push('\n');
                let folder = text.len();
                lower(record.folder(), &mut text);
                Key {
                    name,
                    folder,
                    end: text.len(),
                    starts,
                }
            })
            .collect();
        Prepared {
            generation: 0,
            text,
            keys,
        }
    }

    /// Puts `prepared`, made from the files the place at `place` in `index`
    /// has now, in the searcher, so [`update`](Self::update) needn't prepare
    /// them again.
    pub fn put(&mut self, index: &Index, place: usize, mut prepared: Prepared) {
        let Some(generation) = index.places().get(place).map(Place::generation) else {
            return;
        };
        if self.places.len() <= place {
            self.places.resize_with(place + 1, Prepared::default);
        }
        prepared.generation = generation;
        self.places[place] = prepared;
    }

    /// Prepares the places of `index` that changed since they were last
    /// prepared. Places that only moved, as when one before them is taken
    /// away, keep what was prepared.
    pub fn update(&mut self, index: &Index) {
        let mut old = std::mem::take(&mut self.places);
        self.places = index
            .places()
            .iter()
            .map(|place| {
                // Generations count from one, so a new one is never current.
                match old.iter().position(|p| p.generation == place.generation()) {
                    Some(i) => old.swap_remove(i),
                    None => {
                        let mut prepared = Self::prepare(place.files());
                        prepared.generation = place.generation();
                        prepared
                    }
                }
            })
            .collect();
    }

    /// The files of `index` that match `query`, at most `limit` of them,
    /// best first: those that match best, then those with shorter names,
    /// then in order of place and path. Call [`update`](Self::update) with
    /// the index first. An empty query matches nothing.
    pub fn search(&self, index: &Index, query: &Query, limit: usize) -> Found {
        if query.is_empty() {
            return Found::default();
        }
        let finders: Vec<Finder<'_>> = query
            .words()
            .iter()
            .map(|word| Finder::new(word.as_bytes()))
            .collect();
        let mut hits = Vec::new();
        let (mut scores, mut best) = (Vec::new(), Vec::new());
        for (p, (prepared, place)) in self.places.iter().zip(index.places()).enumerate() {
            if prepared.keys.len() != place.files().len() {
                continue;
            }
            prepared.score(place, query, &finders, &mut scores, &mut best);
            for (f, (&score, key)) in scores.iter().zip(&prepared.keys).enumerate() {
                if score != OUT {
                    let file = FileId { place: p, file: f };
                    hits.push((score, key.name_length(), file));
                }
            }
        }
        let total = hits.len();
        let order = |&(score, length, file): &(u32, usize, FileId)| (Reverse(score), length, file);
        if hits.len() > limit && limit > 0 {
            hits.select_nth_unstable_by_key(limit - 1, order);
        }
        hits.truncate(limit);
        hits.sort_unstable_by_key(order);
        Found {
            hits: hits
                .into_iter()
                .map(|(score, _, file)| Hit { file, score })
                .collect(),
            total,
        }
    }

    /// The files most like the file `like`, at most `limit` of them, most
    /// alike first: those whose names share the most words with its name,
    /// then those in the same folder, then those whose length is nearest
    /// its. Until analysis gives files an embedding, names and lengths are
    /// all there is to go on. Numbers shorter than three digits aren't words
    /// here: "Kick 01" and "Kick 02" share "kick", but not "01", while "808"
    /// is a word. Files that aren't audio Musicable reads are left out, but
    /// not those whose headers are still to be read, which come after those
    /// whose lengths are known. Call [`update`](Self::update) with the index
    /// first.
    pub fn similar(&self, index: &Index, like: FileId, limit: usize) -> Vec<Hit> {
        let Some(record) = index.get(like) else {
            return Vec::new();
        };
        let words = name_words(record);
        let seconds = record.audio.map(|audio| audio.seconds());
        let mut found: Vec<(u32, u64, FileId)> = Vec::new();
        for (p, (prepared, place)) in self.places.iter().zip(index.places()).enumerate() {
            if prepared.keys.len() != place.files().len() {
                continue;
            }
            for (f, (key, other)) in prepared.keys.iter().zip(place.files()).enumerate() {
                let file = FileId { place: p, file: f };
                if file == like || (other.audio.is_none() && !other.unread) {
                    continue;
                }
                // A bit for each of the words it shares.
                let mut shared = 0u64;
                prepared.words(key, |word| {
                    if let Some(i) = words.iter().position(|w| w == word) {
                        shared |= 1 << (i % 64);
                    }
                });
                let shared = shared.count_ones();
                let folder = p == like.place && other.folder() == record.folder();
                if shared == 0 && !folder {
                    continue;
                }
                let score = 4 * shared + u32::from(folder);
                // How far apart the lengths are, as a ratio, in thousandths.
                let apart = match (seconds, other.audio.map(|audio| audio.seconds())) {
                    (Some(a), Some(b)) if a > 0.0 && b > 0.0 => {
                        ((a / b).ln().abs() * 1000.0) as u64
                    }
                    _ => u64::MAX,
                };
                found.push((score, apart, file));
            }
        }
        let order = |&(score, apart, file): &(u32, u64, FileId)| (Reverse(score), apart, file);
        if found.len() > limit && limit > 0 {
            found.select_nth_unstable_by_key(limit - 1, order);
        }
        found.truncate(limit);
        found.sort_unstable_by_key(order);
        found
            .into_iter()
            .map(|(score, _, file)| Hit { file, score })
            .collect()
    }
}

impl Prepared {
    /// Scores every file of `place`, these files, for `query`, whose words
    /// `finders` find: `OUT` for those a word isn't in. `best` is room for
    /// the best match of a word in each file.
    fn score(
        &self,
        place: &Place,
        query: &Query,
        finders: &[Finder<'_>],
        scores: &mut Vec<u32>,
        best: &mut Vec<u8>,
    ) {
        let files = self.keys.len();
        scores.clear();
        scores.resize(files, 0);
        for (word, finder) in query.words().iter().zip(finders) {
            best.clear();
            best.resize(files, 0);
            // Where the words are come in order, so the file each is in is
            // found going forwards.
            let mut file = 0;
            for at in finder.find_iter(self.text.as_bytes()) {
                while self.keys[file].end <= at {
                    file += 1;
                }
                if scores[file] != OUT && best[file] < NAME_START as u8 {
                    let class = self.keys[file].class(&self.text, at) as u8;
                    best[file] = best[file].max(class);
                }
            }
            for (f, (score, &best)) in scores.iter_mut().zip(best.iter()).enumerate() {
                if *score == OUT {
                    continue;
                }
                let class = match best {
                    0 if place.has_tags() && tagged(&place.files()[f], word) => TAG,
                    0 => {
                        *score = OUT;
                        continue;
                    }
                    best => u32::from(best),
                };
                *score += class;
            }
        }
    }

    /// Tells `each` the words of the name of the file at `key`, as
    /// [`name_words`] finds them, short numbers and single letters too.
    fn words(&self, key: &Key, mut each: impl FnMut(&str)) {
        let name = &self.text[key.name..key.folder - 1];
        let mut start = None;
        for (at, c) in name.char_indices() {
            let starts = at < 64 && key.starts & (1 << at) != 0;
            if let Some(from) = start
                && (starts || !c.is_alphanumeric())
            {
                each(&name[from..at]);
                start = None;
            }
            if c.is_alphanumeric() && start.is_none() {
                start = Some(at);
            }
        }
        if let Some(from) = start {
            each(&name[from..]);
        }
    }
}

/// Whether one of `record`'s tags has `word` in it.
fn tagged(record: &Record, word: &str) -> bool {
    record.tags.iter().any(|tag| tag.id.contains(word))
}

/// Adds `text` to `out` in lower case, and returns where its words start in
/// what it added: a bit for each of the first 64 bytes. A word starts after
/// a character that isn't a letter or digit, at a capital after a small
/// letter, and where letters and digits meet: "KickHard808" has words at
/// "Kick", "Hard" and "808".
fn lower(text: &str, out: &mut String) -> u64 {
    if text.is_ascii() {
        return lower_ascii(text, out);
    }
    let base = out.len();
    let mut starts = 0;
    let mut before: Option<char> = None;
    for c in text.chars() {
        let at = out.len() - base;
        let start = c.is_alphanumeric()
            && before.is_none_or(|b| {
                !b.is_alphanumeric()
                    || (b.is_lowercase() && c.is_uppercase())
                    || (b.is_alphabetic() && c.is_numeric())
                    || (b.is_numeric() && c.is_alphabetic())
            });
        if start && at < 64 {
            starts |= 1 << at;
        }
        out.extend(c.to_lowercase());
        before = Some(c);
    }
    starts
}

/// [`lower`] for text that's all ASCII, as most file names are, which is
/// quicker.
fn lower_ascii(text: &str, out: &mut String) -> u64 {
    let bytes = text.as_bytes();
    let mut starts = 0;
    for (at, &c) in bytes.iter().enumerate().take(64) {
        let start = c.is_ascii_alphanumeric()
            && (at == 0 || {
                let b = bytes[at - 1];
                !b.is_ascii_alphanumeric()
                    || (b.is_ascii_lowercase() && c.is_ascii_uppercase())
                    || (b.is_ascii_alphabetic() && c.is_ascii_digit())
                    || (b.is_ascii_digit() && c.is_ascii_alphabetic())
            });
        if start {
            starts |= 1 << at;
        }
    }
    let base = out.len();
    out.push_str(text);
    out[base..].make_ascii_lowercase();
    starts
}

/// The words of a file's name, in lower case, without short numbers or
/// single letters, in order, each once.
fn name_words(record: &Record) -> Vec<String> {
    let mut text = String::from("\n");
    let starts = lower(record.stem(), &mut text);
    text.push('\n');
    let prepared = Prepared {
        generation: 0,
        keys: vec![Key {
            name: 1,
            folder: text.len(),
            end: text.len(),
            starts,
        }],
        text,
    };
    let mut words = Vec::new();
    prepared.words(&prepared.keys[0], |word| words.push(word.to_owned()));
    words.retain(|word| {
        let length = word.chars().count();
        length > 1 && (length > 2 || !word.chars().all(char::is_numeric))
    });
    words.sort();
    words.dedup();
    words
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::index::{AudioFacts, Format, Tag};

    fn audio(seconds: f64) -> Option<AudioFacts> {
        Some(AudioFacts {
            format: Format::Wav,
            sample_rate: 1000,
            channels: 1,
            frames: (seconds * 1000.0) as u64,
        })
    }

    fn index(places: &[(&str, &[&str])]) -> Index {
        let mut index = Index::new();
        for &(folder, files) in places {
            let folder = PathBuf::from(folder);
            index.add_place(folder.clone());
            let records = files
                .iter()
                .map(|path| Record::new(*path, 1, 1, audio(1.0)))
                .collect();
            index.set_files(&folder, records);
        }
        index
    }

    fn found(index: &Index, query: &str) -> Vec<String> {
        let mut searcher = Searcher::new();
        searcher.update(index);
        let found = searcher.search(index, &Query::new(query), 100);
        assert_eq!(found.total, found.hits.len());
        found
            .hits
            .iter()
            .map(|hit| index.get(hit.file).unwrap().path.clone())
            .collect()
    }

    #[test]
    fn every_word_has_to_be_in_the_name_the_folders_or_the_tags() {
        let mut index = index(&[(
            "/samples",
            &[
                "Drums/Kick 01.wav",
                "Drums/Snare 01.wav",
                "Bass/Sub Kick.wav",
                "Loops/Drum Loop 120.wav",
            ],
        )]);
        assert_eq!(found(&index, "snare"), ["Drums/Snare 01.wav"]);
        assert_eq!(found(&index, "DRUMS snare"), ["Drums/Snare 01.wav"]);
        assert_eq!(found(&index, "kick bass"), ["Bass/Sub Kick.wav"]);
        assert_eq!(found(&index, "kick clap"), Vec::<String>::new());
        assert_eq!(found(&index, ""), Vec::<String>::new());
        // Not the extension.
        assert_eq!(found(&index, "wav"), Vec::<String>::new());
        let loop_file = index.find(&PathBuf::from("/samples/Loops/Drum Loop 120.wav"));
        index.tag(loop_file.unwrap(), "groove", true);
        assert_eq!(found(&index, "groove"), ["Loops/Drum Loop 120.wav"]);
        let tag = Tag::yours("x");
        assert_eq!(tag.id, "x");
    }

    #[test]
    fn names_count_most_and_word_starts_more() {
        let index = index(&[(
            "/samples",
            &[
                "Kicks/Sidekick Hit.wav",
                "Kicks/Big Hit.wav",
                "Other/KickHard808.wav",
                "Other/Kick.wav",
                "Other/Kick Long Name.wav",
            ],
        )]);
        // At word starts in the name, shorter names first; then inside a
        // word of the name; then the folder.
        assert_eq!(
            found(&index, "kick"),
            [
                "Other/Kick.wav",
                "Other/KickHard808.wav",
                "Other/Kick Long Name.wav",
                "Kicks/Sidekick Hit.wav",
                "Kicks/Big Hit.wav",
            ]
        );
        // Words start at capitals and where digits meet letters.
        assert_eq!(found(&index, "hard 808"), ["Other/KickHard808.wav"]);
        let mut searcher = Searcher::new();
        searcher.update(&index);
        let hard = searcher.search(&index, &Query::new("hard"), 10).hits[0];
        let ard = searcher.search(&index, &Query::new("ard"), 10).hits[0];
        assert_eq!((hard.score, ard.score), (NAME_START, NAME));
    }

    #[test]
    fn searches_give_the_best_up_to_the_limit_and_count_the_rest() {
        let names: Vec<String> = (0..50).map(|i| format!("Hit {i:02}.wav")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut index = index(&[("/a", &names), ("/b", &["Hit.wav"])]);
        let mut searcher = Searcher::new();
        searcher.update(&index);
        let found = searcher.search(&index, &Query::new("hit"), 5);
        assert_eq!(found.total, 51);
        let paths: Vec<&str> = found
            .hits
            .iter()
            .map(|hit| index.get(hit.file).unwrap().path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "Hit.wav",
                "Hit 00.wav",
                "Hit 01.wav",
                "Hit 02.wav",
                "Hit 03.wav"
            ]
        );
        // Places that change are prepared again.
        index.set_files(
            &PathBuf::from("/b"),
            vec![Record::new("Clap.wav", 1, 1, None)],
        );
        searcher.update(&index);
        assert_eq!(searcher.search(&index, &Query::new("clap"), 5).total, 1);
        index.remove_place(&PathBuf::from("/a"));
        searcher.update(&index);
        assert_eq!(searcher.search(&index, &Query::new("hit"), 5).total, 0);
    }

    #[test]
    fn words_are_never_found_across_files_or_between_name_and_folder() {
        let index = index(&[("/s", &["Ab.wav", "Cd.wav", "Ef/Gh.wav"])]);
        assert_eq!(found(&index, "bc"), Vec::<String>::new());
        assert_eq!(found(&index, "hef"), Vec::<String>::new());
        assert_eq!(found(&index, "ef gh"), ["Ef/Gh.wav"]);
    }

    #[test]
    fn other_names_score_as_files_do() {
        let query = Query::new("surge pad");
        assert_eq!(
            query.score("Warm Pad", "Surge XT"),
            Some(NAME_START + FOLDER_START)
        );
        assert_eq!(query.score("Warm Pad", "Vital"), None);
        assert_eq!(Query::new(" ").score("Pad", ""), None);
    }

    #[test]
    fn places_prepared_elsewhere_go_in_the_searcher() {
        let mut index = index(&[("/s", &["Kick.wav"])]);
        let mut searcher = Searcher::new();
        searcher.update(&index);
        let records = vec![Record::new("Snare.wav", 1, 1, audio(1.0))];
        let prepared = Searcher::prepare(&records);
        index.set_files(&PathBuf::from("/s"), records);
        searcher.put(&index, 0, prepared);
        assert_eq!(searcher.search(&index, &Query::new("snare"), 10).total, 1);
        assert_eq!(searcher.search(&index, &Query::new("kick"), 10).total, 0);
    }

    #[test]
    fn similar_files_share_words_then_folders_then_lengths() {
        let mut index = Index::new();
        let folder = PathBuf::from("/s");
        index.add_place(folder.clone());
        let files = vec![
            Record::new("Drums/Kick 808 01.wav", 1, 1, audio(1.0)),
            Record::new("Drums/Kick 808 02.wav", 1, 1, audio(4.0)),
            Record::new("Drums/Kick Acoustic.wav", 1, 1, audio(1.1)),
            Record::new("Drums/Snare.wav", 1, 1, audio(1.0)),
            Record::new("Bass/808 Sub.wav", 1, 1, audio(1.0)),
            Record::new("Bass/Pluck.wav", 1, 1, audio(1.0)),
            Record::new("Drums/Kick Broken.wav", 1, 1, None),
            // Its header is still to be read.
            Record::found("Drums/Kick Unread.wav", 1, 1),
        ];
        index.set_files(&folder, files);
        let like = index.find(&folder.join("Drums/Kick 808 01.wav")).unwrap();
        let mut searcher = Searcher::new();
        searcher.update(&index);
        let similar: Vec<&str> = searcher
            .similar(&index, like, 10)
            .iter()
            .map(|hit| index.get(hit.file).unwrap().path.as_str())
            .collect();
        assert_eq!(
            similar,
            [
                "Drums/Kick 808 02.wav",
                "Drums/Kick Acoustic.wav",
                "Drums/Kick Unread.wav",
                "Bass/808 Sub.wav",
                "Drums/Snare.wav",
            ]
        );
        let words = name_words(&Record::new("Kick_808HardKick 01 a.wav", 1, 1, None));
        assert_eq!(words, ["808", "hard", "kick"]);
    }
}
