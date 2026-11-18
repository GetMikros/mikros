//! The FAT and exFAT servers' volumes (MIKSPEC-0017.R26), mounted
//! read-only from the bytes of a hostile disk: the first byte chooses
//! the format, the rest is the partition. Every directory is listed and
//! every file read, within bounds, as a client could ask.

#![no_main]

use libfuzzer_sys::fuzz_target;
use user_fuzz::Image;

/// How many nodes and bytes one input may make the walk visit, so that
/// a volume whose directories loop still ends.
const NODES_MAX: usize = 256;
const READ_MAX: usize = 1 << 16;

fn fat(bytes: Vec<u8>) {
    let size = bytes.len() as u64 & !511;
    let Ok(mut volume) = mikros_fat::Volume::mount(Image(bytes), size, false) else {
        return;
    };
    let _ = volume.label();
    let _ = volume.free_clusters();
    let root = volume.root();
    let mut dirs = vec![root];
    let mut visited = 0;
    let mut buf = vec![0u8; 4096];
    while let Some(dir) = dirs.pop() {
        let Ok((nodes, _)) = volume.list(dir, 0, 64) else {
            continue;
        };
        for mut node in nodes {
            visited += 1;
            if visited > NODES_MAX {
                return;
            }
            match node.as_dir(root) {
                Some(child) => dirs.push(child),
                None => {
                    let mut offset = 0;
                    while offset < READ_MAX as u64 {
                        match volume.read(&mut node, offset, &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => offset += n as u64,
                        }
                    }
                }
            }
        }
    }
}

fn exfat(bytes: Vec<u8>) {
    let size = bytes.len() as u64 & !511;
    let Ok(mut volume) = mikros_exfat::Volume::mount(Image(bytes), size, false) else {
        return;
    };
    let _ = volume.label();
    let _ = volume.free_clusters();
    let mut dirs = vec![volume.root()];
    let mut visited = 0;
    let mut buf = vec![0u8; 4096];
    while let Some(dir) = dirs.pop() {
        let Ok((nodes, _)) = volume.list(dir, 0, 64) else {
            continue;
        };
        for mut node in nodes {
            visited += 1;
            if visited > NODES_MAX {
                return;
            }
            match node.as_dir() {
                Some(child) => dirs.push(child),
                None => {
                    let mut offset = 0;
                    while offset < READ_MAX as u64 {
                        match volume.read(&mut node, offset, &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => offset += n as u64,
                        }
                    }
                }
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&format, rest)) = data.split_first() else {
        return;
    };
    match format & 1 {
        0 => fat(rest.to_vec()),
        _ => exfat(rest.to_vec()),
    }
});
