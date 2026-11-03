//! Exiting boot services.

/// The most times exiting boot services is tried.
pub const EXIT_ATTEMPTS: usize = 8;

/// The final firmware memory map, as the number of bytes written and the
/// descriptor size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalMap {
    pub size: usize,
    pub descriptor_size: usize,
}

/// The two boot services used to exit boot services.
pub trait BootServices {
    type Error;

    /// Reads the firmware memory map into the buffer allocated for it
    /// beforehand, giving the map and its key.
    fn memory_map(&mut self) -> Result<(FinalMap, usize), Self::Error>;

    /// Exits boot services with the map key `key`, giving whether that
    /// succeeded.
    fn exit(&mut self, key: usize) -> bool;
}

/// Reads the memory map and exits boot services with its key, reading the
/// map again after each failed exit, at most `EXIT_ATTEMPTS` times. Gives
/// the map whose key was accepted. Gives `Err` with an error only when no
/// exit has been tried, so that boot services can still report it; after a
/// failed exit nothing but these two services is called.
pub fn exit<B: BootServices>(services: &mut B) -> Result<FinalMap, Option<B::Error>> {
    for attempt in 0..EXIT_ATTEMPTS {
        let (map, key) = match services.memory_map() {
            Ok(read) => read,
            Err(e) if attempt == 0 => return Err(Some(e)),
            Err(_) => return Err(None),
        };
        if services.exit(key) {
            return Ok(map);
        }
    }
    Err(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Map,
        Exit(usize),
    }

    /// Firmware whose memory map changes with every read, and whose exit
    /// fails `stale` times before it succeeds; reads after `map_fails_at`
    /// reads fail.
    struct Fake {
        calls: Vec<Call>,
        stale: usize,
        map_fails_at: Option<usize>,
        exited: bool,
    }

    impl Fake {
        fn new(stale: usize) -> Fake {
            Fake {
                calls: Vec::new(),
                stale,
                map_fails_at: None,
                exited: false,
            }
        }

        fn reads(&self) -> usize {
            self.calls.iter().filter(|c| **c == Call::Map).count()
        }
    }

    impl BootServices for Fake {
        type Error = &'static str;

        fn memory_map(&mut self) -> Result<(FinalMap, usize), &'static str> {
            assert!(!self.exited, "called after the exit");
            let n = self.reads();
            self.calls.push(Call::Map);
            if self.map_fails_at == Some(n) {
                return Err("buffer too small");
            }
            let map = FinalMap {
                size: 48 * (n + 1),
                descriptor_size: 48,
            };
            Ok((map, 100 + n))
        }

        fn exit(&mut self, key: usize) -> bool {
            assert!(!self.exited, "called after the exit");
            self.calls.push(Call::Exit(key));
            // Only the key of the latest read is current.
            if self.stale > 0 || key != 100 + self.reads() - 1 {
                self.stale = self.stale.saturating_sub(1);
                return false;
            }
            self.exited = true;
            true
        }
    }

    #[test]
    fn exits_with_the_first_map() {
        let mut fw = Fake::new(0);
        assert_eq!(
            exit(&mut fw),
            Ok(FinalMap {
                size: 48,
                descriptor_size: 48
            })
        );
        assert_eq!(fw.calls, [Call::Map, Call::Exit(100)]);
    }

    #[test]
    fn reads_the_map_again_after_a_failed_exit() {
        let mut fw = Fake::new(2);
        let map = exit(&mut fw).unwrap();
        assert_eq!(map.size, 48 * 3, "the map whose key was accepted");
        assert_eq!(
            fw.calls,
            [
                Call::Map,
                Call::Exit(100),
                Call::Map,
                Call::Exit(101),
                Call::Map,
                Call::Exit(102)
            ]
        );
        let mut fw = Fake::new(EXIT_ATTEMPTS - 1);
        assert!(exit(&mut fw).is_ok());
    }

    #[test]
    fn gives_up_after_eight_failed_exits() {
        let mut fw = Fake::new(usize::MAX);
        assert_eq!(exit(&mut fw), Err(None));
        assert_eq!(fw.calls.len(), 2 * 8);
        assert_eq!(fw.calls.last(), Some(&Call::Exit(107)));
    }

    #[test]
    fn reports_a_failed_read_only_before_any_exit() {
        let mut fw = Fake::new(0);
        fw.map_fails_at = Some(0);
        assert_eq!(exit(&mut fw), Err(Some("buffer too small")));
        assert_eq!(fw.calls, [Call::Map]);
        let mut fw = Fake::new(1);
        fw.map_fails_at = Some(1);
        assert_eq!(exit(&mut fw), Err(None));
        assert_eq!(fw.calls, [Call::Map, Call::Exit(100), Call::Map]);
    }
}
