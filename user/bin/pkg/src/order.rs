//! The order in which `pkg install` installs the packages it is given:
//! each after those of them that it needs (MIKSPEC-0101.R28).

/// The indices of `packages`, each an ID with the IDs it needs, in an
/// order in which each package comes after the given packages it needs;
/// the ID of a package in a cycle of needs when there is one.
pub fn order(packages: &[(String, Vec<String>)]) -> Result<Vec<usize>, String> {
    let mut placed: Vec<usize> = Vec::with_capacity(packages.len());
    let mut done = vec![false; packages.len()];
    while placed.len() < packages.len() {
        let next = (0..packages.len()).find(|&i| {
            !done[i]
                && packages[i].1.iter().all(|needed| {
                    packages
                        .iter()
                        .enumerate()
                        .all(|(j, (id, _))| id != needed || done[j] || j == i)
                })
        });
        match next {
            Some(i) => {
                done[i] = true;
                placed.push(i);
            }
            None => {
                let stuck = (0..packages.len()).find(|&i| !done[i]).unwrap_or(0);
                return Err(packages[stuck].0.clone());
            }
        }
    }
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, needs: &[&str]) -> (String, Vec<String>) {
        (id.into(), needs.iter().map(|n| n.to_string()).collect())
    }

    #[test]
    fn puts_what_is_needed_first() {
        let packages = [
            p("org.example.app", &["org.example.lib", "org.example.gone"]),
            p("org.example.lib", &["org.example.base"]),
            p("org.example.base", &[]),
        ];
        assert_eq!(order(&packages).unwrap(), [2, 1, 0]);
        let cycle = [p("org.a.x", &["org.a.y"]), p("org.a.y", &["org.a.x"])];
        assert_eq!(order(&cycle).unwrap_err(), "org.a.x");
        assert_eq!(order(&[]).unwrap(), Vec::<usize>::new());
    }
}
