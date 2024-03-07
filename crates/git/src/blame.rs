use fs::repository::GitRepository;
use std::collections::HashMap;
use std::iter;

use anyhow::anyhow;
use anyhow::Result;

use chrono::{DateTime, FixedOffset, LocalResult, TimeZone};
use parking_lot::Mutex;
use std::{fmt, ops::Range, path::Path, sync::Arc};
use sum_tree::SumTree;
use util::ResultExt;

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
pub struct BlameHunk<T> {
    pub buffer_range: Range<T>,

    pub oid: libgit::Oid,
    pub name: Option<String>,
    pub email: Option<String>,
    pub time: DateTime<FixedOffset>,
}

struct Signature {
    name: Option<String>,
    email: Option<String>,
    time: DateTime<FixedOffset>,
}

impl<T> BlameHunk<T> {
    pub fn short_blame(&self) -> String {
        let pretty_commit_id = format!("{}", self.oid);
        pretty_commit_id.chars().take(6).collect::<String>()
    }
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
            short_commit_id,
            self.name.as_deref().unwrap_or("<no name>"),
            self.email.as_deref().unwrap_or("no email"),
            datetime
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
    last_buffer_version: Option<clock::Global>,
    tree: SumTree<BlameHunk<Anchor>>,
}

impl BufferBlame {
    pub fn new_with_cli(path: &Arc<Path>, buffer_contents: &String) -> BufferBlame {
        println!("path: {}", path.display());
        println!("buffer_contents: {}", buffer_contents);
        BufferBlame {
            last_buffer_version: None,
            tree: SumTree::new(),
        }
    }

    pub fn new() -> BufferBlame {
        BufferBlame {
            last_buffer_version: None,
            tree: SumTree::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    pub fn hunks_in_row_range<'a>(
        &'a self,
        range: Range<u32>,
        buffer: &'a text::BufferSnapshot,
    ) -> impl 'a + Iterator<Item = BlameHunk<u32>> {
        let start = buffer.anchor_before(Point::new(range.start, 0));
        let end = buffer.anchor_after(Point::new(range.end, 0));

        self.hunks_intersecting_range(start..end, buffer)
    }

    pub fn hunks_intersecting_range<'a>(
        &'a self,
        range: Range<Anchor>,
        buffer: &'a text::BufferSnapshot,
    ) -> impl 'a + Iterator<Item = BlameHunk<u32>> {
        // TODO: This is just straight-up copy&pasted from git::diff::Diff.
        let mut cursor = self.tree.filter::<_, BlameHunkSummary>(move |summary| {
            let before_start = summary.buffer_range.end.cmp(&range.start, buffer).is_lt();
            let after_end = summary.buffer_range.start.cmp(&range.end, buffer).is_gt();
            !before_start && !after_end
        });

        let anchor_iter = std::iter::from_fn(move || {
            cursor.next(buffer);
            cursor.item()
        })
        .flat_map(move |hunk| {
            [
                (&hunk.buffer_range.start, hunk),
                (&hunk.buffer_range.end, hunk),
            ]
            .into_iter()
        });

        let mut summaries = buffer.summaries_for_anchors_with_payload::<Point, _, _>(anchor_iter);
        iter::from_fn(move || {
            let (start_point, hunk) = summaries.next()?;
            let (end_point, _) = summaries.next()?;

            let end_row = if end_point.column > 0 {
                end_point.row + 1
            } else {
                end_point.row
            };

            // TODO: Why do we have to clone here?
            Some(BlameHunk {
                buffer_range: start_point.row..end_row,
                oid: hunk.oid,
                name: hunk.name.clone(),
                email: hunk.email.clone(),
                time: hunk.time,
            })
        })
    }

    pub fn update(
        &mut self,
        repo: Arc<Mutex<dyn GitRepository>>,
        path: &Path,
        buffer: &text::BufferSnapshot,
    ) -> Result<()> {
        let repo = repo.lock();

        let start_time = std::time::Instant::now();
        let blame = repo.blame_path(path)?;
        let buffer_text = buffer.as_rope().to_string();
        let blame_buffer = blame.blame_buffer(buffer_text.as_bytes())?;
        println!("git blame, execution time: {:?}", start_time.elapsed());

        println!("using blame.get_line() api:");
        for (line_idx, line) in buffer_text.lines().enumerate() {
            if let Some(hunk) = blame_buffer.get_line(line_idx + 1) {
                println!(
                    "line: {}, oid: {}, start: {}, line count: {}",
                    line_idx,
                    hunk.final_commit_id(),
                    hunk.final_start_line(),
                    hunk.lines_in_hunk()
                );
            }
        }

        println!("iterating over hunks:");
        let mut tree = SumTree::new();
        let mut signatures = HashMap::default();
        for hunk_index in 0..blame_buffer.len() {
            let hunk =
                Self::process_blame_hunk(&blame_buffer, hunk_index, &buffer, &mut signatures)
                    .log_err()
                    .flatten();
            if let Some(hunk) = hunk {
                tree.push(hunk, buffer);
            }
        }

        self.tree = tree;
        self.last_buffer_version = Some(buffer.version().clone());

        Ok(())
    }

    fn process_blame_hunk(
        blame: &libgit::Blame<'_>,
        hunk_index: usize,
        buffer: &text::BufferSnapshot,
        signatures: &mut HashMap<libgit::Oid, Signature>,
    ) -> Result<Option<BlameHunk<Anchor>>> {
        let Some(hunk) = blame.get_index(hunk_index) else {
            return Ok(None);
        };

        let oid = hunk.final_commit_id();
        if oid.is_zero() {
            println!(
                "hunk: {}, zero commit, start: {}, line count: {}",
                hunk_index,
                hunk.final_start_line(),
                hunk.lines_in_hunk()
            );
            return Ok(None);
        } else {
            println!(
                "hunk: {}, oid: {}, start: {}, line_count: {}",
                hunk_index,
                oid,
                hunk.final_start_line(),
                hunk.lines_in_hunk()
            );
        }

        let line_count = hunk.lines_in_hunk();
        if line_count == usize::MAX {
            // TODO: not sure when this happens.
            return Ok(None);
        }

        let line_count = line_count as u32;
        let start_line = hunk.final_start_line() as u32 - 1;

        let start = Point::new(start_line, 0);
        let end = Point::new(start_line + line_count, 0);

        // println!("{}, start: {}, end: {}", oid, start.row, end.row);
        let buffer_range = buffer.anchor_before(start)..buffer.anchor_before(end);

        if let Some(signature) = signatures.get(&oid) {
            Ok(Some(BlameHunk {
                oid,
                name: signature.name.clone(),
                email: signature.email.clone(),
                time: signature.time.clone(),
                buffer_range,
            }))
        } else {
            let final_signature = hunk.final_signature();
            let name = final_signature.name().map(String::from);
            let email = final_signature.email().map(String::from);
            let when = hunk.final_signature().when();
            let time = git_time_to_chrono(when)?;

            let signature = Signature { name, email, time };
            let blame_hunk = BlameHunk {
                oid,
                name: signature.name.clone(),
                email: signature.email.clone(),
                time: signature.time.clone(),
                buffer_range,
            };
            signatures.insert(oid, signature);

            Ok(Some(blame_hunk))
        }
    }
}

fn git_time_to_chrono(time: libgit::Time) -> Result<DateTime<FixedOffset>> {
    let offset_seconds = time.offset_minutes() * 60; // convert minutes to seconds
    let fixed_offset = FixedOffset::east_opt(offset_seconds)
        .ok_or_else(|| anyhow!("failed to parse timezone in 'git blame' timestamp"))?;

    match fixed_offset.timestamp_opt(time.seconds(), 0) {
        LocalResult::Single(datetime) => Ok(datetime),
        _ => Err(anyhow!("failed to parse 'git blame' timestamp")),
    }
}
