//! The diagnostic build's account of the kernel's state, which
//! `diag_snapshot` gives as lines of tab-separated fields, as text tables
//! for people: one table per section, each with a header row, and the
//! user and app ID of each process from the process manager.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

#[cfg(test)]
mod tests;

const NANOS: i64 = 1_000_000_000;
const PAGE: u64 = 4096;

/// What the process manager says of the process with a koid: its user ID
/// and its app ID.
pub struct Identity {
    pub user: u32,
    pub app_id: String,
}

/// A table: its title, its header row and its rows.
struct Table {
    title: &'static str,
    header: &'static [&'static str],
    rows: Vec<Vec<String>>,
}

impl Table {
    fn new(title: &'static str, header: &'static [&'static str]) -> Table {
        Table {
            title,
            header,
            rows: Vec::new(),
        }
    }

    /// Writes the table with its columns aligned, after a line with its
    /// title.
    fn write(&self, out: &mut String) {
        let mut widths: Vec<usize> = self.header.iter().map(|h| h.chars().count()).collect();
        for row in &self.rows {
            for (w, cell) in widths.iter_mut().zip(row) {
                *w = (*w).max(cell.chars().count());
            }
        }
        let _ = writeln!(out, "{}", self.title);
        let header: Vec<String> = self.header.iter().map(|h| String::from(*h)).collect();
        for row in core::iter::once(&header).chain(&self.rows) {
            let mut line = String::new();
            for (i, (cell, w)) in row.iter().zip(&widths).enumerate() {
                if i > 0 {
                    line.push_str("  ");
                }
                let _ = write!(line, "{cell:<w$}");
            }
            let _ = writeln!(out, "{}", line.trim_end());
        }
        out.push('\n');
    }
}

/// Nanoseconds as seconds with three decimals.
fn seconds(ns: &str) -> String {
    match ns.parse::<i64>() {
        Ok(ns) => format!("{}.{:03}", ns / NANOS, ns % NANOS / 1_000_000),
        Err(_) => String::from(ns),
    }
}

/// Bytes, or a count of pages, as KiB, MiB or GiB.
fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{}.{} GiB", b >> 30, ((b % (1 << 30)) * 10) >> 30),
        b if b >= 1 << 20 => format!("{}.{} MiB", b >> 20, ((b % (1 << 20)) * 10) >> 20),
        b => format!("{} KiB", b.div_ceil(1024)),
    }
}

fn number(text: &str) -> u64 {
    text.parse().unwrap_or(0)
}

/// The tables of `snapshot`, with the identity that `identity` gives each
/// process by its koid, cut at the end of the last line that fits in
/// `max` bytes.
pub fn render(snapshot: &str, identity: &dyn Fn(u64) -> Option<Identity>, max: usize) -> String {
    let mut system = Table::new("System", &["uptime s", "build ID"]);
    let mut uptime = String::new();
    let mut processes = Table::new(
        "Processes",
        &[
            "koid", "user", "app ID", "threads", "handles", "mapped", "name",
        ],
    );
    let mut threads = Table::new(
        "Threads",
        &[
            "koid", "process", "state", "waits on", "cpu", "run s", "name",
        ],
    );
    let mut objects = Table::new("Kernel objects", &["type", "live"]);
    let mut heap = Table::new("Kernel heap", &["block size", "in use", "bytes"]);
    let mut memory = Table::new("Memory", &["what", "amount"]);
    let mut cpus = Table::new(
        "Processors",
        &["cpu", "thread", "idle s", "shootdown pending"],
    );
    let mut domains = Table::new("IOMMU domains", &["device", "domain", "pinned pages"]);
    let mut budget = None;
    let mut frames = None;
    let mut pending = Vec::new();
    for line in snapshot.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let cells = |from: usize| fields[from..].iter().map(|f| String::from(*f)).collect();
        match (fields[0], fields.len()) {
            ("U", 2) => uptime = seconds(fields[1]),
            ("K", 2) => system.rows.push([uptime.clone(), fields[1].into()].into()),
            ("O", 3) => objects.rows.push(cells(1)),
            ("B", 3) => budget = Some((number(fields[1]), number(fields[2]))),
            ("C", 4) => cpus
                .rows
                .push([fields[1].into(), fields[2].into(), seconds(fields[3])].into()),
            ("S", 3) => pending.push((String::from(fields[1]), String::from(fields[2]))),
            ("H", 3) if fields[1] == "pages" => {
                let pages = number(fields[2]);
                heap.rows
                    .push(["pages".into(), fields[2].into(), size(pages * PAGE)].into());
            }
            ("H", 3) => {
                let bytes = number(fields[1]) * number(fields[2]);
                heap.rows
                    .push([fields[1].into(), fields[2].into(), size(bytes)].into());
            }
            ("F", 4) => frames = Some((number(fields[1]), number(fields[2]), number(fields[3]))),
            ("D", 3) => {
                let (used, total) = (number(fields[1]), number(fields[2]));
                memory.rows.push(
                    [
                        "kernel dynamic region".into(),
                        format!("{} of {}", size(used), size(total)),
                    ]
                    .into(),
                );
            }
            ("I", 4) => domains.rows.push(cells(1)),
            ("P", 6) => {
                let koid = number(fields[1]);
                let (user, app) = match identity(koid) {
                    Some(i) => (format!("{}", i.user), i.app_id),
                    None => (String::new(), String::new()),
                };
                let mapped = size(number(fields[4]));
                processes.rows.push(
                    [
                        fields[1].into(),
                        user,
                        app,
                        fields[2].into(),
                        fields[3].into(),
                        mapped,
                        fields[5].into(),
                    ]
                    .into(),
                );
            }
            ("T", 8) => {
                let mut row: Vec<String> = cells(1);
                row[5] = seconds(fields[6]);
                threads.rows.push(row);
            }
            _ => {}
        }
    }
    for row in &mut cpus.rows {
        let flag = pending
            .iter()
            .find(|(cpu, _)| *cpu == row[0])
            .map(|p| p.1.clone());
        row.push(match flag.as_deref() {
            Some("1") => "yes".into(),
            Some(_) => "no".into(),
            None => String::new(),
        });
    }
    if let Some((total, free, pinned)) = frames {
        let pages = budget.map_or(0, |b| b.1);
        let rows: [(&str, u64); 5] = [
            ("frames in total", total),
            ("free", free),
            ("in memory objects", pages),
            (
                "kernel and other",
                total.saturating_sub(free).saturating_sub(pages),
            ),
            ("pinned for DMA", pinned),
        ];
        let mut first = Vec::new();
        for (what, frames) in rows {
            first.push([what.into(), format!("{frames} ({})", size(frames * PAGE))].into());
        }
        first.append(&mut memory.rows);
        memory.rows = first;
    }
    if let Some((kernel, _)) = budget {
        memory
            .rows
            .push(["kernel memory charged".into(), size(kernel)].into());
    }
    let mut out = String::new();
    for table in [
        &system, &memory, &cpus, &processes, &threads, &objects, &heap, &domains,
    ] {
        table.write(&mut out);
    }
    // Cut at the end of the last line that fits.
    if out.len() > max {
        let mut fits = max;
        while !out.is_char_boundary(fits) {
            fits -= 1;
        }
        let end = out[..fits].rfind('\n').map_or(0, |at| at + 1);
        out.truncate(end);
    }
    out
}
