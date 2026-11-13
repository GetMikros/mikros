//! NVMe commands for the NVMe driver: the commands it submits, the
//! submission queue entries they make, and its Read and Write commands.
//!
//! Nothing here touches hardware, so that the host tests can check it.

#[cfg(test)]
mod tests;

/// The opcodes of the NVM command set's Write and Read.
pub const WRITE: u8 = 0x01;
pub const READ: u8 = 0x02;
/// Write's Force Unit Access bit, in command dword 12.
pub const FUA: u32 = 1 << 30;

/// A command, without its command ID, which the queue assigns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Command {
    pub opcode: u8,
    pub nsid: u32,
    pub prp1: u64,
    pub prp2: u64,
    /// Command dwords 10 to 15.
    pub cdw: [u32; 6],
}

impl Command {
    /// A Read, or a Write when `write`, of the `blocks` blocks, at least
    /// one, from `block` of the namespace `nsid`, with the data where
    /// the PRP entries `prp1` and `prp2` say, and with Force Unit Access
    /// for a write when `fua`.
    pub fn read_write(
        write: bool,
        nsid: u32,
        block: u64,
        blocks: u32,
        prp1: u64,
        prp2: u64,
        fua: bool,
    ) -> Command {
        let mut cdw12 = blocks - 1;
        if write && fua {
            cdw12 |= FUA;
        }
        Command {
            opcode: if write { WRITE } else { READ },
            nsid,
            prp1,
            prp2,
            cdw: [block as u32, (block >> 32) as u32, cdw12, 0, 0, 0],
        }
    }

    /// The dwords of the 64-byte submission queue entry of the command,
    /// with the command ID `cid`.
    pub fn entry(&self, cid: u16) -> [u32; 16] {
        let mut entry = [0; 16];
        entry[0] = u32::from(self.opcode) | u32::from(cid) << 16;
        entry[1] = self.nsid;
        entry[6] = self.prp1 as u32;
        entry[7] = (self.prp1 >> 32) as u32;
        entry[8] = self.prp2 as u32;
        entry[9] = (self.prp2 >> 32) as u32;
        entry[10..].copy_from_slice(&self.cdw);
        entry
    }
}
