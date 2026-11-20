//! The Wayland protocols that the compositor serves.

pub mod wire;

/// The code of each interface of the protocol files, which the build
/// script generates: a module per interface with its requests decoded
/// into `Request`, a `send_` function per event and a module of
/// constants per enum.
#[allow(
    dead_code,
    unused_imports,
    unused_mut,
    unused_variables,
    non_upper_case_globals,
    clippy::all
)]
pub mod protocols {
    include!(concat!(env!("OUT_DIR"), "/protocols.rs"));
}

#[cfg(test)]
mod tests {
    use super::protocols::{wl_callback, wl_registry, wl_surface, xdg_toplevel};
    use super::wire::{self, Arg, DecodeError, Fd, NewId, Outgoing};
    use std::collections::VecDeque;

    /// The request that `send` encodes, decoded with the table of
    /// `requests`.
    fn request(requests: &[wire::Message], opcode: u16, args: Vec<Arg>) -> Vec<Arg> {
        let mut out = Outgoing::default();
        out.send(2, opcode, args);
        let (bytes, fds) = out.take().unwrap();
        let (header, body, _) = wire::split(&bytes).unwrap();
        let message = &requests[usize::from(header.opcode)];
        wire::decode(message, body, &mut VecDeque::from(fds)).unwrap()
    }

    #[test]
    fn decodes_requests_into_their_fields() {
        let args = request(
            &wl_surface::REQUESTS,
            1,
            vec![Arg::Object(None), Arg::Int(3), Arg::Int(-4)],
        );
        match wl_surface::Request::decode(1, args) {
            Ok(wl_surface::Request::Attach { buffer, x, y }) => {
                assert_eq!((buffer, x, y), (None, 3, -4))
            }
            other => panic!("{other:?}"),
        }
        let bind = vec![
            Arg::Uint(5),
            Arg::Untyped {
                interface: "xdg_wm_base".into(),
                version: 6,
                id: NewId(9),
            },
        ];
        let args = request(&wl_registry::REQUESTS, 0, bind);
        match wl_registry::Request::decode(0, args) {
            Ok(wl_registry::Request::Bind {
                name,
                id_interface,
                id_version,
                id,
            }) => assert_eq!(
                (name, id_interface.as_str(), id_version, id),
                (5, "xdg_wm_base", 6, NewId(9))
            ),
            other => panic!("{other:?}"),
        }
        let title = vec![Arg::String(Some("Files".into()))];
        let args = request(&xdg_toplevel::REQUESTS, 2, title);
        assert!(matches!(
            xdg_toplevel::Request::decode(2, args),
            Ok(xdg_toplevel::Request::SetTitle { title }) if title == "Files"
        ));
        assert!(matches!(
            wl_surface::Request::decode(200, vec![]),
            Err(DecodeError::Opcode)
        ));
        assert_eq!(wl_surface::REQUESTS[0].name, "destroy");
        assert!(wl_surface::REQUESTS[0].destructor);
    }

    #[test]
    fn sends_events_with_their_arguments() {
        let mut out = Outgoing::default();
        wl_callback::send_done(&mut out, 7, 1234);
        let (bytes, _) = out.take().unwrap();
        let (header, body, _) = wire::split(&bytes).unwrap();
        assert_eq!((header.sender, header.opcode), (7, 0));
        let args = wire::decode(&wl_callback::EVENTS[0], body, &mut VecDeque::new());
        assert_eq!(args, Ok(vec![Arg::Uint(1234)]));
        assert_eq!(wl_surface::error::INVALID_SCALE, 0);
        let _: fn(&mut Outgoing, u32, u32, Fd, u32) = super::protocols::wl_keyboard::send_keymap;
    }
}
