//! USB descriptors: the device descriptor, and the interfaces and
//! endpoints of a configuration descriptor.

use alloc::string::String;
use alloc::vec::Vec;

pub const DEVICE: u8 = 1;
pub const CONFIGURATION: u8 = 2;
pub const STRING: u8 = 3;
const INTERFACE: u8 = 4;
const ENDPOINT: u8 = 5;
const SS_COMPANION: u8 = 0x30;
/// The class of hubs.
pub const CLASS_HUB: u8 = 9;

/// Endpoint transfer types, from `bmAttributes`.
pub const ISOCHRONOUS: u8 = 1;
pub const BULK: u8 = 2;
pub const INTERRUPT: u8 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Device {
    pub usb: u16,
    pub class: u8,
    pub max_packet0: u8,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub manufacturer: u8,
    pub product_string: u8,
    pub serial: u8,
    pub configurations: u8,
}

impl Device {
    /// The device descriptor in `bytes`, at least 18 of them.
    pub fn parse(bytes: &[u8]) -> Option<Device> {
        if bytes.len() < 18 || bytes[1] != DEVICE {
            return None;
        }
        let le = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        Some(Device {
            usb: le(2),
            class: bytes[4],
            max_packet0: bytes[7],
            vendor: le(8),
            product: le(10),
            version: le(12),
            manufacturer: bytes[14],
            product_string: bytes[15],
            serial: bytes[16],
            configurations: bytes[17],
        })
    }
}

/// An endpoint descriptor, with what its SuperSpeed companion adds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Endpoint {
    pub address: u8,
    pub attributes: u8,
    pub max_packet: u16,
    pub interval: u8,
    /// `bMaxBurst` of the companion, 0 without one.
    pub burst: u8,
    /// `wBytesPerInterval` of the companion, 0 without one.
    pub bytes_per_interval: u16,
}

impl Endpoint {
    pub fn kind(&self) -> u8 {
        self.attributes & 3
    }

    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    /// The endpoint ID, the index of its context in the device context.
    pub fn id(&self) -> u8 {
        endpoint_id(self.address)
    }
}

/// The endpoint ID of the endpoint with address `address`.
pub fn endpoint_id(address: u8) -> u8 {
    let number = address & 0xF;
    match number {
        0 => 1,
        _ => 2 * number + u8::from(address & 0x80 != 0),
    }
}

/// One alternate setting of an interface.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Interface {
    pub number: u8,
    pub alternate: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<Endpoint>,
}

/// A configuration descriptor with its interfaces.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Configuration {
    pub value: u8,
    pub interfaces: Vec<Interface>,
}

impl Configuration {
    /// The configuration descriptor in `bytes`, all `wTotalLength` of
    /// them. Descriptors that run past the end are left out.
    pub fn parse(bytes: &[u8]) -> Option<Configuration> {
        if bytes.len() < 9 || bytes[1] != CONFIGURATION {
            return None;
        }
        let mut configuration = Configuration {
            value: bytes[5],
            interfaces: Vec::new(),
        };
        let mut at = 0;
        while at + 2 <= bytes.len() {
            let length = usize::from(bytes[at]);
            if length < 2 || at + length > bytes.len() {
                break;
            }
            let d = &bytes[at..at + length];
            let interfaces = &mut configuration.interfaces;
            match d[1] {
                INTERFACE if length >= 9 => interfaces.push(Interface {
                    number: d[2],
                    alternate: d[3],
                    class: d[5],
                    subclass: d[6],
                    protocol: d[7],
                    endpoints: Vec::new(),
                }),
                ENDPOINT if length >= 7 => {
                    if let Some(interface) = interfaces.last_mut() {
                        interface.endpoints.push(Endpoint {
                            address: d[2],
                            attributes: d[3],
                            max_packet: u16::from_le_bytes([d[4], d[5]]),
                            interval: d[6],
                            burst: 0,
                            bytes_per_interval: 0,
                        });
                    }
                }
                SS_COMPANION if length >= 6 => {
                    let last = interfaces.last_mut().and_then(|i| i.endpoints.last_mut());
                    if let Some(endpoint) = last {
                        endpoint.burst = d[2];
                        endpoint.bytes_per_interval = u16::from_le_bytes([d[4], d[5]]);
                    }
                }
                _ => {}
            }
            at += length;
        }
        Some(configuration)
    }

    /// Alternate setting `alternate` of interface `number`.
    pub fn interface(&self, number: u8, alternate: u8) -> Option<&Interface> {
        let mut interfaces = self.interfaces.iter();
        interfaces.find(|i| i.number == number && i.alternate == alternate)
    }
}

/// The text of a string descriptor in `bytes`: UTF-16LE, with
/// characters that are not valid replaced.
pub fn string(bytes: &[u8]) -> String {
    if bytes.len() < 2 || bytes[1] != STRING {
        return String::new();
    }
    let length = usize::from(bytes[0]).clamp(2, bytes.len());
    let units = bytes[2..length]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c));
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// The language IDs of string descriptor 0 in `bytes`.
pub fn languages(bytes: &[u8]) -> Vec<u16> {
    if bytes.len() < 2 || bytes[1] != STRING {
        return Vec::new();
    }
    let length = usize::from(bytes[0]).clamp(2, bytes.len());
    bytes[2..length]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect()
}
