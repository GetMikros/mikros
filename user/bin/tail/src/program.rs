//! tail on Mikros: it reads each file, or standard input, through the
//! standard library, from the end where it can seek, and follows the
//! files with `-f` and `-F`. A followed file is watched through its node
//! connection (MIKSPEC-0049.R15) for changes, with one watcher channel
//! for all of them; where the file system cannot watch, or `-F` or
//! `--pid` needs a look every second, tail reads again every second.

use crate::tailing::{self, COMMAND, Failed, Part};
use mikros_cli::{FAILURE, INVALID, Paint, Run, SUCCESS, complain, start};
use mikros_command::node;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::{NodeClient, WatchMask, Watcher, WatcherServer};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Seek, SeekFrom, StdoutLock, Write};
use std::os::unix::fs::MetadataExt;
use std::process::ExitCode;
use std::time::Duration;

const SECOND: i64 = 1_000_000_000;
const FOREVER: i64 = i64::MAX;
const SAME_RIGHTS: u32 = 1 << 31;

/// A file that tail follows.
struct Followed {
    name: String,
    /// The open file, while there is one to follow.
    file: Option<File>,
    /// How far it has been written.
    position: u64,
    /// Its device and inode, to find it replaced under its name.
    identity: Option<(u64, u64)>,
    /// The node connection that holds its watch, while it has one.
    watch: Option<NodeClient>,
}

/// What the run asks and where tail writes.
struct Tail<'a> {
    run: &'a Run,
    paint: Paint,
    titled: bool,
    /// Which file was written last, for the titles.
    last: Option<usize>,
    out: BufWriter<StdoutLock<'static>>,
    failed: bool,
    /// Follows names rather than open files, with `-F`.
    by_name: bool,
    /// The channel that the watches send their events to.
    watcher: Option<(ClientEnd<Watcher>, WatcherServer)>,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let part = match Part::of(&run) {
        Ok(part) => part,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let pid = match run.value("--pid").map(str::parse::<u32>) {
        None => None,
        Some(Ok(pid)) => Some(pid),
        Some(Err(_)) => {
            let given = run.value("--pid").unwrap_or_default();
            complain(&COMMAND, &run, &format!("invalid process ID: {given}"));
            return ExitCode::from(INVALID);
        }
    };
    let operands: Vec<String> = match run.operands.is_empty() {
        true => vec!["-".into()],
        false => run.operands.clone(),
    };
    let following = run.has("-f") || run.has("-F");
    let mut tail = Tail {
        run: &run,
        paint: Paint::stdout(&run),
        titled: tailing::titled(&run, operands.len()),
        last: None,
        out: BufWriter::new(io::stdout().lock()),
        failed: false,
        by_name: run.has("-F"),
        watcher: None,
    };
    let mut followed = Vec::new();
    for (index, operand) in operands.iter().enumerate() {
        match tail.first(operand, index, part) {
            Some(f) if following => followed.push(f),
            _ => {}
        }
    }
    // The titles count the files followed from here on: the last one
    // written needs none for what is added to it next.
    if tail.last.is_some() {
        let last_followed = followed
            .last()
            .is_some_and(|f| Some(&f.name) == operands.last());
        tail.last = Some(match last_followed {
            true => followed.len() - 1,
            false => usize::MAX,
        });
    }
    if let Err(e) = tail.out.flush() {
        return ExitCode::from(exit_code(&run, e));
    }
    if following && !followed.is_empty() {
        tail.follow(&mut followed, pid);
    }
    match tail.failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

impl Tail<'_> {
    fn fail(&mut self, message: &str) {
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }

    /// Writes `part` of `operand`, and returns it to follow when it is a
    /// file that can be: a regular one, or with `-F` one that is not
    /// there yet. Standard input is not followed.
    fn first(&mut self, operand: &str, index: usize, part: Part) -> Option<Followed> {
        if operand == "-" {
            self.title(index, operand);
            let copied = tailing::copy(part, &mut io::stdin().lock(), &mut self.out);
            self.copied(operand, copied);
            return None;
        }
        let missing = Followed {
            name: operand.into(),
            file: None,
            position: 0,
            identity: None,
            watch: None,
        };
        let mut file = match File::open(operand) {
            Ok(file) => file,
            Err(e) => {
                self.fail(&format!("{operand}: {e}"));
                return self.by_name.then_some(missing);
            }
        };
        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(e) => {
                self.fail(&format!("{operand}: {e}"));
                return None;
            }
        };
        self.title(index, operand);
        if !metadata.is_file() {
            let copied = tailing::copy(part, &mut BufReader::new(file), &mut self.out);
            self.copied(operand, copied);
            return None;
        }
        let from = match tailing::start(part, &mut file, metadata.len()) {
            Ok(from) => from,
            Err(e) => {
                self.fail(&format!("{operand}: {e}"));
                return None;
            }
        };
        let rest = match part {
            Part::FromLine(n) => Part::FromLine(n),
            _ => Part::FromByte(1),
        };
        let copied = match file.seek(SeekFrom::Start(from)) {
            Ok(_) => tailing::copy(rest, &mut BufReader::new(&mut file), &mut self.out),
            Err(e) => Err(Failed::Read(e)),
        };
        if !self.copied(operand, copied) {
            return None;
        }
        let position = file.stream_position().unwrap_or(metadata.len());
        Some(Followed {
            name: operand.into(),
            file: Some(file),
            position,
            identity: Some((metadata.dev(), metadata.ino())),
            watch: None,
        })
    }

    /// The title above the file at `index`, when files have titles and
    /// another was written last.
    fn title(&mut self, index: usize, name: &str) {
        if !self.titled || self.last == Some(index) {
            return;
        }
        let title = tailing::title(name, self.last.is_none(), self.paint);
        self.last = Some(index);
        if let Err(e) = self.out.write_all(title.as_bytes()) {
            std::process::exit(i32::from(exit_code(self.run, e)));
        }
    }

    /// Whether a copy of `name` went well; a failed write ends tail.
    fn copied(&mut self, name: &str, copied: Result<(), Failed>) -> bool {
        match copied {
            Ok(()) => true,
            Err(Failed::Read(e)) => {
                self.fail(&format!("{name}: {e}"));
                false
            }
            Err(Failed::Write(e)) => std::process::exit(i32::from(exit_code(self.run, e))),
        }
    }

    /// Follows `files` until process `pid` has ended, or for ever.
    fn follow(&mut self, files: &mut [Followed], pid: Option<u32>) {
        if let Ok((client, server)) = endpoints::<Watcher>() {
            self.watcher = Some((client, WatcherServer::new(server)));
        }
        for (i, f) in files.iter_mut().enumerate() {
            f.watch = self.watch(f.file.as_ref(), i);
        }
        loop {
            let looks = self.by_name || pid.is_some() || files.iter().any(|f| f.watch.is_none());
            self.wait(looks);
            let ended = pid.is_some_and(|pid| std::os::mikros::process::info(pid).is_err());
            for i in 0..files.len() {
                self.check(files, i);
            }
            if let Err(e) = self.out.flush() {
                std::process::exit(i32::from(exit_code(self.run, e)));
            }
            if ended || (!self.by_name && files.iter().all(|f| f.file.is_none())) {
                return;
            }
        }
    }

    /// A watch of `file` that sends its changes with the cookie `index`,
    /// or none where its file system cannot watch it.
    fn watch(&self, file: Option<&File>, index: usize) -> Option<NodeClient> {
        let (client, _) = self.watcher.as_ref()?;
        let node = node::node(file?).ok()?;
        let end = mikros_rt::objects::duplicate(client.handle().raw(), SAME_RIGHTS).ok()?;
        let mask = WatchMask::MODIFIED.0 | WatchMask::ATTRIBUTES.0 | WatchMask::SELF.0;
        match node.watch(WatchMask(mask), index as u64, ClientEnd::new(end), FOREVER) {
            Ok(Ok(_)) => Some(node),
            _ => None,
        }
    }

    /// Waits for a change of a watched file, for at most a second when
    /// `looks` asks for a look every second.
    fn wait(&mut self, looks: bool) {
        let Some((_, server)) = &self.watcher else {
            std::thread::sleep(Duration::from_secs(1));
            return;
        };
        let deadline = match looks {
            true => mikros_rt::objects::now().saturating_add(SECOND),
            false => FOREVER,
        };
        match server.next(deadline) {
            Ok(_) => while let Ok(Some(_)) = server.try_next() {},
            // An error before the deadline is a broken channel: from here
            // on, every file is read again every second.
            Err(_) if deadline == FOREVER || mikros_rt::objects::now() < deadline => {
                self.watcher = None;
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(_) => {}
        }
    }

    /// Writes what was added to the file at `index`, from its start when
    /// it was truncated, and with `-F` opens its name again when it was
    /// replaced or has appeared.
    fn check(&mut self, files: &mut [Followed], index: usize) {
        if self.by_name {
            self.check_name(files, index);
        }
        let f = &mut files[index];
        let Some(file) = f.file.as_mut() else {
            return;
        };
        let size = match file.metadata() {
            Ok(m) => m.len(),
            Err(e) => {
                let message = format!("{}: {e}", f.name);
                f.file = None;
                f.watch = None;
                self.fail(&message);
                return;
            }
        };
        if size < f.position {
            complain(&COMMAND, self.run, &format!("{}: file truncated", f.name));
            f.position = 0;
        }
        if size == f.position {
            return;
        }
        let name = f.name.clone();
        let copied = match file.seek(SeekFrom::Start(f.position)) {
            Ok(_) => {
                self.title(index, &name);
                tailing::copy_all(&mut BufReader::new(&mut *file), &mut self.out)
            }
            Err(e) => Err(Failed::Read(e)),
        };
        if self.copied(&name, copied) {
            f.position = file.stream_position().unwrap_or(size);
        } else {
            f.file = None;
            f.watch = None;
        }
    }

    /// With `-F`, follows the file now under the name at `index`.
    fn check_name(&mut self, files: &mut [Followed], index: usize) {
        let f = &mut files[index];
        let identity = match fs::metadata(&f.name) {
            Ok(m) => (m.dev(), m.ino()),
            Err(e) => {
                if f.file.take().is_some() {
                    f.watch = None;
                    let message = format!("{} has become inaccessible: {e}", f.name);
                    complain(&COMMAND, self.run, &message);
                }
                return;
            }
        };
        if f.file.is_some() && f.identity == Some(identity) {
            return;
        }
        let Ok(file) = File::open(&f.name) else {
            return;
        };
        let news = match f.identity {
            Some(_) => "has been replaced",
            None => "has appeared",
        };
        complain(
            &COMMAND,
            self.run,
            &format!("{} {news}; following new file", f.name),
        );
        f.watch = self.watch(Some(&file), index);
        f.file = Some(file);
        f.identity = Some(identity);
        f.position = 0;
    }
}

/// The exit status after writing to standard output failed, telling why
/// unless the reader closed the pipe.
fn exit_code(run: &Run, e: io::Error) -> u8 {
    if e.kind() != io::ErrorKind::BrokenPipe {
        complain(&COMMAND, run, &format!("standard output: {e}"));
    }
    FAILURE
}
