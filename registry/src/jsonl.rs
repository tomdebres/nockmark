//! Append-only JSONL, as the AI and ZK v5 run stores use it: one JSON event
//! per line, replayed at boot.
//!
//! Written so that a crash or a full disk in the middle of an append costs
//! at most the record being written. Two things make that true. Each record
//! goes out in one `write_all` call, where `writeln!` on a `File` issues a
//! write per formatted fragment. And an append that finds the file ending in
//! a fragment (no final newline) terminates it first. Without that, the next
//! record would land on the fragment's line, fail to parse with it, and be
//! skipped at the next replay: a lost `run` line would drop a verified row and
//! reopen its challenge (adversarial review, M7 step 3).

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Append `v` as one line, newline-terminating a torn last line first.
pub fn append_line(path: &Path, v: &serde_json::Value) -> std::io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    let len = f.metadata()?.len();
    if len > 0 {
        let mut last = [0u8; 1];
        f.seek(SeekFrom::Start(len - 1))?;
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.insert(0, '\n');
        }
    }
    // O_APPEND: the write goes to the end whatever the read moved the cursor to.
    f.write_all(line.as_bytes())
}

/// Every line of `raw` that parses as JSON, and how many did not. Callers
/// skip the bad ones (a torn write) and log the count, so a skipped record is
/// visible in the boot log rather than silently gone.
pub fn parse_lines(raw: &str) -> (Vec<serde_json::Value>, usize) {
    let mut bad = 0;
    let values = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str(l) {
            Ok(v) => Some(v),
            Err(_) => {
                bad += 1;
                None
            }
        })
        .collect();
    (values, bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fragment left by a crash mid-append does not take the next record
    /// down with it.
    #[test]
    fn a_torn_last_line_does_not_swallow_the_next_record() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("store.jsonl");
        append_line(&p, &serde_json::json!({"ev": "a"})).unwrap();
        // What a crash mid-append leaves: half a record, no newline.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"{\"ev\": \"run\", \"id\"")
            .unwrap();
        append_line(&p, &serde_json::json!({"ev": "b"})).unwrap();
        let (values, bad) = parse_lines(&std::fs::read_to_string(&p).unwrap());
        assert_eq!(values, vec![serde_json::json!({"ev": "a"}), serde_json::json!({"ev": "b"})]);
        assert_eq!(bad, 1, "the fragment alone is lost");
    }
}
