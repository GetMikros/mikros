//! What pkg writes: the list of installed packages in aligned columns or
//! as JSON, and what a package is and installs.

use mikros_cli::json_string;
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

/// The list: a header `ID  VERSION  KIND  WHERE` and one line per
/// package, the columns as wide as their widest cell.
pub fn table(rows: &[Row]) -> String {
    let cells: Vec<[&str; 4]> = std::iter::once(["ID", "VERSION", "KIND", "WHERE"])
        .chain(
            rows.iter()
                .map(|r| [r.id.as_str(), &r.version, &r.kind, place(r.global)]),
        )
        .collect();
    let mut widths = [0usize; 4];
    for row in &cells {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.len());
        }
    }
    let mut text = String::new();
    for row in &cells {
        let line = format!(
            "{:w0$}  {:w1$}  {:w2$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2]
        );
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
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

/// A package's name, ID, version, description and kind, and what it
/// installs: its program, commands, services, which ask for
/// `admin.services.install`, components and driver.
pub fn describe(m: &Manifest) -> String {
    let mut text = String::new();
    let name = m.name.as_deref().unwrap_or(&m.id);
    let version = m
        .version
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_default();
    text.push_str(&format!("{name} ({}) {version}\n", m.id));
    if let Some(description) = &m.description {
        text.push_str(&format!("  {description}\n"));
    }
    text.push_str(&format!("  kind: {}\n", kind_name(m.kind)));
    if let Some(executable) = &m.executable {
        text.push_str(&format!("  program: {executable}\n"));
    }
    for bin in &m.bins {
        text.push_str(&format!("  command: {} ({})\n", bin.name, bin.path));
    }
    for (name, _) in &m.services {
        text.push_str(&format!(
            "  service: {}.{name}, which asks for admin.services.install\n",
            m.id
        ));
    }
    for component in &m.components {
        text.push_str(&format!("  component: {component}\n"));
    }
    if let Some(driver) = &m.driver {
        text.push_str(&format!(
            "  driver: {driver}, which asks for admin.drivers.install\n"
        ));
    }
    for (needed, _) in &m.needs {
        text.push_str(&format!("  needs: {needed}\n"));
    }
    text
}

/// The warnings that installing `version` over the installed `installed`
/// of `id` gives (MIKSPEC-0101.R23).
pub fn update_warnings(id: &str, installed: &str, version: &str) -> String {
    let mut text = format!(
        "warning: {id} {installed} is installed; this file cannot be checked to come from the same developer\n"
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
            "warning: {version} is older than the installed {installed}\n"
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
            table(&rows),
            "ID                  VERSION  KIND     WHERE\n\
             org.example.editor  1.2.0    app      global\n\
             org.x.lib           10.0.0   library  local\n"
        );
        assert_eq!(
            json(&rows[..1]),
            "[{\"id\":\"org.example.editor\",\"version\":\"1.2.0\",\"kind\":\"app\",\"where\":\"global\"}]\n"
        );
        assert_eq!(table(&[]), "ID  VERSION  KIND  WHERE\n");
    }

    #[test]
    fn describes_packages_and_warns_about_updates() {
        let m = Manifest::parse(
            b"id: org.example.editor\nkind: app\nversion: 1.0.0\nname: Editor\n\
              description: Edits.\nexecutable: bin/editor\nservices:\n  index: s.yaml\n",
        )
        .unwrap();
        let text = describe(&m);
        assert!(text.starts_with("Editor (org.example.editor) 1.0.0\n  Edits.\n  kind: app\n"));
        assert!(
            text.contains(
                "service: org.example.editor.index, which asks for admin.services.install"
            )
        );
        let same = update_warnings("org.x.y", "1.0.0", "1.0.0");
        assert_eq!(same.lines().count(), 1);
        assert!(same.starts_with(
            "warning: org.x.y 1.0.0 is installed; this file cannot be checked to come from the same developer"
        ));
        let older = update_warnings("org.x.y", "2.0.0", "1.9.0");
        assert!(older.ends_with("warning: 1.9.0 is older than the installed 2.0.0\n"));
    }
}
