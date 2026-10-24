//! The library at the size of a large sample collection, printed as a
//! Markdown table: reading and writing the index file, preparing it for
//! search, searches of one and more words, finding similar files, and
//! scanning a folder of real files, the first time and again, and reading
//! their headers.
//!
//! ```sh
//! cargo bench -p musicable-library --bench large
//! cargo bench -p musicable-library --bench large -- 1000000   # files indexed
//! ```
//!
//! The index is made up, a quarter of a million files by default, named as
//! sample packs name them. The scan is of 20,000 small WAV files the bench
//! writes to the temp folder, and the system has them cached, so it measures
//! Musicable's work, not the disk's.

// A report: it prints.
#![allow(clippy::disallowed_macros)]

use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use musicable_io::audio::{SampleFormat, save_wav};
use musicable_library::scan::header;
use musicable_library::{AudioFacts, FileId, Format, Index, Query, Record, Searcher, scan};

/// Files in the made-up index, unless the command line says.
const FILES: usize = 250_000;
/// Real files the scan goes through.
const SCANNED: usize = 20_000;

const PACKS: &[&str] = &[
    "Analog Drums",
    "Deep House",
    "Lo-Fi Keys",
    "Modular Textures",
    "Trap Essentials",
    "Vinyl Breaks",
    "Cinematic FX",
    "Techno Tools",
];
const FOLDERS: &[&str] = &[
    "Drums/Kicks",
    "Drums/Snares",
    "Drums/Hats",
    "Drums/Percussion",
    "Loops/Drums",
    "Loops/Bass",
    "Loops/Melodic",
    "One Shots/Synth",
    "One Shots/FX",
    "Vocals",
];
const WORDS: &[&str] = &[
    "Kick", "Snare", "Hat", "Clap", "Rim", "Tom", "Shaker", "Bass", "Sub", "Pad", "Lead", "Pluck",
    "Chord", "Stab", "Vox", "Riser", "Impact", "Noise", "Hard", "Soft", "Dark", "Bright", "Dusty",
    "Warm", "Short", "Long", "Open", "Closed", "Acoustic", "Tape", "Crunch", "Room",
];

/// A little generator of made-up names, the same every run.
struct Random(u64);

impl Random {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as usize
    }

    fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.below(from.len())]
    }
}

fn made_up(files: usize) -> Index {
    let mut random = Random(1);
    let mut index = Index::new();
    let places = 4;
    for p in 0..places {
        let folder = PathBuf::from(format!("/samples/Library {p}"));
        index.add_place(folder.clone());
        let records = (0..files / places)
            .map(|i| {
                let pack = random.pick(PACKS);
                let folder = random.pick(FOLDERS);
                let words = 1 + random.below(3);
                let mut name = String::new();
                for _ in 0..words {
                    name.push_str(random.pick(WORDS));
                    name.push(if random.below(4) == 0 { '_' } else { ' ' });
                }
                let tempo = 80 + 2 * random.below(40);
                let seconds = 1 + random.below(30);
                let path = format!("{pack} {}/{folder}/{name}{tempo} {i:05}.wav", i % 40);
                let audio = AudioFacts {
                    format: Format::Wav,
                    sample_rate: 44_100,
                    channels: 2,
                    frames: 44_100 * seconds as u64,
                };
                Record::new(path, 1000 + i as u64, 1_700_000_000_000, Some(audio))
            })
            .collect();
        index.set_files(&folder, records);
    }
    index
}

/// The median of `runs` runs of `f`, after one to warm up.
fn time<T>(runs: usize, mut f: impl FnMut() -> T) -> Duration {
    black_box(f());
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            black_box(f());
            start.elapsed()
        })
        .collect();
    times.sort();
    times[times.len() / 2]
}

fn ms(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

fn main() {
    let files = std::env::args()
        .skip(1)
        .find_map(|arg| arg.parse().ok())
        .unwrap_or(FILES);
    let index = made_up(files);
    let folder = std::env::temp_dir().join(format!("musicable-large-{}", std::process::id()));
    fs::create_dir_all(&folder).unwrap();
    let file = folder.join("library.json");

    println!("{} files indexed\n", index.len());
    println!("| What | Time |");
    println!("|---|---|");
    let write = time(3, || index.write(&file).unwrap());
    let size = fs::metadata(&file).unwrap().len();
    println!(
        "| Writing the index file ({:.1} MB) | {} |",
        size as f64 / 1e6,
        ms(write)
    );
    println!(
        "| Reading the index file | {} |",
        ms(time(3, || Index::read(&file).unwrap()))
    );
    println!(
        "| Preparing the index for search | {} |",
        ms(time(5, || {
            let mut searcher = Searcher::new();
            searcher.update(&index);
            searcher
        }))
    );
    println!(
        "| Preparing one place, of {} files | {} |",
        index.places()[0].files().len(),
        ms(time(5, || Searcher::prepare(index.places()[0].files())))
    );
    let mut searcher = Searcher::new();
    searcher.update(&index);
    for text in [
        "kick",
        "dark pad",
        "deep house kick 120",
        "ha",
        "nothing like it",
    ] {
        let query = Query::new(text);
        let total = searcher.search(&index, &query, 200).total;
        println!(
            "| Searching for \"{text}\" ({total} found) | {} |",
            ms(time(11, || searcher.search(&index, &query, 200)))
        );
    }
    let like = FileId { place: 0, file: 0 };
    println!(
        "| Finding files like one | {} |",
        ms(time(5, || searcher.similar(&index, like, 50)))
    );

    let place = folder.join("Scanned");
    let samples = vec![0.25f32; 64];
    for i in 0..SCANNED {
        let path = place
            .join(format!("Pack {}", i % 20))
            .join(FOLDERS[i % FOLDERS.len()])
            .join(format!("Sample {i:05}.wav"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        save_wav(&path, 44_100, SampleFormat::Int16, &[&samples]).unwrap();
    }
    let mut records = Vec::new();
    let first = time(1, || {
        records = scan(&place, &[], &|| true, &mut |_| {}).unwrap().unwrap();
    });
    println!(
        "| Scanning {SCANNED} files the first time | {} |",
        ms(first)
    );
    let read = time(1, || {
        let headers = records
            .iter()
            .map(|record| header(&place.join(&record.path)));
        headers.collect::<Vec<_>>()
    });
    println!("| Reading their headers, on one thread | {} |", ms(read));
    println!(
        "| Scanning them again, unchanged | {} |",
        ms(time(3, || scan(&place, &records, &|| true, &mut |_| {})))
    );
    let _ = fs::remove_dir_all(&folder);
}
