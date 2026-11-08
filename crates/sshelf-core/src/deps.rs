//! Tunnel dependencies (SPEC.md §7 *Order and dependencies*, M15): pure
//! graph helpers over a [`TunnelsFile`]. Unknown ids in `depends_on` are
//! ignored; loops never make a walk run forever.

use std::collections::BTreeSet;

use crate::tunnels::TunnelsFile;

/// The tunnels `id` depends on directly (known ones only).
fn direct<'a>(file: &'a TunnelsFile, id: &str) -> Vec<&'a str> {
    file.tunnel(id)
        .map(|t| {
            t.depends_on
                .iter()
                .map(String::as_str)
                .filter(|d| file.tunnel(d).is_some())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `from` depends on `to`, directly or through others.
pub fn reaches(file: &TunnelsFile, from: &str, to: &str) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = direct(file, from);
    while let Some(id) = stack.pop() {
        if id == to {
            return true;
        }
        if seen.insert(id) {
            stack.extend(direct(file, id));
        }
    }
    false
}

/// The tunnels on a dependency loop: they load, but none of them starts.
pub fn in_loop(file: &TunnelsFile) -> BTreeSet<String> {
    file.tunnels
        .iter()
        .filter(|t| reaches(file, &t.id, &t.id))
        .map(|t| t.id.clone())
        .collect()
}

/// Up to three ways `from` depends on `to`, each from `from` to `to` (the
/// hidden-loop tooltip's chains).
pub fn chains(file: &TunnelsFile, from: &str, to: &str) -> Vec<Vec<String>> {
    fn walk(file: &TunnelsFile, path: &mut Vec<String>, to: &str, found: &mut Vec<Vec<String>>) {
        let last = path.last().cloned().unwrap_or_default();
        for next in direct(file, &last) {
            if found.len() == 3 {
                return;
            }
            if next == to {
                let mut chain = path.clone();
                chain.push(next.to_string());
                found.push(chain);
            } else if !path.iter().any(|p| p == next) {
                path.push(next.to_string());
                walk(file, path, to, found);
                path.pop();
            }
        }
    }
    let mut found = Vec::new();
    walk(file, &mut vec![from.to_string()], to, &mut found);
    found
}

/// Everything `id` depends on, transitively, deepest first, each once
/// (`id` itself left out).
pub fn closure(file: &TunnelsFile, id: &str) -> Vec<String> {
    fn visit(file: &TunnelsFile, id: &str, seen: &mut BTreeSet<String>, out: &mut Vec<String>) {
        for dep in direct(file, id) {
            if seen.insert(dep.to_string()) {
                visit(file, dep, seen, out);
                out.push(dep.to_string());
            }
        }
    }
    let mut seen = BTreeSet::from([id.to_string()]);
    let mut out = Vec::new();
    visit(file, id, &mut seen, &mut out);
    out
}

/// `tunnels` in start order: each one moved down just enough to sit below
/// every listed tunnel it depends on (directly or through others); the rest
/// keep their order. Tunnels on a loop stay where they are.
pub fn ordered(tunnels: &[String], file: &TunnelsFile) -> Vec<String> {
    let mut list = tunnels.to_vec();
    let looped = in_loop(file);
    // Each move puts one tunnel below a dependency for good: n² bounds it.
    for _ in 0..list.len() * list.len() + 1 {
        let moved = (0..list.len()).find_map(|i| {
            if looped.contains(&list[i]) {
                return None;
            }
            let last = (i + 1..list.len())
                .filter(|&j| reaches(file, &list[i], &list[j]))
                .max()?;
            Some((i, last))
        });
        let Some((from, to)) = moved else { break };
        let id = list.remove(from);
        list.insert(to, id);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnels::{Group, Tunnel};

    /// Tunnels `a`, `b`, … with `deps` as (tunnel, its dependencies).
    fn file(ids: &[&str], deps: &[(&str, &[&str])]) -> TunnelsFile {
        let mut file = TunnelsFile::default();
        for id in ids {
            let mut tunnel = Tunnel::ssh(id, id, "h", Vec::new());
            if let Some((_, on)) = deps.iter().find(|(t, _)| t == id) {
                tunnel.depends_on = on.iter().map(|d| d.to_string()).collect();
            }
            file.upsert_tunnel(tunnel);
        }
        file
    }

    fn list(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reaches_follows_chains_and_skips_unknown_ids() {
        let f = file(&["a", "b", "c"], &[("a", &["b", "ghost"]), ("b", &["c"])]);
        assert!(reaches(&f, "a", "c"));
        assert!(!reaches(&f, "c", "a"));
        assert!(!reaches(&f, "a", "ghost"));
        assert_eq!(closure(&f, "a"), ["c", "b"]);
        assert!(in_loop(&f).is_empty());
    }

    #[test]
    fn loops_are_found_and_never_walked_forever() {
        let f = file(
            &["a", "b", "c"],
            &[("a", &["b"]), ("b", &["a"]), ("c", &["a"])],
        );
        assert_eq!(in_loop(&f), BTreeSet::from(["a".into(), "b".into()]));
        assert_eq!(closure(&f, "c"), ["b", "a"]);
        // The order leaves loop tunnels in place.
        assert_eq!(ordered(&list(&["a", "b", "c"]), &f), list(&["a", "b", "c"]));
    }

    #[test]
    fn the_order_moves_a_tunnel_down_just_enough() {
        // mailpit depends on k3s, which sits below it: mailpit moves just
        // under k3s; vault and postgres keep their places.
        let f = file(
            &["vault", "mailpit", "k3s", "postgres"],
            &[("mailpit", &["k3s"])],
        );
        let order = ordered(&list(&["vault", "mailpit", "k3s", "postgres"]), &f);
        assert_eq!(order, list(&["vault", "k3s", "mailpit", "postgres"]));
        // Already right: unchanged.
        assert_eq!(ordered(&order, &f), order);
        // Through an outside tunnel: x isn't listed but links them.
        let f = file(&["a", "x", "b"], &[("a", &["x"]), ("x", &["b"])]);
        assert_eq!(ordered(&list(&["a", "b"]), &f), list(&["b", "a"]));
    }

    #[test]
    fn chains_list_the_ways_one_tunnel_reaches_another() {
        let f = file(
            &["postgres", "mailpit", "k3s", "web"],
            &[
                ("postgres", &["mailpit", "web"]),
                ("mailpit", &["k3s"]),
                ("web", &["k3s"]),
            ],
        );
        assert_eq!(
            chains(&f, "postgres", "k3s"),
            [
                list(&["postgres", "mailpit", "k3s"]),
                list(&["postgres", "web", "k3s"])
            ]
        );
        assert!(chains(&f, "k3s", "postgres").is_empty());
    }

    #[test]
    fn a_hand_edited_file_is_put_in_order_on_load() {
        let mut f = file(&["mailpit", "k3s"], &[("mailpit", &["k3s"])]);
        f.upsert_group(Group {
            id: "g".into(),
            name: "Dev".into(),
            tunnels: list(&["mailpit", "k3s"]),
            start_on_launch: false,
            start_deps_without_asking: false,
        });
        assert_eq!(f.normalized().groups[0].tunnels, list(&["k3s", "mailpit"]));
    }

    #[test]
    fn deleting_a_tunnel_drops_it_from_every_dependency() {
        let mut f = file(&["a", "b"], &[("a", &["b"])]);
        assert!(f.remove_tunnel("b"));
        assert!(f.tunnel("a").unwrap().depends_on.is_empty());
    }
}
