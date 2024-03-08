use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::{Command, Stdio};

use anyhow::anyhow;
use anyhow::Result;
use chrono::{DateTime, FixedOffset, NaiveDateTime};
use std::io::Write;

use std::path::Path;

const UNCOMMITTED_SHA: &'static str = "0000000000000000000000000000000000000000";

pub fn git_blame_incremental(
    working_directory: &Path,
    path: &Path,
    contents: &String,
) -> Result<String> {
    let mut child = Command::new("git")
        .current_dir(working_directory)
        .arg("blame")
        // TODO: turn off all the git configurations
        .arg("--incremental")
        .arg("--contents")
        .arg("-")
        .arg(path.as_os_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("Failed to start git blame process: {}", e))?;

    if let Some(ref mut stdin) = child.stdin {
        stdin
            .write_all(contents.as_bytes())
            .map_err(|e| anyhow!("Failed to write to git blame stdin: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| anyhow!("Failed to read git blame output: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git blame process failed: {}", stderr));
    }

    Ok(String::from_utf8(output.stdout)?)
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct BlameEntry {
    pub sha: String,
    pub original_line_number: u32,
    pub final_line_number: u32,
    pub line_count: u32,

    pub author: String,
    pub author_mail: String,
    pub author_time: i64,
    pub author_tz: String,

    pub committer: String,
    pub committer_mail: String,
    pub committer_time: i64,
    pub committer_tz: String,

    pub summary: String,

    pub previous: Option<String>,
    pub filename: String,
}

impl BlameEntry {
    pub fn committer_datetime(&self) -> Result<DateTime<FixedOffset>> {
        let naive_datetime = NaiveDateTime::from_timestamp_opt(self.committer_time, 0)
            .expect("failed to parse timestamp");
        let timezone_offset_in_seconds = self
            .committer_tz
            .parse::<i32>()
            .map_err(|e| anyhow!("Failed to parse timezone offset: {}", e))?
            / 100
            * 36;
        let timezone = FixedOffset::east_opt(timezone_offset_in_seconds)
            .ok_or_else(|| anyhow!("Invalid timezone offset: {}", self.committer_tz))?;
        Ok(DateTime::<FixedOffset>::from_naive_utc_and_offset(
            naive_datetime,
            timezone,
        ))
    }

    fn new_from_first_entry_line(parts: &[&str]) -> Result<BlameEntry> {
        if let [sha, source_line_str, result_line_str, num_lines_str] = parts[..] {
            let original_line_number = source_line_str
                .parse::<u32>()
                .map_err(|e| anyhow!("Failed to parse original line number: {}", e))?;
            let final_line_number = result_line_str
                .parse::<u32>()
                .map_err(|e| anyhow!("Failed to parse final line number: {}", e))?;
            let line_count = num_lines_str
                .parse::<u32>()
                .map_err(|e| anyhow!("Failed to parse line count: {}", e))?;

            Ok(BlameEntry {
                sha: sha.to_string(),
                original_line_number,
                final_line_number,
                line_count,
                ..Default::default()
            })
        } else {
            Err(anyhow!(
                "Failed to parse first 'git blame' entry line: {}",
                parts.join(" ").to_string()
            ))
        }
    }
}

// parse_git_blame parses the output of `git blame --incremental`, which returns
// all the blame-entries for a given path incrementally, as it finds them.
//
// Each entry *always* starts with:
//
//     <40-byte-hex-sha1> <sourceline> <resultline> <num-lines>
//
// Each entry *always* ends with:
//
//     filename <whitespace-quoted-filename-goes-here>
//
// Line numbers are 1-indexed.

// A `git blame --incremental` entry looks like this:
//
//    6ad46b5257ba16d12c5ca9f0d4900320959df7f4 2 2 1
//    author Joe Schmoe
//    author-mail <joe.schmoe@example.com>
//    author-time 1709741400
//    author-tz +0100
//    committer Joe Schmoe
//    committer-mail <joe.schmoe@example.com>
//    committer-time 1709741400
//    committer-tz +0100
//    summary Joe's cool commit
//    previous 486c2409237a2c627230589e567024a96751d475 index.js
//    filename index.js
//
// If the entry has the same SHA as an entry that was already printed then no
// signature information is printed:
//
//    6ad46b5257ba16d12c5ca9f0d4900320959df7f4 3 4 1
//    previous 486c2409237a2c627230589e567024a96751d475 index.js
//    filename index.js
//
// More about `--incremental` output: https://mirrors.edge.kernel.org/pub/software/scm/git/docs/git-blame.html
pub fn parse_git_blame(output: &str) -> Result<Vec<BlameEntry>> {
    let mut entries: Vec<BlameEntry> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    let mut current_entry: Option<BlameEntry> = None;

    for line in output.lines() {
        let parts = line.split_whitespace().collect::<Vec<&str>>();
        if parts.len() < 2 {
            continue;
        }

        let mut done = false;
        match &mut current_entry {
            None => {
                let mut new_entry = BlameEntry::new_from_first_entry_line(&parts)?;
                if let Some(existing_entry) = index
                    .get(&new_entry.sha)
                    .and_then(|slot| entries.get(*slot))
                {
                    new_entry.author = existing_entry.author.clone();
                    new_entry.author_mail = existing_entry.author_mail.clone();
                    new_entry.author_time = existing_entry.author_time;
                    new_entry.author_tz = existing_entry.author_tz.clone();
                    new_entry.committer = existing_entry.committer.clone();
                    new_entry.committer_mail = existing_entry.committer_mail.clone();
                    new_entry.committer_time = existing_entry.committer_time;
                    new_entry.committer_tz = existing_entry.committer_tz.clone();
                    new_entry.summary = existing_entry.summary.clone();
                }

                current_entry.replace(new_entry);
            }
            Some(entry) => {
                let Some(key) = parts.first() else {
                    continue;
                };
                let value = parts[1..].join(" ").to_string();
                match *key {
                    "filename" => {
                        entry.filename = value;
                        done = true;
                    }
                    "summary" => entry.summary = value,
                    "previous" => entry.previous = Some(value),

                    "author" => {
                        entry.author = if entry.sha == UNCOMMITTED_SHA {
                            "Not committed".to_string()
                        } else {
                            value
                        }
                    }
                    "author-mail" => entry.author_mail = value,
                    "author-time" => entry.author_time = value.parse::<i64>()?,
                    "author-tz" => entry.author_tz = value,

                    "committer" => {
                        entry.committer = if entry.sha == UNCOMMITTED_SHA {
                            "Not committed".to_string()
                        } else {
                            value
                        }
                    }
                    "committer-mail" => entry.committer_mail = value,
                    "committer-time" => entry.committer_time = value.parse::<i64>()?,
                    "committer-tz" => entry.committer_tz = value,
                    _ => {}
                }
            }
        };

        if done {
            if let Some(entry) = current_entry.take() {
                index.insert(entry.sha.clone(), entries.len());
                entries.push(entry);
            }
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::blame_incremental::parse_git_blame;

    use super::BlameEntry;

    fn read_test_data(filename: &str) -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("test_data");
        path.push(filename);

        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("Could not read test data at {:?}. Is it generated?", path))
    }

    fn assert_eq_golden(entries: &Vec<BlameEntry>, golden_filename: &str) {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("test_data");
        path.push("golden");
        path.push(format!("{}.json", golden_filename));

        let have_json =
            serde_json::to_string_pretty(&entries).expect("could not serialize entries to JSON");

        let update = std::env::var("UPDATE_GOLDEN")
            .map(|val| val.to_ascii_lowercase() == "true")
            .unwrap_or(false);

        if update {
            std::fs::create_dir_all(path.parent().unwrap())
                .expect("could not create golden test data directory");
            std::fs::write(&path, have_json).expect("could not write out golden data");
        } else {
            let want_json =
                std::fs::read_to_string(&path).unwrap_or_else(|_| {
                    panic!("could not read golden test data file at {:?}. Did you run the test with UPDATE_GOLDEN=true before?", path);
                });

            pretty_assertions::assert_eq!(have_json, want_json, "wrong blame entries");
        }
    }

    #[test]
    fn test_parse_git_blame_simple() {
        let output = read_test_data("blame_incremental_simple");
        let entries = parse_git_blame(&output).unwrap();
        assert_eq_golden(&entries, "blame_incremental_simple");
    }

    #[test]
    fn test_parse_git_blame_complex() {
        // This testdata is the `git blame --incremental` output of `editor/src/editor.rs`:
        let output = read_test_data("blame_incremental_complex");
        let entries = parse_git_blame(&output).unwrap();
        assert_eq_golden(&entries, "blame_incremental_complex");
    }
}
