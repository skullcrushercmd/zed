use fs::repository::GitRepository;

use anyhow::Result;
use chrono::{DateTime, FixedOffset, LocalResult, TimeZone};
use libgit::Oid;
use parking_lot::Mutex;
use std::{ops::Range, path::Path, sync::Arc};
use sum_tree::SumTree;

use text::Anchor;

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
pub struct BlameHunk<T> {
    pub buffer_range: Range<T>,
    pub commit: Oid,
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

        for (idx, line) in buffer.lines().enumerate() {
            println!("buffer line {idx}");
            match blame_buffer.get_line(idx + 1) {
                Some(hunk) => {
                    let commit_id = hunk.final_commit_id();
                    let uncommitted = commit_id == git2::Oid::zero();
                    if uncommitted {
                        println!("uncommitted - {}", line);
                    } else {
                        let final_signature = hunk.final_signature();
                        let name = final_signature.name().unwrap_or_default();
                        let email = final_signature.email().unwrap_or_default();

                        let when = hunk.final_signature().when();
                        let datetime = git_time_to_chrono(when)
                            .map(|datetime| datetime.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_else(|| format!("unknown!"));

                        let pretty_commit_id = format!("{}", commit_id);
                        let short_commit_id = pretty_commit_id.chars().take(6).collect::<String>();

                        println!(
                            "{} - {} <{}> - ({}) - {}",
                            short_commit_id, name, email, datetime, line
                        );
                    }
                }
                None => {
                    println!("no commit found - {line}");
                }
            }
        }

        Ok(())
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
