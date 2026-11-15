//! The arguments of the firmware variable calls, and the statuses that
//! firmware's answers give.

use crate::status::Error;

/// The vendor GUID of Mikros's variables, as UEFI stores it.
pub const VENDOR: [u8; 16] = [
    0xbd, 0xe1, 0x63, 0x06, 0xce, 0x86, 0x8b, 0x4e, 0xa0, 0x25, 0x50, 0x0d, 0x76, 0xc6, 0x0a, 0x1a,
];
/// The longest variable name, in characters.
pub const NAME_MAX: usize = 64;
/// The most bytes `firmware_variable_set` writes.
pub const SET_MAX: u64 = 1024;
/// The most bytes `firmware_variable_get` reads at once.
pub const GET_MAX: usize = 64 * 1024;
/// The attributes a variable may be set with: non-volatile, boot service
/// and runtime access, or the last two.
pub const NON_VOLATILE: u32 = 7;
pub const VOLATILE: u32 = 6;

/// UEFI's statuses, with the error bit.
const ERROR: usize = 1 << (usize::BITS - 1);
pub const EFI_BUFFER_TOO_SMALL: usize = ERROR | 5;
const EFI_OUT_OF_RESOURCES: usize = ERROR | 9;
const EFI_WRITE_PROTECTED: usize = ERROR | 8;
const EFI_NOT_FOUND: usize = ERROR | 14;
const EFI_SECURITY_VIOLATION: usize = ERROR | 26;

/// The UCS-2 form of the variable name `name`, with a terminating null
/// character, when it has 1 to `NAME_MAX` ASCII letters and digits.
pub fn name(name: &[u8]) -> Result<[u16; NAME_MAX + 1], Error> {
    if name.is_empty() || name.len() > NAME_MAX || !name.iter().all(u8::is_ascii_alphanumeric) {
        return Err(Error::InvalidArgs);
    }
    let mut ucs2 = [0; NAME_MAX + 1];
    for (unit, &b) in ucs2.iter_mut().zip(name) {
        *unit = u16::from(b);
    }
    Ok(ucs2)
}

/// Checks the attributes and size that a variable is set with.
pub fn check_set(attributes: u32, size: u64) -> Result<(), Error> {
    if (attributes != NON_VOLATILE && attributes != VOLATILE) || size > SET_MAX {
        return Err(Error::InvalidArgs);
    }
    Ok(())
}

/// The result of a call that returned `status`. Warnings count as
/// success.
pub fn result(status: usize) -> Result<(), Error> {
    match status {
        _ if status & ERROR == 0 => Ok(()),
        EFI_NOT_FOUND => Err(Error::NotFound),
        EFI_OUT_OF_RESOURCES => Err(Error::NoResources),
        EFI_WRITE_PROTECTED | EFI_SECURITY_VIOLATION => Err(Error::AccessDenied),
        _ => Err(Error::Io),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_valid_names() {
        let n = name(b"MikrosBoot").unwrap();
        assert_eq!(n[..11], [77, 105, 107, 114, 111, 115, 66, 111, 111, 116, 0]);
        assert!(name(&[b'a'; 64]).is_ok());
        for bad in [&b""[..], &[b'a'; 65], b"a-b", b"a b", b"\xc3\xa9"] {
            assert_eq!(name(bad), Err(Error::InvalidArgs), "{bad:?}");
        }
    }

    #[test]
    fn checks_attributes_and_sizes() {
        assert_eq!(check_set(7, 1024), Ok(()));
        assert_eq!(check_set(6, 0), Ok(()));
        assert_eq!(check_set(7, 1025), Err(Error::InvalidArgs));
        for attributes in [0, 1, 3, 5, 0x27] {
            assert_eq!(check_set(attributes, 1), Err(Error::InvalidArgs));
        }
    }

    #[test]
    fn maps_firmware_statuses() {
        assert_eq!(result(0), Ok(()));
        assert_eq!(result(4), Ok(()), "a warning");
        assert_eq!(result(EFI_NOT_FOUND), Err(Error::NotFound));
        assert_eq!(result(EFI_OUT_OF_RESOURCES), Err(Error::NoResources));
        assert_eq!(result(EFI_WRITE_PROTECTED), Err(Error::AccessDenied));
        assert_eq!(result(EFI_SECURITY_VIOLATION), Err(Error::AccessDenied));
        assert_eq!(result(ERROR | 2), Err(Error::Io));
        assert_eq!(result(EFI_BUFFER_TOO_SMALL), Err(Error::Io));
    }
}
