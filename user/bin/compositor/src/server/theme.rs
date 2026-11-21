//! `mikros_theme_v1` (MIKSPEC-0076.R17, MIKSPEC-0066.R27): the theme's
//! values but its cursor theme, sent to each theme object when its client
//! binds the global and again to all of them whenever one of them
//! changes.

use super::{ClientId, Resource, Server};
use crate::theme::{Scheme, Theme};
use crate::wayland::protocols::mikros_theme_v1;
use crate::wayland::wire::ObjectId;

impl Server {
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Makes `theme` the theme, and sends it to every theme object if it
    /// changed.
    pub fn set_theme(&mut self, theme: Theme) {
        // Clients get every value but the cursor theme's.
        let seen = |t: &Theme| (t.accent, t.scheme, t.font.clone(), t.font_size);
        let changed = seen(&theme) != seen(&self.theme);
        self.theme = theme;
        if !changed {
            return;
        }
        let objects: Vec<(ClientId, ObjectId)> = self
            .clients
            .iter()
            .flat_map(|(&client, c)| {
                c.objects
                    .iter()
                    .filter(|(_, o)| o.resource == Resource::Theme)
                    .map(move |(&id, _)| (client, id))
            })
            .collect();
        for (client, object) in objects {
            self.send_theme(client, object);
        }
    }

    /// Sends the theme's values to `object` of `client`, ending with
    /// `done`.
    pub(super) fn send_theme(&mut self, client: ClientId, object: ObjectId) {
        let Some(c) = self.clients.get_mut(&client) else {
            return;
        };
        let theme = &self.theme;
        let [red, green, blue] = theme.accent.map(u32::from);
        mikros_theme_v1::send_accent(&mut c.out, object, red, green, blue);
        let scheme = match theme.scheme {
            Scheme::Light => 1,
            Scheme::Dark => 2,
        };
        mikros_theme_v1::send_scheme(&mut c.out, object, scheme);
        mikros_theme_v1::send_font(&mut c.out, object, &theme.font, theme.font_size);
        mikros_theme_v1::send_done(&mut c.out, object);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestClient;
    use super::*;
    use crate::wayland::protocols::wl_registry;
    use crate::wayland::wire::{Arg, NewId};

    /// The events of the theme object 3 of `client`.
    fn events(server: &mut Server, client: &TestClient) -> Vec<(u32, u16, Vec<Arg>)> {
        client.events(server, &|id| match id {
            3 => &mikros_theme_v1::EVENTS,
            _ => &wl_registry::EVENTS,
        })
    }

    #[test]
    fn sends_the_theme_on_binding_and_when_it_changes() {
        let mut server = Server::new();
        let client = TestClient::new(&mut server);
        client.send(&mut server, 1, 1, vec![Arg::NewId(NewId(2))]);
        client.bind(&mut server, 2, &mikros_theme_v1::INTERFACE, 3);
        let sent: Vec<_> = events(&mut server, &client)
            .into_iter()
            .filter(|e| e.0 == 3)
            .collect();
        assert_eq!(sent.len(), 4);
        assert_eq!(sent[0].2, [0x00, 0x66, 0xff].map(Arg::Uint));
        assert_eq!(sent[1].2, [Arg::Uint(1)]);
        let font = [Arg::String(Some("sans-serif".into())), Arg::Uint(1000)];
        assert_eq!(sent[2].2, font);
        let theme = Theme {
            scheme: Scheme::Dark,
            ..Theme::default()
        };
        server.set_theme(theme.clone());
        let sent = events(&mut server, &client);
        assert_eq!(sent.len(), 4);
        assert_eq!(sent[1].2, [Arg::Uint(2)]);
        server.set_theme(theme.clone());
        assert!(events(&mut server, &client).is_empty());
        // Nor does the cursor theme, which clients do not get.
        server.set_theme(Theme {
            cursor: "big".into(),
            ..theme
        });
        assert!(events(&mut server, &client).is_empty());
        assert_eq!(server.theme().cursor, "big");
    }
}
