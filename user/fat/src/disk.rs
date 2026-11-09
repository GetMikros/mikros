//! The device under a volume, with a cache of the sectors that hold its
//! metadata. Writes go through to the device at once.

use crate::{Device, Error};
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

/// The most sectors the cache holds before it starts over.
const CACHED_MAX: usize = 256;

pub(crate) struct Disk<D> {
    pub(crate) device: D,
    sector: u64,
    cache: BTreeMap<u64, Vec<u8>>,
}

impl<D: Device> Disk<D> {
    /// A disk whose I/O is in sectors of `sector` bytes.
    pub(crate) fn new(device: D, sector: u32) -> Disk<D> {
        Disk {
            device,
            sector: u64::from(sector),
            cache: BTreeMap::new(),
        }
    }

    /// The cached sector starting at `at`, read when it is not cached.
    fn cached(&mut self, at: u64) -> Result<&mut Vec<u8>, Error> {
        if !self.cache.contains_key(&at) {
            if self.cache.len() >= CACHED_MAX {
                self.cache.clear();
            }
            let mut sector = vec![0; self.sector as usize];
            self.device.read(at, &mut sector)?;
            self.cache.insert(at, sector);
        }
        Ok(self.cache.get_mut(&at).expect("inserted"))
    }

    /// Reads `buf.len()` bytes at `at`, through the cache.
    pub(crate) fn read(&mut self, at: u64, buf: &mut [u8]) -> Result<(), Error> {
        let mut done = 0;
        while done < buf.len() {
            let pos = at + done as u64;
            let start = pos - pos % self.sector;
            let within = (pos - start) as usize;
            let n = (self.sector as usize - within).min(buf.len() - done);
            let sector = self.cached(start)?;
            buf[done..done + n].copy_from_slice(&sector[within..within + n]);
            done += n;
        }
        Ok(())
    }

    /// Writes `data` at `at`, through the cache and on to the device.
    pub(crate) fn write(&mut self, at: u64, data: &[u8]) -> Result<(), Error> {
        let mut done = 0;
        while done < data.len() {
            let pos = at + done as u64;
            let start = pos - pos % self.sector;
            let within = (pos - start) as usize;
            let n = (self.sector as usize - within).min(data.len() - done);
            let sector = self.cached(start)?;
            sector[within..within + n].copy_from_slice(&data[done..done + n]);
            let copy = sector.clone();
            self.device.write(start, &copy)?;
            done += n;
        }
        Ok(())
    }

    /// Reads whole sectors at `at` straight from the device, for file
    /// data, taking cached sectors into account.
    pub(crate) fn read_direct(&mut self, at: u64, buf: &mut [u8]) -> Result<(), Error> {
        self.device.read(at, buf)?;
        let end = at + buf.len() as u64;
        for (&start, sector) in self.cache.range(at..end) {
            let from = (start - at) as usize;
            buf[from..from + sector.len()].copy_from_slice(sector);
        }
        Ok(())
    }

    /// Writes whole sectors at `at` straight to the device, for file
    /// data, dropping the cached sectors it covers.
    pub(crate) fn write_direct(&mut self, at: u64, data: &[u8]) -> Result<(), Error> {
        let end = at + data.len() as u64;
        let stale: Vec<u64> = self.cache.range(at..end).map(|(&k, _)| k).collect();
        for key in stale {
            self.cache.remove(&key);
        }
        self.device.write(at, data)
    }

    /// The sector size of the disk's I/O.
    pub(crate) fn sector(&self) -> u64 {
        self.sector
    }
}
