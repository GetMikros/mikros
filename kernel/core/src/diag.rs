//! The diagnostic build's account of the kernel's state, for
//! `diag_snapshot` and the reports of panics.
//!
//! The account is text: a record per line, of fields separated by tabs,
//! the first of which names the record. Only the kernel and the
//! diagnostic service read it:
//!
//! - `U uptime_ns`
//! - `O type live`, for each object type
//! - `B kernel_bytes pages`, charged to the root budget and below
//! - `C cpu thread idle_ns`, for each processor; thread 0 is the idle loop
//! - `P koid threads handles mapped_bytes name`, for each process
//! - `T koid process state waits_on cpu run_ns name`, for each thread of
//!   a process, waits_on the koid of the object it waits on, `futex` or 0
//!
//! The architecture adds its own records to these.

use crate::object::budget::{Budget, Scratch};
use crate::object::{Object, ObjectType};
use crate::process::Process;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

/// The live objects of each type, by the type's value.
static LIVE: [AtomicU64; ObjectType::ALL.len() + 1] =
    [const { AtomicU64::new(0) }; ObjectType::ALL.len() + 1];

/// The root budget, which every charge reaches.
static ROOT: AtomicPtr<Budget> = AtomicPtr::new(core::ptr::null_mut());

/// The `waits_on` of a thread that waits on a futex.
pub const FUTEX: u64 = u64::MAX;

/// Counts an object of type `kind` made.
pub fn made(kind: ObjectType) {
    LIVE[kind as usize].fetch_add(1, Ordering::Relaxed);
}

/// Counts an object of type `kind` gone.
pub fn gone(kind: ObjectType) {
    LIVE[kind as usize].fetch_sub(1, Ordering::Relaxed);
}

/// Keeps `budget`, the root budget, for the account.
pub fn set_root_budget(budget: &Arc<Budget>) {
    let kept = Arc::into_raw(budget.clone()).cast_mut();
    ROOT.store(kept, Ordering::Release);
}

/// Text that grows as it is written, until memory runs out.
#[derive(Default)]
pub struct Text(pub Vec<u8>);

impl Write for Text {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.try_reserve(s.len()).map_err(|_| fmt::Error)?;
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// `text` with tabs, line ends and other control characters as spaces.
fn field(text: &str) -> impl fmt::Display + '_ {
    struct Field<'a>(&'a str);
    impl fmt::Display for Field<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            for c in self.0.chars() {
                f.write_char(if c.is_control() { ' ' } else { c })?;
            }
            Ok(())
        }
    }
    Field(text)
}

/// The records that need no lock, which a panic report can give too.
pub fn summary(out: &mut dyn Write) -> fmt::Result {
    writeln!(out, "U\t{}", crate::arch::monotonic_now())?;
    for kind in ObjectType::ALL {
        let live = LIVE[kind as usize].load(Ordering::Relaxed);
        writeln!(out, "O\t{kind:?}\t{live}")?;
    }
    let root = ROOT.load(Ordering::Acquire);
    // SAFETY: `set_root_budget` keeps a reference to the budget forever.
    if let Some(root) = unsafe { root.as_ref() } {
        let (kernel, pages) = root.usage();
        writeln!(out, "B\t{kernel}\t{pages}")?;
    }
    for index in 0..crate::cpu::count() {
        let Some(cpu) = crate::cpu::of(index) else {
            continue;
        };
        let thread = cpu.running.load(Ordering::Relaxed);
        let idle = cpu.idle.load(Ordering::Relaxed);
        writeln!(out, "C\t{index}\t{thread}\t{idle}")?;
    }
    Ok(())
}

/// The records of every process and of its threads.
pub fn tasks(out: &mut dyn Write) -> fmt::Result {
    let processes: Vec<Arc<Process>> = crate::process::all().map_err(|_| fmt::Error)?;
    for process in &processes {
        let threads = process
            .live_threads(&mut Scratch::new(None))
            .map_err(|_| fmt::Error)?;
        writeln!(
            out,
            "P\t{}\t{}\t{}\t{}\t{}",
            process.header().koid(),
            threads.len(),
            process.handle_count(),
            process.mapped_bytes(),
            field(process.name().as_str())
        )?;
        for t in &threads {
            writeln!(
                out,
                "T\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                t.header().koid(),
                process.header().koid(),
                state_name(t.state()),
                match t.waits_on() {
                    FUTEX => "futex".into(),
                    koid => alloc::format!("{koid}"),
                },
                t.owner_cpu(),
                t.cpu_time(),
                field(t.name().as_str())
            )?;
        }
    }
    Ok(())
}

fn state_name(state: u32) -> &'static str {
    use crate::sched::{BLOCKED, DEAD, EXCEPTION, NEW, RUNNABLE, SUSPENDED};
    match state {
        NEW => "new",
        RUNNABLE => "runnable",
        BLOCKED => "blocked",
        EXCEPTION => "exception",
        DEAD => "dead",
        SUSPENDED => "suspended",
        _ => "?",
    }
}

/// Records, while it lives, that the running thread waits on the object
/// with `koid`, or a futex for `FUTEX`.
pub struct Waiting(Option<Arc<crate::sched::Thread>>);

impl Waiting {
    pub fn on(koid: u64) -> Waiting {
        let thread = crate::sched::current();
        if let Some(t) = &thread {
            t.set_waits_on(koid);
        }
        Waiting(thread)
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            t.set_waits_on(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_fields_without_control_characters() {
        let mut text = Text::default();
        write!(text, "{}", field("a\tb\nc")).unwrap();
        assert_eq!(text.0, b"a b c");
    }

    #[test]
    fn lists_live_objects_and_names_states() {
        let mut text = Text::default();
        summary(&mut text).unwrap();
        let text = core::str::from_utf8(&text.0).unwrap();
        assert!(text.starts_with("U\t"));
        assert!(text.contains("\nO\tEvent\t"));
        assert!(text.contains("\nC\t0\t"));
        assert_eq!(state_name(crate::sched::BLOCKED), "blocked");
    }
}
