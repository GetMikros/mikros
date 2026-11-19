//! What pkg writes: the list of installed packages in aligned columns or
//! as JSON, and what a package is and installs.

use mikros_cli::table::{Cell, columns};
use mikros_cli::{Paint, Role, json_string};
use mikros_mika::manifest::{Kind, Manifest};

/// An installed package, as `List` gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub global: bool,
}

fn place(global: bool) -> &'static str {
    if global { "global" } else { "local" }
}

/// The list: a header `ID  VERSION  KIND  WHERE` in the secondary
/// colour and one line per package, the columns as wide as their widest
/// cell.
pub fn table(rows: &[Row], paint: Paint) -> String {
    let rows: Vec<Vec<Cell>> = rows
        .iter()
        .map(|r| {
            vec![
                (r.id.clone(), None),
                (r.version.clone(), None),
                (r.kind.clone(), None),
                (place(r.global).into(), None),
            ]
        })
        .collect();
    columns(
        &["ID", "VERSION", "KIND", "WHERE"],
        &rows,
        &[false; 4],
        paint,
    )
}

pub fn json(rows: &[Row]) -> String {
    let items: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{{\"id\":{},\"version\":{},\"kind\":{},\"where\":{}}}",
                json_string(&r.id),
                json_string(&r.version),
                json_string(&r.kind),
                json_string(place(r.global))
            )
        })
        .collect();
    format!("[{}]\n", items.join(","))
}

pub fn kind_name(kind: Option<Kind>) -> &'static str {
    match kind {
        Some(Kind::App) => "app",
        Some(Kind::Library) => "library",
        Some(Kind::Driver) => "driver",
        None => "package",
    }
}

/// A package's name, ID, version, description and kind, where it is
/// installed, and what it installs: its program, commands, services, which run only when it is
/// installed for every user, components and driver; the description
/// coloured as free text.
pub fn describe(m: &Manifest, paint: Paint) -> String {
    let mut text = String::new();
    let name = m.name.as_deref().unwrap_or(&m.id);
    let version = m
        .version
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_default();
    text.push_str(&format!("{name} ({}) {version}\n", m.id));
    if let Some(description) = &m.description {
        text.push_str(&format!("  {}\n", paint.paint(Role::String, description)));
    }
    text.push_str(&format!("  kind: {}\n", kind_name(m.kind)));
    // Where it goes, which its manifest alone names (MIKSPEC-0101.R57).
    text.push_str(match m.global {
        true => "  installed: globally, for every user\n",
        false => "  installed: locally, for this user\n",
    });
    if let Some(executable) = &m.executable {
        text.push_str(&format!("  program: {executable}\n"));
    }
    for bin in &m.bins {
        text.push_str(&format!("  command: {} ({})\n", bin.name, bin.path));
    }
    for (name, _) in &m.services {
        text.push_str(&format!(
            "  service: {}.{name}, which runs when installed for every user\n",
            m.id
        ));
    }
    for component in &m.components {
        let optional = if component.optional {
            " (optional)"
        } else {
            ""
        };
        text.push_str(&format!("  component: {}{optional}\n", component.path));
    }
    if let Some(driver) = &m.driver {
        text.push_str(&format!("  driver: {driver}\n"));
    }
    for (needed, _) in &m.needs {
        text.push_str(&format!("  needs: {needed}\n"));
    }
    text
}

/// What a driver package's driver matches and needs, shown before it is
/// installed (MIKSPEC-0101.R19).
pub fn describe_driver(driver: &mikros_manifest::Driver) -> String {
    use mikros_manifest::Need;
    let mut text = String::new();
    let matches = match driver.matches.is_empty() {
        true => String::from("no device"),
        false => driver.matches.join(", "),
    };
    text.push_str(&format!("  matches: {matches}\n"));
    let needs: Vec<&str> = driver
        .needs
        .iter()
        .map(|n| match n {
            Need::Dma => "DMA",
            Need::Ioports => "I/O ports",
            Need::Bus => "the bus",
        })
        .collect();
    match needs.is_empty() {
        true => text.push_str("  needs: nothing beyond its devices\n"),
        false => text.push_str(&format!("  needs: {}\n", needs.join(", "))),
    }
    text
}

/// The warnings that installing `version` over the installed `installed`
/// of `id` gives (MIKSPEC-0101.R23), each after `warning:` in the
/// warning colour.
pub fn update_warnings(id: &str, installed: &str, version: &str, paint: Paint) -> String {
    let warning = paint.paint(Role::Warning, "warning:");
    let mut text = format!(
        "{warning} {id} {installed} is installed; this file cannot be checked to come from the same developer\n"
    );
    let older = match (
        mikros_mika::version::Version::parse(version),
        mikros_mika::version::Version::parse(installed),
    ) {
        (Some(new), Some(old)) => new < old,
        _ => false,
    };
    if older {
        text.push_str(&format!(
            "{warning} {version} is older than the installed {installed}\n"
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, version: &str, kind: &str, global: bool) -> Row {
        Row {
            id: id.into(),
            version: version.into(),
            kind: kind.into(),
            global,
        }
    }

    #[test]
    fn lists_in_columns_and_as_json() {
        let rows = [
            row("org.example.editor", "1.2.0", "app", true),
            row("org.x.lib", "10.0.0", "library", false),
        ];
        assert_eq!(
            table(&rows, Paint::never()),
            "ID                  VERSION  KIND     WHERE\n\
             org.example.editor  1.2.0    app      global\n\
             org.x.lib           10.0.0   library  local\n"
        );
        assert_eq!(
            json(&rows[..1]),
            "[{\"id\":\"org.example.editor\",\"version\":\"1.2.0\",\"kind\":\"app\",\"where\":\"global\"}]\n"
        );
        assert_eq!(table(&[], Paint::never()), "ID  VERSION  KIND  WHERE\n");
        let on = Paint::new(Some("always"), false, None);
        assert_eq!(
            table(&rows, on).lines().nth(2),
            Some("org.x.lib           10.0.0   library  local")
        );
        assert!(table(&rows, on).starts_with("\x1b[90mID\x1b[0m                  \x1b[90mVERSION"));
    }

    #[test]
    fn describes_packages_and_warns_about_updates() {
        let m = Manifest::parse(
            b"id: org.example.editor\nkind: app\nversion: 1.0.0\nname: Editor\n\
              description: Edits.\nexecutable: bin/editor\nservices:\n  index: s.yaml\n",
        )
        .unwrap();
        let text = describe(&m, Paint::never());
        assert!(text.starts_with(
            "Editor (org.example.editor) 1.0.0\n  Edits.\n  kind: app\n  \
             installed: globally, for every user\n"
        ));
        let mut local = m.clone();
        local.global = false;
        assert!(
            describe(&local, Paint::never()).contains("\n  installed: locally, for this user\n")
        );
        assert!(text.contains(
            "service: org.example.editor.index, which runs when installed for every user"
        ));
        let same = update_warnings("org.x.y", "1.0.0", "1.0.0", Paint::never());
        assert_eq!(same.lines().count(), 1);
        assert!(same.starts_with(
            "warning: org.x.y 1.0.0 is installed; this file cannot be checked to come from the same developer"
        ));
        let on = Paint::new(Some("always"), false, None);
        assert!(describe(&m, on).contains("\n  Edits.\n"));
        assert!(update_warnings("a", "1", "1", on).starts_with("\x1b[33mwarning:\x1b[0m a 1 "));
        let older = update_warnings("org.x.y", "2.0.0", "1.9.0", Paint::never());
        assert!(older.ends_with("warning: 1.9.0 is older than the installed 2.0.0\n"));
    }

    #[test]
    fn shows_what_a_driver_matches_and_needs() {
        let text = b"name: org.example.drv\nbinary: /system/drivers/org.example.drv/drv\n\
                     match: [pci:v8086d100e]\nneeds: [dma]\n";
        let driver = mikros_manifest::parse_driver("org.example.drv", text)
            .unwrap()
            .driver;
        assert_eq!(
            describe_driver(&driver),
            "  matches: pci:v8086d100e\n  needs: DMA\n"
        );
    }
}
