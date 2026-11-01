//! Onset lists: `[{"frame": u64, "strength": f32, "percussiveness": f32}, ...]`.

use serde::Deserialize;

fn one() -> f32 {
    1.0
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Onset {
    /// Sample index in the input where the attack starts.
    pub frame: u64,
    #[serde(default = "one")]
    pub strength: f32,
    #[serde(default)]
    pub percussiveness: f32,
}

impl Onset {
    pub fn at(frame: u64) -> Self {
        Onset { frame, strength: 1.0, percussiveness: 1.0 }
    }
}

pub fn load(path: &std::path::Path) -> Result<Vec<Onset>, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&s).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parse a JSON onset list (sorted and de-duplicated, see [`normalize`]).
pub fn parse(json: &str) -> Result<Vec<Onset>, String> {
    let v: Vec<Onset> = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(normalize(v))
}

/// Sort by frame, merge duplicates (keep the strongest).
pub fn normalize(mut v: Vec<Onset>) -> Vec<Onset> {
    v.sort_by_key(|o| o.frame);
    let mut out: Vec<Onset> = Vec::with_capacity(v.len());
    for o in v {
        if let Some(last) = out.last_mut()
            && last.frame == o.frame {
                if o.strength > last.strength {
                    *last = o;
                }
                continue;
            }
        out.push(o);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_sorts_and_dedupes() {
        let v = parse(r#"[{"frame": 900, "strength": 0.2, "percussiveness": 0.9},
                          {"frame": 100},
                          {"frame": 900, "strength": 0.7, "percussiveness": 0.1}]"#)
        .unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0], Onset { frame: 100, strength: 1.0, percussiveness: 0.0 });
        assert_eq!(v[1].frame, 900);
        assert_eq!(v[1].strength, 0.7);
        assert!(parse("[{\"strength\": 1.0}]").is_err());
    }
}
