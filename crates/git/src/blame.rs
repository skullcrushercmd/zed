use fs::repository::GitRepository;

use anyhow::Result;
use chrono::{DateTime, FixedOffset, LocalResult, TimeZone};
use libgit::Oid;
use parking_lot::Mutex;
use std::{fmt, ops::Range, path::Path, sync::Arc};
use sum_tree::SumTree;

use text::{Anchor, Point};

pub use git2 as libgit;

// DiffHunk is the data
// DiffHunk<u32> has a `.status`
// DiffHunk<Anchor> implements `sum_tree::Item`
//   summary is `DiffHunkSummary` with `buffer_range: Range<Anchor>`

// DiffHunkSummary implements `sum_tree::Summary`
// when that gets added via `add_summary` with another summary
// it expands its range.

// BufferDiff, `hunks_in_row_range` takes in a `Range<u32>`
// converts the range to anchors
// then sets `hunks_intersecting_range`, which takes in anchors

// the real magic happens in `hunks_intersecting_range`

// - builds a cursor that only gives the hunks in the tree that are in the range
// - then takes the hunks that cursor returns and turns them into pair of start/end

// - it then calls `buffer.summaries_for_anchors_with_payload` to essentially convert
// the `Anchor`s into `Point`s. The `payload` is the diff information.

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlameHunk<T> {
    buffer_range: Range<T>,

    oid: libgit::Oid,
    name: String,
    email: String,
    time: DateTime<FixedOffset>,
}

impl BlameHunk<u32> {}

impl sum_tree::Item for BlameHunk<Anchor> {
    type Summary = BlameHunkSummary;

    fn summary(&self) -> Self::Summary {
        BlameHunkSummary {
            buffer_range: self.buffer_range.clone(),
        }
    }
}

impl<T> fmt::Display for BlameHunk<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), std::fmt::Error> {
        let datetime = self.time.format("%Y-%m-%d %H:%M").to_string();

        let pretty_commit_id = format!("{}", self.oid);
        let short_commit_id = pretty_commit_id.chars().take(6).collect::<String>();

        write!(
            f,
            "{} - {} <{}> - ({})",
            short_commit_id, self.name, self.email, datetime
        )
    }
}

#[derive(Debug, Default, Clone)]
pub struct BlameHunkSummary {
    buffer_range: Range<Anchor>,
}

impl sum_tree::Summary for BlameHunkSummary {
    type Context = text::BufferSnapshot;

    fn add_summary(&mut self, other: &Self, buffer: &Self::Context) {
        self.buffer_range.start = self
            .buffer_range
            .start
            .min(&other.buffer_range.start, buffer);
        self.buffer_range.end = self.buffer_range.end.max(&other.buffer_range.end, buffer);
    }
}

#[derive(Clone)]
pub struct BufferBlame {
    tree: SumTree<BlameHunk<Anchor>>,
}

impl BufferBlame {
    pub fn new() -> BufferBlame {
        BufferBlame {
            tree: SumTree::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    pub fn update(
        &mut self,
        repo: Arc<Mutex<dyn GitRepository>>,
        path: &Path,
        buffer: String,
    ) -> Result<()> {
        let repo = repo.lock();

        let blame = repo.blame_path(path)?;
        let blame_buffer = blame.blame_buffer(buffer.as_bytes())?;

        for hunk_index in 0..blame_buffer.len() {
            let hunk = Self::process_blame_hunk(&blame_buffer, hunk_index, &buffer);
            match hunk {
                Some(hunk) => println!("hunk {hunk_index}: {}", hunk),
                None => {}
            }
        }

        Ok(())
    }

    fn process_blame_hunk(
        blame: &libgit::Blame<'_>,
        hunk_index: usize,
        buffer: &String,
    ) -> Option<BlameHunk<u32>> {
        let Some(hunk) = blame.get_index(hunk_index) else {
            println!("no hunk {hunk_index} found");
            return None;
        };

        let oid = hunk.final_commit_id();
        if oid == git2::Oid::zero() {
            let start_line = hunk.final_start_line();
            let line_count = hunk.lines_in_hunk();
            println!("hunk start_line={start_line} (line_count={line_count}) -- not committed!");
            return None;
        }

        let start_line = hunk.final_start_line();
        let line_count = hunk.lines_in_hunk();
        if line_count == usize::MAX {
            // Not sure when this happens.
            println!("what the hell");
            return None;
        }

        // TODO: This is wrong. We need to figure out whether this is in the buffer
        let start = (start_line as u32) - 1;
        let end = (start_line as u32) - 1 + (line_count as u32);
        let buffer_range = start..end;

        let final_signature = hunk.final_signature();
        let name = final_signature.name()?.to_string();
        let email = final_signature.email()?.to_string();
        let when = hunk.final_signature().when();
        let time = git_time_to_chrono(when)?;

        Some(BlameHunk {
            oid,
            name,
            email,
            time,
            buffer_range,
        })
    }
}

fn git_time_to_chrono(time: libgit::Time) -> Option<DateTime<FixedOffset>> {
    let offset_seconds = time.offset_minutes() * 60; // convert minutes to seconds
    let fixed_offset = FixedOffset::east_opt(offset_seconds)?;

    match fixed_offset.timestamp_opt(time.seconds(), 0) {
        LocalResult::Single(datetime) => Some(datetime),
        _ => None, // you might want to handle this case differently, depending on your needs
    }
}
