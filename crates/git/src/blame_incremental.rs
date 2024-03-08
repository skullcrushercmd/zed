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

#[derive(Default, Debug)]
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
                            "Not commited".to_string()
                        } else {
                            value
                        }
                    }
                    "author-mail" => entry.author_mail = value,
                    "author-time" => entry.author_time = value.parse::<i64>()?,
                    "author-tz" => entry.author_tz = value,

                    "committer" => {
                        entry.committer = if entry.sha == UNCOMMITTED_SHA {
                            "Not commited".to_string()
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
    use crate::blame_incremental::{parse_git_blame, UNCOMMITTED_SHA};

    macro_rules! assert_author_commiter {
        ($entry:expr, $author:expr, $mail:expr, $time:expr, $tz:expr) => {
            assert_eq!($entry.author, $author);
            assert_eq!($entry.author_mail, $mail);
            assert_eq!($entry.author_time, $time);
            assert_eq!($entry.author_tz, $tz);
            assert_eq!($entry.committer, $author);
            assert_eq!($entry.committer_mail, $mail);
            assert_eq!($entry.committer_time, $time);
            assert_eq!($entry.committer_tz, $tz);
        };
    }

    macro_rules! assert_uncommitted {
        ($entry:expr, $time:expr, $tz:expr) => {
            assert_eq!($entry.author, "Not Committed Yet");
            assert_eq!($entry.author_mail, "<not.committed.yet>");
            assert_eq!($entry.author_time, $time);
            assert_eq!($entry.author_tz, $tz);
            assert_eq!($entry.committer, "Not Committed Yet");
            assert_eq!($entry.committer_mail, "<not.committed.yet>");
            assert_eq!($entry.committer_time, $time);
            assert_eq!($entry.committer_tz, $tz);
        };
    }

    #[test]
    fn test_parse_incremental_output() {
        let output = r#"
0000000000000000000000000000000000000000 3 3 1
author Not Committed Yet
author-mail <not.committed.yet>
author-time 1709895274
author-tz +0100
committer Not Committed Yet
committer-mail <not.committed.yet>
committer-time 1709895274
committer-tz +0100
summary Version of index.js from index.js
previous a7037b4567dd171bfe563c761354ec9236c803b3 index.js
filename index.js
0000000000000000000000000000000000000000 7 7 2
previous a7037b4567dd171bfe563c761354ec9236c803b3 index.js
filename index.js
c8d34ae30c87e59aaa5eb65f6c64d6206f525d7c 7 6 1
author Thorsten Ball
author-mail <mrnugget@example.com>
author-time 1709808710
author-tz +0100
committer Thorsten Ball
committer-mail <mrnugget@example.com>
committer-time 1709808710
committer-tz +0100
summary Make a commit
previous 6ad46b5257ba16d12c5ca9f0d4900320959df7f4 index.js
filename index.js
6ad46b5257ba16d12c5ca9f0d4900320959df7f4 2 2 1
author Joe Schmoe
author-mail <joe.schmoe@example.com>
author-time 1709741400
author-tz +0100
committer Joe Schmoe
committer-mail <joe.schmoe@example.com>
committer-time 1709741400
committer-tz +0100
summary Joe's cool commit
previous 486c2409237a2c627230589e567024a96751d475 index.js
filename index.js
6ad46b5257ba16d12c5ca9f0d4900320959df7f4 3 4 1
previous 486c2409237a2c627230589e567024a96751d475 index.js
filename index.js
6ad46b5257ba16d12c5ca9f0d4900320959df7f4 13 9 1
previous 486c2409237a2c627230589e567024a96751d475 index.js
filename index.js
486c2409237a2c627230589e567024a96751d475 3 1 1
author Thorsten Ball
author-mail <mrnugget@example.com>
author-time 1709129122
author-tz +0100
committer Thorsten Ball
committer-mail <mrnugget@example.com>
committer-time 1709129122
committer-tz +0100
summary Get to a state where eslint would change code and imports
previous 504065e448b467e79920040f22153e9d2ea0fd6e index.js
filename index.js
504065e448b467e79920040f22153e9d2ea0fd6e 3 5 1
author Thorsten Ball
author-mail <mrnugget@example.com>
author-time 1709128963
author-tz +0100
committer Thorsten Ball
committer-mail <mrnugget@example.com>
committer-time 1709128963
committer-tz +0100
summary Add some stuff
filename index.js
504065e448b467e79920040f22153e9d2ea0fd6e 21 10 1
filename index.js
"#;

        let entries = parse_git_blame(&output).unwrap();
        assert_eq!(entries.len(), 9);

        assert_eq!(entries[0].sha, UNCOMMITTED_SHA);
        assert_eq!(entries[0].original_line_number, 3);
        assert_eq!(entries[0].final_line_number, 3);
        assert_eq!(entries[0].line_count, 1);
        assert_eq!(entries[0].filename, "index.js");
        assert_eq!(entries[0].summary, "Version of index.js from index.js");
        assert_eq!(
            entries[0].previous,
            Some("a7037b4567dd171bfe563c761354ec9236c803b3 index.js".to_owned())
        );
        assert_uncommitted!(entries[0], 1709895274, "+0100");

        assert_eq!(entries[1].sha, UNCOMMITTED_SHA);
        assert_eq!(entries[1].original_line_number, 7);
        assert_eq!(entries[1].final_line_number, 7);
        assert_eq!(entries[1].line_count, 2);
        assert_eq!(entries[1].filename, "index.js");
        assert_eq!(entries[1].summary, "Version of index.js from index.js");
        assert_eq!(
            entries[1].previous,
            Some("a7037b4567dd171bfe563c761354ec9236c803b3 index.js".to_owned())
        );
        assert_uncommitted!(entries[1], 1709895274, "+0100");

        assert_eq!(entries[2].sha, "c8d34ae30c87e59aaa5eb65f6c64d6206f525d7c");
        assert_eq!(entries[2].original_line_number, 7);
        assert_eq!(entries[2].final_line_number, 6);
        assert_eq!(entries[2].line_count, 1);
        assert_eq!(entries[2].filename, "index.js");
        assert_eq!(entries[2].summary, "Make a commit");
        assert_eq!(
            entries[2].previous,
            Some("6ad46b5257ba16d12c5ca9f0d4900320959df7f4 index.js".to_owned())
        );
        assert_author_commiter!(
            entries[2],
            "Thorsten Ball",
            "<mrnugget@example.com>",
            1709808710,
            "+0100"
        );

        assert_eq!(entries[3].sha, "6ad46b5257ba16d12c5ca9f0d4900320959df7f4");
        assert_eq!(entries[3].original_line_number, 2);
        assert_eq!(entries[3].final_line_number, 2);
        assert_eq!(entries[3].line_count, 1);
        assert_eq!(entries[3].filename, "index.js");
        assert_eq!(entries[3].summary, "Joe's cool commit");
        assert_eq!(
            entries[3].previous,
            Some("486c2409237a2c627230589e567024a96751d475 index.js".to_owned())
        );
        assert_author_commiter!(
            entries[3],
            "Joe Schmoe",
            "<joe.schmoe@example.com>",
            1709741400,
            "+0100"
        );

        assert_eq!(entries[4].sha, "6ad46b5257ba16d12c5ca9f0d4900320959df7f4");
        assert_eq!(entries[4].original_line_number, 3);
        assert_eq!(entries[4].final_line_number, 4);
        assert_eq!(entries[4].line_count, 1);
        assert_eq!(
            entries[4].previous,
            Some("486c2409237a2c627230589e567024a96751d475 index.js".to_owned())
        );
        assert_eq!(entries[5].sha, "6ad46b5257ba16d12c5ca9f0d4900320959df7f4");
        assert_eq!(entries[5].original_line_number, 13);
        assert_eq!(entries[5].final_line_number, 9);
        assert_eq!(entries[5].line_count, 1);
        assert_eq!(
            entries[5].previous,
            Some("486c2409237a2c627230589e567024a96751d475 index.js".to_owned())
        );

        assert_eq!(entries[6].sha, "486c2409237a2c627230589e567024a96751d475");
        assert_eq!(entries[6].original_line_number, 3);
        assert_eq!(entries[6].final_line_number, 1);
        assert_eq!(entries[6].line_count, 1);
        assert_eq!(
            entries[6].previous,
            Some("504065e448b467e79920040f22153e9d2ea0fd6e index.js".to_owned())
        );
        assert_author_commiter!(
            entries[6],
            "Thorsten Ball",
            "<mrnugget@example.com>",
            1709129122,
            "+0100"
        );

        assert_eq!(entries[7].sha, "504065e448b467e79920040f22153e9d2ea0fd6e");
        assert_eq!(entries[7].original_line_number, 3);
        assert_eq!(entries[7].final_line_number, 5);
        assert_eq!(entries[7].line_count, 1);
        assert_author_commiter!(
            entries[7],
            "Thorsten Ball",
            "<mrnugget@example.com>",
            1709128963,
            "+0100"
        );
        assert_eq!(entries[8].sha, "504065e448b467e79920040f22153e9d2ea0fd6e");
        assert_eq!(entries[8].original_line_number, 21);
        assert_eq!(entries[8].final_line_number, 10);
        assert_eq!(entries[8].line_count, 1);
    }
}
