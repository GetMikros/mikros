//! What tee takes, and how it copies its input to every output, going on
//! with the others when one fails (MIKSPEC-0098.R49).

use mikros_cli::{Kind, Operand, Opt, Spec};
use std::io::{self, Read, Write};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "file",
        Kind::File,
        "the files to write besides standard output",
    )],
    options: &[
        Opt::both("append", 'a', "add to the files rather than replace them"),
        Opt::both("ignore-interrupts", 'i', "ignore the interrupt signal"),
        Opt::short(
            'p',
            "go on when a pipe closes, and name only errors of other outputs",
        ),
    ],
    ..Spec::new(
        "tee",
        "Copies standard input to standard output and to each file given.\n\
         A file named - is a file of that name.",
    )
};

/// One output: its name in messages and its writer, until writing to
/// it failed.
pub struct Output<W> {
    pub name: String,
    pub writer: Option<W>,
}

/// How a copy ended.
#[derive(Debug, Default)]
pub struct Copied {
    /// The outputs that failed, by name, with their errors.
    pub failed: Vec<(String, io::Error)>,
    /// Why reading failed, if it did.
    pub read: Option<io::Error>,
    /// Whether a pipe closed without `-p`, which ends tee as a closed
    /// pipe ends a writer that does not ignore it.
    pub closed: bool,
}

/// Copies `input` to `outputs` until it ends, or until no output is
/// left. With `pipes`, from `-p`, an output whose pipe closed is left
/// out quietly; without, it ends the copy.
pub fn copy<W: Write>(input: &mut impl Read, outputs: &mut [Output<W>], pipes: bool) -> Copied {
    let mut copied = Copied::default();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        if outputs.iter().all(|o| o.writer.is_none()) {
            return copied;
        }
        let n = match input.read(&mut buffer) {
            Ok(0) => return copied,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                copied.read = Some(e);
                return copied;
            }
        };
        for output in outputs.iter_mut() {
            let Some(writer) = output.writer.as_mut() else {
                continue;
            };
            let Err(e) = writer.write_all(&buffer[..n]).and_then(|()| writer.flush()) else {
                continue;
            };
            output.writer = None;
            match (e.kind() == io::ErrorKind::BrokenPipe, pipes) {
                (true, true) => {}
                (true, false) => {
                    copied.closed = true;
                    return copied;
                }
                (false, _) => copied.failed.push((output.name.clone(), e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer that fails as `kind` after taking `room` bytes.
    struct Failing {
        room: usize,
        kind: io::ErrorKind,
        taken: Vec<u8>,
    }

    impl Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.room == 0 {
                return Err(io::Error::from(self.kind));
            }
            let n = bytes.len().min(self.room);
            self.room -= n;
            self.taken.extend_from_slice(&bytes[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn output(name: &str, room: usize, kind: io::ErrorKind) -> Output<Failing> {
        Output {
            name: name.into(),
            writer: Some(Failing {
                room,
                kind,
                taken: Vec::new(),
            }),
        }
    }

    fn taken(outputs: &[Output<Failing>]) -> Vec<Option<usize>> {
        outputs
            .iter()
            .map(|o| o.writer.as_ref().map(|w| w.taken.len()))
            .collect()
    }

    /// Input that comes in pieces of `piece` bytes.
    struct Pieces(Vec<u8>, usize);

    impl Read for Pieces {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let n = self.0.len().min(self.1).min(buffer.len());
            buffer[..n].copy_from_slice(&self.0[..n]);
            self.0.drain(..n);
            Ok(n)
        }
    }

    #[test]
    fn copies_to_every_output() {
        let mut outputs = vec![
            Output {
                name: "a".into(),
                writer: Some(Vec::new()),
            },
            Output {
                name: "b".into(),
                writer: Some(Vec::new()),
            },
        ];
        let copied = copy(&mut &b"some text\n"[..], &mut outputs, false);
        assert!(copied.failed.is_empty() && copied.read.is_none() && !copied.closed);
        for o in &outputs {
            assert_eq!(o.writer.as_deref(), Some(&b"some text\n"[..]));
        }
    }

    #[test]
    fn goes_on_after_an_output_fails() {
        let full = io::ErrorKind::StorageFull;
        let mut outputs = vec![output("a", 100, full), output("b", 3, full)];
        let copied = copy(&mut Pieces(vec![7; 20], 5), &mut outputs, false);
        assert_eq!(copied.failed.len(), 1);
        assert_eq!(copied.failed[0].0, "b");
        assert_eq!(taken(&outputs), [Some(20), None]);
    }

    #[test]
    fn ends_at_a_closed_pipe_unless_p() {
        let pipe = io::ErrorKind::BrokenPipe;
        let full = io::ErrorKind::StorageFull;
        let mut outputs = vec![output("-", 5, pipe), output("f", 100, full)];
        let copied = copy(&mut Pieces(vec![1; 20], 5), &mut outputs, false);
        assert!(copied.closed && copied.failed.is_empty());
        assert_eq!(taken(&outputs), [None, Some(5)]);
        let mut outputs = vec![output("-", 5, pipe), output("f", 100, full)];
        let copied = copy(&mut Pieces(vec![1; 20], 5), &mut outputs, true);
        assert!(!copied.closed && copied.failed.is_empty());
        assert_eq!(taken(&outputs), [None, Some(20)]);
    }

    #[test]
    fn stops_when_no_output_is_left() {
        let mut outputs = vec![output("a", 0, io::ErrorKind::BrokenPipe)];
        let mut input = Pieces(vec![1; 20], 5);
        let copied = copy(&mut input, &mut outputs, true);
        assert!(!copied.closed);
        assert_eq!(input.0.len(), 15);
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/tee.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
