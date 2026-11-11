//! The system that msh's library runs on: files and folders through
//! std, programs started with `std::process::Command` and joined by
//! pipes, and, in an interactive shell, each pipeline in a process group
//! of its own, in the terminal's foreground unless it runs in the
//! background.

use crate::sys::{self, Change};
use mikros_msh::ast::RedirectionKind;
use mikros_msh::eval::glob::Folders;
use mikros_msh::eval::{Input, Program, Ran, Run, Stream, System};
use mikros_msh::jobs::{Ended, Jobs, Lookup, State};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;

/// How each process of a started pipeline ended, in order, once it did.
struct Started {
    pids: Vec<i32>,
    ended: Vec<Option<Ended>>,
}

pub struct Os {
    /// Whether pipelines run in process groups of their own, as an
    /// interactive shell's do.
    job_control: bool,
    /// The shell's process group, which takes the terminal back after a
    /// job in the foreground.
    group: i32,
    jobs: Jobs,
    started: BTreeMap<usize, Started>,
}

/// `e` as an error message, without the error number.
pub fn reason(e: &io::Error) -> String {
    let text = e.to_string();
    let text = match text.find(" (os error") {
        Some(at) => &text[..at],
        None => &text,
    };
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn file_error(path: &str, e: io::Error) -> String {
    format!("{path}: {}", reason(&e))
}

/// A copy of standard stream `fd` of a program, the shell's own when
/// it has none of its own.
fn copy(own: &Option<OwnedFd>, fd: i32) -> Result<OwnedFd, String> {
    let copied = match own {
        Some(own) => own.try_clone(),
        None if fd == 1 => io::stdout().as_fd().try_clone_to_owned(),
        None => io::stderr().as_fd().try_clone_to_owned(),
    };
    copied.map_err(|e| reason(&e))
}

fn stdio(fd: Option<OwnedFd>) -> Stdio {
    fd.map_or_else(Stdio::inherit, Stdio::from)
}

fn pipe() -> Result<(OwnedFd, OwnedFd), String> {
    let (reader, writer) = io::pipe().map_err(|e| reason(&e))?;
    Ok((reader.into(), writer.into()))
}

impl Os {
    pub fn new(job_control: bool) -> Os {
        Os {
            job_control,
            group: sys::group(),
            jobs: Jobs::new(),
            started: BTreeMap::new(),
        }
    }

    /// Starts `program` with standard input `stdin` and output `stdout`,
    /// the shell's for `None`, and its redirections, in process group
    /// `group`, or in a new one for 0, and gives its process ID.
    fn start(
        &self,
        program: &Program,
        stdin: Option<OwnedFd>,
        stdout: Option<OwnedFd>,
        group: i32,
    ) -> Result<i32, String> {
        let mut ends = [stdin, stdout, None];
        for (kind, target) in &program.redirections {
            let opened = |file: io::Result<File>| match file {
                Ok(file) => Ok(Some(OwnedFd::from(file))),
                Err(e) => Err(file_error(target, e)),
            };
            match kind {
                RedirectionKind::Input => ends[0] = opened(File::open(target))?,
                RedirectionKind::Output => ends[1] = opened(File::create(target))?,
                RedirectionKind::Append => {
                    let file = OpenOptions::new().append(true).create(true).open(target);
                    ends[1] = opened(file)?;
                }
                RedirectionKind::Error => ends[2] = opened(File::create(target))?,
                RedirectionKind::ErrorToOutput => ends[2] = Some(copy(&ends[1], 1)?),
                RedirectionKind::OutputToError => ends[1] = Some(copy(&ends[2], 2)?),
            }
        }
        let [stdin, stdout, stderr] = ends;
        let mut command = Command::new(&program.path);
        if let Some((name, args)) = program.args.split_first() {
            command.arg0(name).args(args);
        }
        command
            .env_clear()
            .envs(program.environment.iter().map(|(k, v)| (k, v)))
            .stdin(stdio(stdin))
            .stdout(stdio(stdout))
            .stderr(stdio(stderr));
        let child = command.spawn().map_err(|e| file_error(&program.path, e))?;
        let pid = child.id() as i32;
        if self.job_control {
            // The program may have ended already, which is no failure.
            let _ = sys::set_group(pid, if group == 0 { pid } else { group });
        }
        Ok(pid)
    }

    /// Records that process `pid` changed as `change`.
    fn changed(&mut self, pid: i32, change: Change) {
        let ended = match change {
            Change::Stopped(signal) => {
                self.jobs.stopped(pid, signal);
                return;
            }
            Change::Exited(code) => Ended::Exited(code),
            Change::Signaled(signal) => Ended::Signaled(signal),
        };
        let Some(number) = self.jobs.of_process(pid) else {
            return;
        };
        let Some(started) = self.started.get_mut(&number) else {
            return;
        };
        let Some(at) = started.pids.iter().position(|&p| p == pid) else {
            return;
        };
        started.ended[at] = Some(ended);
        let last = at + 1 == started.pids.len();
        self.jobs.ended(pid, ended, last);
    }

    /// Waits until job `number` ended or stopped, and gives how each of
    /// its processes ended, or nothing when it stopped.
    fn wait_for(&mut self, number: usize) -> Vec<Ended> {
        loop {
            let Some(job) = self.jobs.get(number) else {
                return Vec::new();
            };
            match job.state {
                State::Running => {}
                State::Stopped(_) => return Vec::new(),
                State::Done(_) => break,
            }
            let Some(&pid) = job.running.first() else {
                break;
            };
            match sys::wait(pid, true) {
                Ok(Some((pid, change))) => self.changed(pid, change),
                Ok(None) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // It was waited for already: the job cannot be followed.
                Err(_) => self.changed(pid, Change::Signaled(sys::SIGKILL)),
            }
        }
        self.jobs.remove(number);
        let started = self.started.remove(&number);
        started.map_or_else(Vec::new, |s| s.ended.into_iter().flatten().collect())
    }

    /// Runs job `number` in the foreground, continued first when
    /// `resume` says so, until it ends or stops, then takes the terminal
    /// back, and reports the job when it stopped.
    fn foreground(&mut self, number: usize, resume: bool) -> Vec<Ended> {
        let group = self.jobs.get(number).map(|j| j.group);
        if let (true, Some(group)) = (self.job_control, group) {
            let _ = sys::set_foreground(group);
        }
        if resume {
            // Processes that ended meanwhile are waited for below.
            let _ = self.signal_job(number, sys::SIGCONT);
        }
        let ended = self.wait_for(number);
        if self.job_control {
            let _ = sys::set_foreground(self.group);
        }
        if let Some(job) = self.jobs.get(number) {
            let line = format!("\n{}\n", self.jobs.report(job));
            self.write(Stream::Error, line.as_bytes());
        }
        ended
    }

    /// Sends `signal` to every process of job `number` still running.
    fn signal_job(&self, number: usize, signal: i32) -> io::Result<()> {
        let Some(job) = self.jobs.get(number) else {
            return Ok(());
        };
        match self.job_control {
            true => sys::send(-job.group, signal),
            false => job
                .running
                .iter()
                .try_for_each(|&pid| sys::send(pid, signal)),
        }
    }

    fn find_job(&self, id: &str) -> Result<usize, String> {
        self.jobs.find(id).map_err(|e| match e {
            Lookup::Invalid => format!("{id}: not a job ID"),
            Lookup::NoSuchJob => format!("{id}: no such job"),
            Lookup::Ambiguous => format!("{id}: more than one job matches"),
        })
    }

    /// Notes the jobs that ended or stopped meanwhile, without waiting.
    fn poll(&mut self) {
        while let Ok(Some((pid, change))) = sys::wait(-1, false) {
            self.changed(pid, change);
        }
    }

    /// The lines that report the jobs that ended since the last report,
    /// which are then forgotten.
    pub fn finished(&mut self) -> Vec<String> {
        self.poll();
        let lines = self
            .jobs
            .all()
            .filter(|j| matches!(j.state, State::Done(_)))
            .map(|j| self.jobs.report(j).to_string())
            .collect();
        for job in self.jobs.remove_done() {
            self.started.remove(&job.number);
        }
        lines
    }
}

impl Folders for Os {
    fn entries(&self, path: &str) -> Option<Vec<(String, bool)>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(path).ok()?.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let folder = match entry.file_type() {
                Ok(t) if t.is_symlink() => entry.path().is_dir(),
                Ok(t) => t.is_dir(),
                Err(_) => false,
            };
            entries.push((name, folder));
        }
        Some(entries)
    }
}

impl System for Os {
    fn current_dir(&self) -> String {
        std::env::current_dir().map_or_else(|_| "/".into(), |p| p.to_string_lossy().into_owned())
    }

    fn set_current_dir(&mut self, path: &str) -> Result<(), String> {
        std::env::set_current_dir(path).map_err(|e| file_error(path, e))
    }

    fn read_file(&self, path: &str) -> Result<String, String> {
        fs::read_to_string(path).map_err(|e| file_error(path, e))
    }

    fn executable(&self, path: &str) -> Option<bool> {
        let metadata = fs::metadata(path).ok()?;
        Some(!metadata.is_dir() && sys::may_run(path))
    }

    fn run(&mut self, run: Run) -> Result<Ran, String> {
        let mut input = None;
        let mut feeder = None;
        if let Input::Bytes(bytes) = run.input {
            let (reader, writer) = pipe()?;
            input = Some(reader);
            feeder = Some(thread::spawn(move || {
                let _ = File::from(writer).write_all(&bytes);
            }));
        }
        let mut output = None;
        let mut pids = Vec::new();
        let count = run.programs.len();
        for (i, program) in run.programs.iter().enumerate() {
            let stdin = input.take();
            let stdout = if i + 1 < count || run.capture {
                let (reader, writer) = pipe()?;
                match i + 1 < count {
                    true => input = Some(reader),
                    false => output = Some(reader),
                }
                Some(writer)
            } else {
                None
            };
            let group = pids.first().copied().unwrap_or(0);
            match self.start(program, stdin, stdout, group) {
                Ok(pid) => pids.push(pid),
                Err(e) => {
                    for &pid in &pids {
                        let _ = sys::send(pid, sys::SIGKILL);
                        let _ = sys::wait(pid, true);
                    }
                    return Err(e);
                }
            }
        }
        let reader = output.map(|fd| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = File::from(fd).read_to_end(&mut bytes);
                bytes
            })
        });
        let number = self.jobs.add(pids[0], pids.clone(), &run.text);
        let ended = vec![None; pids.len()];
        self.started.insert(number, Started { pids, ended });
        if run.background {
            return Ok(Ran::default());
        }
        let ended = self.foreground(number, false);
        // A job that stopped may still read its input and write its
        // output.
        let mut output = Vec::new();
        if !ended.is_empty() {
            if let Some(feeder) = feeder {
                let _ = feeder.join();
            }
            if let Some(reader) = reader {
                output = reader.join().unwrap_or_default();
            }
        }
        Ok(Ran { ended, output })
    }

    fn write(&mut self, stream: Stream, bytes: &[u8]) {
        let _ = match stream {
            Stream::Output => io::stdout()
                .lock()
                .write_all(bytes)
                .and(io::stdout().flush()),
            Stream::Error => io::stderr().lock().write_all(bytes),
        };
    }

    fn jobs(&mut self) -> Vec<String> {
        self.poll();
        let lines = self
            .jobs
            .all()
            .map(|j| self.jobs.report(j).to_string())
            .collect();
        for job in self.jobs.remove_done() {
            self.started.remove(&job.number);
        }
        lines
    }

    fn resume(&mut self, id: &str, foreground: bool) -> Result<Ran, String> {
        self.poll();
        let number = self.find_job(id)?;
        if matches!(self.jobs.get(number).map(|j| j.state), Some(State::Done(_))) {
            if !foreground {
                return Err(format!("{id}: the job has ended"));
            }
            // `fg` of a job that ended meanwhile gives how it ended.
            self.jobs.remove(number);
            let started = self.started.remove(&number);
            return Ok(Ran {
                ended: started.map_or_else(Vec::new, |s| s.ended.into_iter().flatten().collect()),
                output: Vec::new(),
            });
        }
        self.jobs.make_current(number);
        self.jobs.continued(number);
        let ended = match foreground {
            true => self.foreground(number, true),
            false => {
                self.signal_job(number, sys::SIGCONT)
                    .map_err(|e| reason(&e))?;
                Vec::new()
            }
        };
        Ok(Ran {
            ended,
            output: Vec::new(),
        })
    }

    fn kill(&mut self, target: &str, signal: i32) -> Result<(), String> {
        let sent = match target.starts_with('%') {
            true => self.signal_job(self.find_job(target)?, signal),
            false => match target.parse() {
                Ok(pid) => sys::send(pid, signal),
                Err(_) => return Err("not a job or process ID".into()),
            },
        };
        sent.map_err(|e| reason(&e))
    }

    fn exec(&mut self, program: Program) -> String {
        let mut command = Command::new(&program.path);
        if let Some((name, args)) = program.args.split_first() {
            command.arg0(name).args(args);
        }
        command
            .env_clear()
            .envs(program.environment.iter().map(|(k, v)| (k, v)));
        let e = command.exec();
        file_error(&program.path, e)
    }
}
