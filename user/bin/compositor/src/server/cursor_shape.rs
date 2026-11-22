//! `wp_cursor_shape_manager_v1` (MIKSPEC-0066.R16): a device for a
//! client's pointer, whose `set_shape` chooses a shape of the cursor
//! theme as the cursor of the client's surfaces (MIKSPEC-0066.R28), for
//! the serial of the last `enter` it got. Tablet tools are not served:
//! the compositor offers no tablet, so no client can name one.

use super::{ProtocolError, Request, Resource, Server};
use crate::wayland::protocols::{
    wl_display, wl_pointer, wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1,
};
use crate::wayland::wire::Arg;

/// The version that the compositor offers, and the highest shape of each
/// version of the device.
pub const VERSION: u32 = 2;
const LAST_SHAPE_V1: u32 = 34;
const LAST_SHAPE_V2: u32 = 36;

impl Server {
    pub(super) fn cursor_shape_request(
        &mut self,
        request: Request,
        resource: Resource,
        args: Vec<Arg>,
    ) -> Result<(), ProtocolError> {
        let invalid = |e| ProtocolError::invalid_method(request.object, format!("{e:?}"));
        match resource {
            Resource::CursorShapes => {
                match wp_cursor_shape_manager_v1::Request::decode(request.opcode, args)
                    .map_err(invalid)?
                {
                    wp_cursor_shape_manager_v1::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                    }
                    wp_cursor_shape_manager_v1::Request::GetPointer {
                        cursor_shape_device,
                        pointer,
                    } => {
                        let c = self.client_mut(request);
                        c.resource(request.object, pointer, &wl_pointer::INTERFACE)?;
                        let interface = &wp_cursor_shape_device_v1::INTERFACE;
                        let device = Resource::CursorShape;
                        c.create(cursor_shape_device, interface, request.version, device)?;
                    }
                    wp_cursor_shape_manager_v1::Request::GetTabletToolV2 { .. } => {
                        return Err(ProtocolError::new(
                            request.object,
                            wl_display::error::INVALID_OBJECT,
                            "no tablet tools are offered",
                        ));
                    }
                }
                Ok(())
            }
            Resource::CursorShape => {
                match wp_cursor_shape_device_v1::Request::decode(request.opcode, args)
                    .map_err(invalid)?
                {
                    wp_cursor_shape_device_v1::Request::Destroy => {
                        self.client_mut(request).destroy(request.object);
                    }
                    wp_cursor_shape_device_v1::Request::SetShape { serial, shape } => {
                        let last = match request.version {
                            1 => LAST_SHAPE_V1,
                            _ => LAST_SHAPE_V2,
                        };
                        if shape == 0 || shape > last {
                            return Err(ProtocolError::new(
                                request.object,
                                wp_cursor_shape_device_v1::error::INVALID_SHAPE,
                                "a shape that the device's version does not have",
                            ));
                        }
                        self.set_cursor_shape(request.client, serial, shape);
                    }
                }
                Ok(())
            }
            _ => unreachable!("cursor_shape_request handles cursor shapes"),
        }
    }
}

/// The name of the shape `shape` of cursor-shape, which names the cursor
/// in a cursor theme (MIKSPEC-0066.R26).
pub fn shape_name(shape: u32) -> &'static str {
    const NAMES: [&str; 36] = [
        "default",
        "context_menu",
        "help",
        "pointer",
        "progress",
        "wait",
        "cell",
        "crosshair",
        "text",
        "vertical_text",
        "alias",
        "copy",
        "move",
        "no_drop",
        "not_allowed",
        "grab",
        "grabbing",
        "e_resize",
        "n_resize",
        "ne_resize",
        "nw_resize",
        "s_resize",
        "se_resize",
        "sw_resize",
        "w_resize",
        "ew_resize",
        "ns_resize",
        "nesw_resize",
        "nwse_resize",
        "col_resize",
        "row_resize",
        "all_scroll",
        "zoom_in",
        "zoom_out",
        "dnd_ask",
        "all_resize",
    ];
    NAMES
        .get((shape as usize).wrapping_sub(1))
        .copied()
        .unwrap_or("default")
}

#[cfg(test)]
mod tests {
    use super::super::seat::Cursor;
    use super::super::tests::TestClient;
    use super::super::{Role, Server};
    use super::*;
    use crate::wayland::protocols::{wl_compositor, wl_registry, wl_seat};
    use crate::wayland::wire::NewId;

    #[test]
    fn names_the_shapes_as_the_protocol_file_does() {
        assert_eq!(shape_name(1), "default");
        assert_eq!(shape_name(4), "pointer");
        assert_eq!(shape_name(34), "zoom_out");
        assert_eq!(shape_name(36), "all_resize");
        assert_eq!(shape_name(0), "default");
        assert_eq!(shape_name(37), "default");
        let file = include_str!("../../../../../target/wayland-protocols/cursor-shape-v1.xml");
        for shape in 1..=36 {
            let entry = format!("<entry name=\"{}\" value=\"{shape}\"", shape_name(shape));
            assert!(file.contains(&entry), "{entry}");
        }
    }

    #[test]
    fn sets_the_shape_for_the_last_enter() {
        let mut server = Server::new();
        server.set_screen((1280, 800));
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(&mut server, 2, &wl_compositor::INTERFACE, 3);
        client.bind(&mut server, 2, &wl_seat::INTERFACE, 4);
        client.send(&mut server, 4, 0, vec![Arg::NewId(NewId(5))]);
        client.bind(&mut server, 2, &wp_cursor_shape_manager_v1::INTERFACE, 6);
        client.send(
            &mut server,
            6,
            1,
            vec![Arg::NewId(NewId(7)), Arg::Object(Some(5))],
        );
        client.send(&mut server, 3, 0, vec![Arg::NewId(NewId(8))]);
        let (&id, surface) = server.surfaces.iter_mut().next().unwrap();
        surface.role = Role::Toplevel;
        surface.current.buffer = Some(99);
        server.buffer_sizes.insert(99, (100, 50));
        server.move_window(id, 0, 0);
        server.pointer_motion(10.0, 10.0, 1);
        let events = client.events(&mut server, &|id| match id {
            4 => &wl_seat::EVENTS,
            5 => &wl_pointer::EVENTS,
            _ => &wl_registry::EVENTS,
        });
        let enter = events.iter().find(|e| (e.0, e.1) == (5, 0)).unwrap();
        let Arg::Uint(serial) = enter.2[0] else {
            panic!("a serial");
        };
        client.send(&mut server, 7, 1, vec![Arg::Uint(serial), Arg::Uint(4)]);
        assert_eq!(server.cursor(), Cursor::Shape(4));
        client.send(&mut server, 7, 1, vec![Arg::Uint(serial), Arg::Uint(37)]);
        let invalid = wp_cursor_shape_device_v1::error::INVALID_SHAPE;
        assert_eq!(client.error(&mut server), Some(invalid));
    }
}
