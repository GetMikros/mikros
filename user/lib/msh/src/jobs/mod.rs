//! The jobs of an interactive shell: each pipeline it started, by its
//! job ID, with its process group, state and command text, the current
//! and the previous job, the job IDs that `fg`, `bg`, `kill` and `jobs`
//! take, and the lines that report a job.

use std::fmt;

/// What a job is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    /// Stopped by a signal.
    Stopped(i32),
    /// Every process ended; the last one's exit status or signal.
    Done(Ended),
}

/// How the last process of a job ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Exited(i32),
    Signaled(i32),
}

/// A pipeline the shell started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// Its job number, from 1.
    pub number: usize,
    /// Its process group, which is its first process's ID.
    pub group: i32,
    /// Its processes that have not ended, by process ID.
    pub running: Vec<i32>,
    pub state: State,
    /// How its last process ended, once it did.
    pub last: Option<Ended>,
    /// The command as it was typed.
    pub command: String,
}

/// Why a job ID names no job.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// It is not of the form `%…`.
    Invalid,
    NoSuchJob,
    /// `%string` or `%?string` names more than one job.
    Ambiguous,
}

/// The jobs, with the current and the previous one.
#[derive(Default)]
pub struct Jobs {
    jobs: Vec<Job>,
    /// Job numbers, most recent first: the current job, then the
    /// previous one.
    recent: Vec<usize>,
}

impl Jobs {
    pub fn new() -> Jobs {
        Jobs::default()
    }

    /// Adds a running job of `command` in process group `group` with
    /// `pids`, as the current job, and returns its number: one more than
    /// the highest in use, from 1.
    pub fn add(&mut self, group: i32, pids: Vec<i32>, command: &str) -> usize {
        let number = self.jobs.iter().map(|j| j.number).max().unwrap_or(0) + 1;
        self.jobs.push(Job {
            number,
            group,
            running: pids,
            state: State::Running,
            last: None,
            command: command.into(),
        });
        self.recent.insert(0, number);
        number
    }

    pub fn get(&self, number: usize) -> Option<&Job> {
        self.jobs.iter().find(|j| j.number == number)
    }

    fn get_mut(&mut self, number: usize) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.number == number)
    }

    /// The jobs, by number.
    pub fn all(&self) -> impl Iterator<Item = &Job> {
        let mut jobs: Vec<&Job> = self.jobs.iter().collect();
        jobs.sort_by_key(|j| j.number);
        jobs.into_iter()
    }

    /// The current job, `%+`.
    pub fn current(&self) -> Option<usize> {
        self.recent.first().copied()
    }

    /// The previous job, `%-`.
    pub fn previous(&self) -> Option<usize> {
        self.recent.get(1).copied()
    }

    /// Makes job `number` the current one, as `fg` and `bg` do.
    pub fn make_current(&mut self, number: usize) {
        if self.get(number).is_some() {
            self.recent.retain(|&n| n != number);
            self.recent.insert(0, number);
        }
    }

    /// The job that `id` names: `%%`, `%+` or `%` the current job, `%-`
    /// the previous one, `%n` job n, `%string` the job whose command
    /// starts with `string`, and `%?string` the job whose command
    /// contains it.
    pub fn find(&self, id: &str) -> Result<usize, Lookup> {
        let spec = id.strip_prefix('%').ok_or(Lookup::Invalid)?;
        let found = match spec {
            "" | "%" | "+" => self.current(),
            "-" => self.previous(),
            _ if spec.bytes().all(|b| b.is_ascii_digit()) => {
                let n: usize = spec.parse().map_err(|_| Lookup::NoSuchJob)?;
                self.get(n).map(|j| j.number)
            }
            _ => {
                let matches: Vec<usize> = match spec.strip_prefix('?') {
                    Some(part) => self
                        .jobs
                        .iter()
                        .filter(|j| j.command.contains(part))
                        .map(|j| j.number)
                        .collect(),
                    None => self
                        .jobs
                        .iter()
                        .filter(|j| j.command.starts_with(spec))
                        .map(|j| j.number)
                        .collect(),
                };
                match matches.as_slice() {
                    [one] => Some(*one),
                    [] => None,
                    _ => return Err(Lookup::Ambiguous),
                }
            }
        };
        found.ok_or(Lookup::NoSuchJob)
    }

    /// The job of process `pid`, among those that have not ended.
    pub fn of_process(&self, pid: i32) -> Option<usize> {
        self.jobs
            .iter()
            .find(|j| j.running.contains(&pid))
            .map(|j| j.number)
    }

    /// Records that process `pid` stopped with `signal`: its job is
    /// stopped and becomes the current one.
    pub fn stopped(&mut self, pid: i32, signal: i32) -> Option<usize> {
        let number = self.of_process(pid)?;
        self.get_mut(number)?.state = State::Stopped(signal);
        self.make_current(number);
        Some(number)
    }

    /// Records that job `number` runs again, after `fg` or `bg`.
    pub fn continued(&mut self, number: usize) {
        if let Some(job) = self.get_mut(number) {
            job.state = State::Running;
        }
    }

    /// Records that process `pid` ended as `ended`, which is the job's
    /// status when `last` says it is the job's last process, the one
    /// whose output the pipeline gives. The job is done once all its
    /// processes ended.
    pub fn ended(&mut self, pid: i32, ended: Ended, last: bool) -> Option<usize> {
        let number = self.of_process(pid)?;
        let job = self.get_mut(number)?;
        job.running.retain(|&p| p != pid);
        if last {
            job.last = Some(ended);
        }
        if job.running.is_empty() {
            job.state = State::Done(job.last.unwrap_or(ended));
        }
        Some(number)
    }

    /// Removes job `number`, as for a pipeline that ended in the
    /// foreground, which is not reported.
    pub fn remove(&mut self, number: usize) -> Option<Job> {
        let index = self.jobs.iter().position(|j| j.number == number)?;
        self.recent.retain(|&n| n != number);
        Some(self.jobs.remove(index))
    }

    /// Removes the jobs that are done, after they were reported, and
    /// returns them.
    pub fn remove_done(&mut self) -> Vec<Job> {
        let (done, kept): (Vec<Job>, Vec<Job>) = self
            .jobs
            .drain(..)
            .partition(|j| matches!(j.state, State::Done(_)));
        self.jobs = kept;
        self.recent.retain(|n| done.iter().all(|j| j.number != *n));
        done
    }

    /// The line that reports `job` as `jobs` lists it.
    pub fn report<'a>(&'a self, job: &'a Job) -> Report<'a> {
        let mark = if Some(job.number) == self.current() {
            '+'
        } else if Some(job.number) == self.previous() {
            '-'
        } else {
            ' '
        };
        Report { job, mark }
    }
}

/// A job's line in the format of POSIX `jobs`: `[1]+  Running   cmd`.
pub struct Report<'a> {
    job: &'a Job,
    mark: char,
}

impl fmt::Display for Report<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let state = match self.job.state {
            State::Running => "Running".to_owned(),
            State::Stopped(_) => "Stopped".to_owned(),
            State::Done(Ended::Exited(0)) => "Done".to_owned(),
            State::Done(Ended::Exited(code)) => format!("Done({code})"),
            State::Done(Ended::Signaled(signal)) => format!("Killed({signal})"),
        };
        write!(
            f,
            "[{}]{}  {:<24}{}",
            self.job.number, self.mark, state, self.job.command
        )
    }
}

#[cfg(test)]
mod tests;
