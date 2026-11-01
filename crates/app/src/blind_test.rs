//! Blind tests: rounds of pairs of sounds, judged without knowing which is
//! which, for checks such as warping against another app, or an analog
//! clone against the hardware. `debug.make_blind_test` makes a round, and
//! the Blind Test window (`blind_test_window.rs`) plays it to be judged.
//!
//! A round is a folder. Each pair, a file from the first list and its
//! partner from the second, becomes a trial: the two copied as `NN A` and
//! `NN B` in an order chosen at random, and, for an ABX round, one of them
//! copied again as `NN X`. `round.json` lists the trials by those names
//! alone. `key.json` says where each copy came from and which X is: the
//! key, which nothing shows until every trial is judged. Each answer is
//! kept in `answers.json` as it's given, so a round can be judged over
//! several sittings. Unsealing reads the key and writes `results.json`:
//! each trial unblinded, how many X's were named right, how often each list
//! was preferred, and the chance of doing as well by guessing.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::log_file;

pub const ROUND_FILE: &str = "round.json";
pub const KEY_FILE: &str = "key.json";
pub const ANSWERS_FILE: &str = "answers.json";
pub const RESULTS_FILE: &str = "results.json";
/// The most trials in a round.
pub const MOST_TRIALS: usize = 999;

/// What to make a round of: see `debug.make_blind_test`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Making {
    pub folder: PathBuf,
    pub first: Vec<PathBuf>,
    pub second: Vec<PathBuf>,
    pub first_name: String,
    pub second_name: String,
    pub name: String,
    pub abx: bool,
    pub seed: Option<u64>,
}

/// One of a trial's sounds, as the window names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    A,
    B,
}

/// Which of a trial's sounds is better, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Better {
    A,
    B,
    Same,
}

/// `round.json`: the trials, by their copies' names alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoundFile {
    pub name: String,
    pub abx: bool,
    pub made: String,
    pub trials: Vec<TrialFiles>,
}

/// A trial's copies, in the round's folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrialFiles {
    pub number: usize,
    pub a: String,
    pub b: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
}

/// `key.json`: where each copy came from, and which X is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct KeyFile {
    first_name: String,
    second_name: String,
    seed: u64,
    trials: Vec<KeyTrial>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct KeyTrial {
    number: usize,
    /// The pair's place in the lists it was made from, from 1.
    pair: usize,
    a: PathBuf,
    b: PathBuf,
    /// Whether A came from the first list.
    a_is_first: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    x: Option<Side>,
}

/// An answer to a trial: which X is, which is better, and why.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<Side>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub better: Option<Better>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// `answers.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct AnswersFile {
    answers: Vec<Answer>,
}

/// Makes a round as `making` says, and says what it made.
pub fn make(making: &Making) -> Result<Json, String> {
    let Making {
        folder,
        first,
        second,
        ..
    } = making;
    if first.len() != second.len() {
        return Err(format!(
            "first has {} files and second {}: each file in first is paired with the one at \
             the same place in second.",
            first.len(),
            second.len()
        ));
    }
    if first.is_empty() || first.len() > MOST_TRIALS {
        return Err(format!("A round has from 1 to {MOST_TRIALS} pairs."));
    }
    for file in first.iter().chain(second) {
        if !file.is_file() {
            return Err(format!("There's no file {}.", file.display()));
        }
    }
    let empty = match fs::read_dir(folder) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => !folder.exists(),
    };
    if !empty {
        return Err(format!(
            "{} isn't empty: a round is made in a new or empty folder.",
            folder.display()
        ));
    }
    fs::create_dir_all(folder)
        .map_err(|error| format!("Couldn't make {}: {error}.", folder.display()))?;

    let seed = making.seed.unwrap_or_else(|| {
        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
        now.map_or(0, |since| since.as_nanos() as u64)
    });
    let mut random = SplitMix(seed);
    let mut order: Vec<usize> = (0..first.len()).collect();
    for i in (1..order.len()).rev() {
        order.swap(i, random.below(i + 1));
    }
    let mut trials = Vec::new();
    let mut key = Vec::new();
    for (index, &pair) in order.iter().enumerate() {
        let number = index + 1;
        let a_is_first = random.below(2) == 0;
        let (a, b) = match a_is_first {
            true => (&first[pair], &second[pair]),
            false => (&second[pair], &first[pair]),
        };
        let x = making.abx.then(|| match random.below(2) {
            0 => Side::A,
            _ => Side::B,
        });
        let copy = |from: &Path, label: &str| -> Result<String, String> {
            let extension = from.extension().and_then(|e| e.to_str()).unwrap_or("wav");
            let name = format!("{number:02} {label}.{}", extension.to_lowercase());
            fs::copy(from, folder.join(&name))
                .map_err(|error| format!("Couldn't copy {}: {error}.", from.display()))?;
            Ok(name)
        };
        let x_file = match x {
            Some(Side::A) => Some(copy(a, "X")?),
            Some(Side::B) => Some(copy(b, "X")?),
            None => None,
        };
        trials.push(TrialFiles {
            number,
            a: copy(a, "A")?,
            b: copy(b, "B")?,
            x: x_file,
        });
        key.push(KeyTrial {
            number,
            pair: pair + 1,
            a: a.clone(),
            b: b.clone(),
            a_is_first,
            x,
        });
    }
    let round = RoundFile {
        name: making.name.clone(),
        abx: making.abx,
        made: log_file::utc(SystemTime::now()),
        trials,
    };
    let key = KeyFile {
        first_name: making.first_name.clone(),
        second_name: making.second_name.clone(),
        seed,
        trials: key,
    };
    write(&folder.join(ROUND_FILE), &round)?;
    write(&folder.join(KEY_FILE), &key)?;
    Ok(json!({
        "folder": folder,
        "name": round.name,
        "trials": round.trials.len(),
        "abx": round.abx,
    }))
}

/// A round, as the window judges it.
#[derive(Debug, Clone)]
pub struct Round {
    pub folder: PathBuf,
    pub file: RoundFile,
    pub answers: Vec<Answer>,
    /// What unsealing found, once it's unsealed.
    pub results: Option<Json>,
}

impl Round {
    /// Opens the round in `folder`, with the answers given so far, and its
    /// results if it's unsealed.
    pub fn open(folder: &Path) -> Result<Self, String> {
        let file: RoundFile = read(&folder.join(ROUND_FILE))
            .map_err(|error| format!("{} doesn't hold a blind test: {error}", folder.display()))?;
        if file.trials.is_empty() {
            return Err(format!(
                "The blind test in {} has no trials.",
                folder.display()
            ));
        }
        let mut answers = match folder.join(ANSWERS_FILE).exists() {
            true => read::<AnswersFile>(&folder.join(ANSWERS_FILE))?.answers,
            false => Vec::new(),
        };
        answers.resize(file.trials.len(), Answer::default());
        let results = match folder.join(RESULTS_FILE).exists() {
            true => Some(read::<Json>(&folder.join(RESULTS_FILE))?),
            false => None,
        };
        Ok(Self {
            folder: folder.to_owned(),
            file,
            answers,
            results,
        })
    }

    /// The file of trial `index`'s sound `which`: A, B, or X for `None`.
    pub fn sound(&self, index: usize, which: Option<Side>) -> Option<PathBuf> {
        let trial = self.file.trials.get(index)?;
        let name = match which {
            Some(Side::A) => &trial.a,
            Some(Side::B) => &trial.b,
            None => trial.x.as_ref()?,
        };
        Some(self.folder.join(name))
    }

    /// Whether trial `index` has every answer it needs: which X is, in an
    /// ABX round, and which is better.
    pub fn judged(&self, index: usize) -> bool {
        let answer = &self.answers[index];
        answer.better.is_some() && (!self.file.abx || answer.x.is_some())
    }

    /// How many trials are judged.
    pub fn judged_count(&self) -> usize {
        (0..self.answers.len()).filter(|&i| self.judged(i)).count()
    }

    /// Keeps trial `index`'s answer, in `answers.json` too. Once it's
    /// unsealed, the answers stay as they were.
    pub fn answer(&mut self, index: usize, answer: Answer) -> Result<(), String> {
        if self.results.is_some() || self.answers.get(index) == Some(&answer) {
            return Ok(());
        }
        self.answers[index] = answer;
        write(
            &self.folder.join(ANSWERS_FILE),
            &AnswersFile {
                answers: self.answers.clone(),
            },
        )
    }

    /// Reads the key, now that every trial is judged, and writes the
    /// results.
    pub fn unseal(&mut self) -> Result<&Json, String> {
        let results = match self.results.take() {
            Some(results) => results,
            None => {
                let left = self.answers.len() - self.judged_count();
                if left > 0 {
                    return Err(format!("{left} trials are still to be judged."));
                }
                let key: KeyFile = read(&self.folder.join(KEY_FILE))?;
                let results = results(&self.file, &key, &self.answers)?;
                write(&self.folder.join(RESULTS_FILE), &results)?;
                results
            }
        };
        Ok(self.results.insert(results))
    }
}

/// The round unblinded.
fn results(round: &RoundFile, key: &KeyFile, answers: &[Answer]) -> Result<Json, String> {
    if key.trials.len() != round.trials.len() {
        return Err("The key doesn't fit the round: it has other trials.".into());
    }
    let (mut right, mut first, mut second, mut same) = (0, 0, 0, 0);
    let names = [key.first_name.as_str(), key.second_name.as_str()];
    let mut trials = Vec::new();
    for ((trial, sealed), answer) in key.trials.iter().zip(&round.trials).zip(answers) {
        let is = |side: Side| {
            let first = (side == Side::A) == trial.a_is_first;
            names[usize::from(!first)]
        };
        let correct = trial.x.zip(answer.x).map(|(x, named)| x == named);
        right += usize::from(correct == Some(true));
        let preferred = match answer.better {
            Some(Better::A) => is(Side::A),
            Some(Better::B) => is(Side::B),
            _ => "same",
        };
        match answer.better {
            Some(Better::Same) | None => same += 1,
            Some(better) => {
                let side = match better {
                    Better::A => Side::A,
                    _ => Side::B,
                };
                match (side == Side::A) == trial.a_is_first {
                    true => first += 1,
                    false => second += 1,
                }
            }
        }
        trials.push(json!({
            "number": trial.number,
            "pair": trial.pair,
            "a": { "file": sealed.a, "from": trial.a, "is": is(Side::A) },
            "b": { "file": sealed.b, "from": trial.b, "is": is(Side::B) },
            "x": trial.x,
            "answered_x": answer.x,
            "correct": correct,
            "better": answer.better,
            "preferred": preferred,
            "note": answer.note,
        }));
    }
    let count = round.trials.len();
    let abx = round.abx.then(|| {
        json!({
            "trials": count,
            "correct": right,
            "p_value": round_to(at_least(right, count), 4),
        })
    });
    Ok(json!({
        "name": round.name,
        "made": round.made,
        "unsealed": log_file::utc(SystemTime::now()),
        "first": key.first_name,
        "second": key.second_name,
        "seed": key.seed,
        "abx": abx,
        "preference": {
            "first": first,
            "second": second,
            "same": same,
            "p_value": round_to(sign_test(first, second), 4),
        },
        "trials": trials,
    }))
}

/// The chance of `k` or more of `n` right by guessing, one in two each.
fn at_least(k: usize, n: usize) -> f64 {
    (k..=n).map(|i| choose(n, i)).sum::<f64>() / 2f64.powi(n as i32)
}

/// The two-sided sign test's p for `a` against `b`.
fn sign_test(a: usize, b: usize) -> f64 {
    let n = a + b;
    if n == 0 {
        return 1.0;
    }
    (2.0 * at_least(a.max(b), n)).min(1.0)
}

fn choose(n: usize, k: usize) -> f64 {
    (0..k).fold(1.0, |c, i| c * (n - i) as f64 / (i + 1) as f64)
}

fn round_to(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    (value * scale).round() / scale
}

/// SplitMix64, for shuffles a seed repeats.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number from 0 to `n` - 1.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("Couldn't read {}: {error}.", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("Couldn't read {}: {error}.", path.display()))
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).expect("it serialises");
    fs::write(path, text + "\n")
        .map_err(|error| format!("Couldn't write {}: {error}.", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("musicable-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_round_hides_its_key_until_every_trial_is_judged() {
        let dir = TempDir::new("blind-test");
        let mut first = Vec::new();
        let mut second = Vec::new();
        for i in 0..6 {
            for (list, name) in [(&mut first, "live"), (&mut second, "ours")] {
                let file = dir.0.join(format!("{name} {i}.wav"));
                fs::write(&file, format!("{name} {i}")).unwrap();
                list.push(file);
            }
        }
        let making = Making {
            folder: dir.0.join("Round"),
            first,
            second,
            first_name: "Live".into(),
            second_name: "Musicable".into(),
            name: "Warping".into(),
            abx: true,
            seed: Some(7),
        };
        let made = make(&making).unwrap();
        assert_eq!(made["trials"], 6);
        let again = make(&making);
        assert!(again.is_err_and(|why| why.contains("isn't empty")));

        // The copies give nothing away: an X is a copy of A or of B.
        let mut round = Round::open(&making.folder).unwrap();
        assert_eq!(round.file.trials[0].a, "01 A.wav");
        let contents = |path: PathBuf| fs::read_to_string(path).unwrap();
        for index in 0..6 {
            let x = contents(round.sound(index, None).unwrap());
            let a = contents(round.sound(index, Some(Side::A)).unwrap());
            let b = contents(round.sound(index, Some(Side::B)).unwrap());
            assert!(x == a || x == b);
            assert_ne!(a, b);
        }
        let error = round.unseal().unwrap_err();
        assert!(error.contains("6 trials"), "{error}");

        // Answers are kept as they're given, and found again.
        for index in 0..6 {
            let x = contents(round.sound(index, None).unwrap());
            let a = contents(round.sound(index, Some(Side::A)).unwrap());
            let named = if x == a { Side::A } else { Side::B };
            // Prefer Musicable's, and name X right but in the last trial.
            let ours = match a.starts_with("ours") {
                true => Better::A,
                false => Better::B,
            };
            let x = match index {
                5 => Some(if named == Side::A { Side::B } else { Side::A }),
                _ => Some(named),
            };
            let answer = Answer {
                x,
                better: Some(ours),
                note: format!("trial {index}"),
            };
            round.answer(index, answer).unwrap();
        }
        let mut round = Round::open(&making.folder).unwrap();
        assert_eq!(round.judged_count(), 6);
        let results = round.unseal().unwrap().clone();
        assert_eq!(results["abx"]["correct"], 5, "{results:#}");
        assert_eq!(results["abx"]["p_value"], 0.1094, "{results:#}");
        assert_eq!(results["preference"]["second"], 6, "{results:#}");
        assert_eq!(results["preference"]["first"], 0, "{results:#}");
        assert_eq!(results["preference"]["p_value"], 0.0313, "{results:#}");
        let trial = &results["trials"][0];
        assert_eq!(trial["preferred"], "Musicable", "{trial}");
        assert_eq!(trial["note"], "trial 0", "{trial}");
        // It's written, and opens unsealed.
        let reopened = Round::open(&making.folder).unwrap();
        assert_eq!(reopened.results.as_ref(), Some(&results));
    }

    #[test]
    fn a_round_needs_pairs_of_files_that_are_there() {
        let dir = TempDir::new("blind-test-refused");
        let file = dir.0.join("one.wav");
        fs::write(&file, "x").unwrap();
        let making = |first: Vec<PathBuf>, second: Vec<PathBuf>| Making {
            folder: dir.0.join("Round"),
            first,
            second,
            first_name: "1".into(),
            second_name: "2".into(),
            name: String::new(),
            abx: false,
            seed: None,
        };
        let uneven = make(&making(vec![file.clone()], Vec::new()));
        assert!(uneven.is_err_and(|why| why.contains("same place")));
        let missing = make(&making(vec![file.clone()], vec![dir.0.join("gone.wav")]));
        assert!(missing.is_err_and(|why| why.contains("no file")));
        let made = make(&making(vec![file.clone()], vec![file])).unwrap();
        assert_eq!(made["abx"], false);
        let round = Round::open(&dir.0.join("Round")).unwrap();
        assert_eq!(round.sound(0, None), None);
    }

    #[test]
    fn guessing_odds_are_binomial() {
        assert_eq!(at_least(0, 4), 1.0);
        assert_eq!(at_least(4, 4), 1.0 / 16.0);
        assert_eq!(sign_test(0, 0), 1.0);
        assert_eq!(sign_test(3, 3), 1.0);
        assert_eq!(sign_test(5, 0), 2.0 / 32.0);
    }
}
