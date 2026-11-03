//! `Include` resolution against read-only fixture trees and temp dirs.

use std::fs;
use std::path::{Path, PathBuf};

use sshconf::{
    ConfigSet, Error, LineKind, LineRef, MAX_INCLUDE_DEPTH, ResolveOptions, Severity, WatchDir,
};

fn fixture(case: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/include")
        .join(case)
}

/// Fixture dirs act as the ssh dir; `~` is their `home/` subdir.
fn opts(dir: &Path) -> ResolveOptions {
    ResolveOptions {
        home: dir.join("home"),
        ssh_dir: dir.to_path_buf(),
        max_depth: MAX_INCLUDE_DEPTH,
    }
}

fn load(case: &str) -> ConfigSet {
    let dir = fixture(case);
    ConfigSet::load(&dir.join("config"), &opts(&dir)).expect("loading fixture")
}

fn name_at(set: &ConfigSet, at: LineRef) -> String {
    match &set.line(at).kind {
        LineKind::HostHeader(h) => h.patterns[0].pattern.clone(),
        LineKind::MatchHeader(m) => m.args.join(" "),
        other => panic!("not a header: {other:?}"),
    }
}

/// First pattern of every header, in evaluation order.
fn header_names(set: &ConfigSet) -> Vec<String> {
    set.headers.iter().map(|h| name_at(set, h.at)).collect()
}

fn file_names(set: &ConfigSet) -> Vec<String> {
    set.files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

fn messages(set: &ConfigSet, severity: Severity) -> Vec<String> {
    set.diagnostics
        .iter()
        .filter(|d| d.severity == severity)
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn glob_expands_sorted_skipping_hidden_files_and_dirs() {
    let set = load("glob");
    assert_eq!(header_names(&set), ["a", "b", "main"]);
    assert_eq!(file_names(&set), ["config", "10-a.conf", "20-b.conf"]);
    let warnings = messages(&set, Severity::Warning);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("is a directory"), "{warnings:?}");
    assert!(messages(&set, Severity::Error).is_empty());
}

#[test]
fn every_file_knows_the_include_that_reached_it() {
    let set = load("glob");
    assert_eq!(set.included_from.len(), set.files.len());
    assert_eq!(set.included_from[0], None);
    for from in &set.included_from[1..] {
        let at = from.expect("an included file");
        assert_eq!(at.file, 0);
        assert!(matches!(set.line(at).kind, LineKind::Include(_)));
    }
}

#[test]
fn tilde_expands_to_home() {
    let set = load("tilde");
    assert_eq!(header_names(&set), ["t"]);
}

#[test]
fn multiple_paths_keep_argument_order() {
    let set = load("multi");
    assert_eq!(header_names(&set), ["b", "a"]);
}

#[test]
fn include_inside_host_is_conditional() {
    let set = load("conditional");
    let shown: Vec<(String, Vec<String>)> = set
        .headers
        .iter()
        .map(|h| {
            let conditions = h.conditions.iter().map(|&c| name_at(&set, c)).collect();
            (name_at(&set, h.at), conditions)
        })
        .collect();
    let expected = [
        ("bastion", vec![]),
        ("w1", vec!["bastion"]),
        ("w2", vec!["bastion"]),
        ("n", vec!["bastion", "w2"]),
        ("after", vec![]),
    ];
    let expected: Vec<(String, Vec<String>)> = expected
        .into_iter()
        .map(|(n, c)| (n.to_string(), c.into_iter().map(String::from).collect()))
        .collect();
    assert_eq!(shown, expected);
    assert!(set.diagnostics.is_empty(), "{:?}", set.diagnostics);
}

#[test]
fn unmatched_includes_are_info_not_errors() {
    let set = load("missing");
    assert_eq!(header_names(&set), ["m"]);
    assert_eq!(messages(&set, Severity::Info).len(), 2);
    assert!(messages(&set, Severity::Warning).is_empty());
    assert!(messages(&set, Severity::Error).is_empty());
}

#[test]
fn include_cycle_is_reported_not_followed() {
    let set = load("cycle");
    assert_eq!(header_names(&set), ["a", "b"]);
    let errors: Vec<_> = set
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("cycle"));
    assert!(set.location(errors[0].at).ends_with("b.conf:2"));
}

#[test]
fn include_depth_is_limited() {
    let dir = fixture("depth");
    let set = ConfigSet::load(&dir.join("config"), &opts(&dir)).unwrap();
    assert_eq!(header_names(&set), ["d1", "d2", "d3"]);
    assert!(set.diagnostics.is_empty());

    let shallow = ResolveOptions {
        max_depth: 2,
        ..opts(&dir)
    };
    let set = ConfigSet::load(&dir.join("config"), &shallow).unwrap();
    assert_eq!(header_names(&set), ["d1", "d2"]);
    let errors = messages(&set, Severity::Error);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("too many nested includes"), "{errors:?}");
}

#[test]
fn missing_main_file_is_an_error() {
    let dir = fixture("glob");
    let err = ConfigSet::load(&dir.join("does-not-exist"), &opts(&dir)).unwrap_err();
    assert!(matches!(err, Error::Io { .. }), "{err}");
}

#[test]
fn absolute_paths_escaped_base_and_problem_files() {
    let tmp = tempfile::tempdir().unwrap();
    // Glob metacharacters in the ssh dir itself must be taken literally.
    let ssh = tmp.path().join("ssh [x]");
    let other = tmp.path().join("other");
    fs::create_dir_all(&ssh).unwrap();
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("abs.conf"), "Host abs\n").unwrap();
    fs::write(ssh.join("rel.conf"), "Host rel\n").unwrap();
    fs::write(ssh.join("binary.conf"), b"Host bin\n\xFF\n").unwrap();
    let abs = other.join("abs.conf").to_string_lossy().replace('\\', "/");
    fs::write(
        ssh.join("config"),
        format!("Include \"{abs}\"\nInclude rel.conf binary.conf\nInclude %d/x ~someone/x\nHost\n"),
    )
    .unwrap();

    let opts = ResolveOptions {
        home: tmp.path().to_path_buf(),
        ssh_dir: ssh.clone(),
        max_depth: MAX_INCLUDE_DEPTH,
    };
    let set = ConfigSet::load(&ssh.join("config"), &opts).unwrap();
    assert_eq!(header_names(&set), ["abs", "rel"]);

    let errors = messages(&set, Severity::Error);
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors.iter().any(|e| e.contains("not valid UTF-8")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("`Host` without patterns")),
        "{errors:?}"
    );

    let warnings = messages(&set, Severity::Warning);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings[0].contains("`%d/x`"), "{warnings:?}");
    assert!(warnings[1].contains("`~user`"), "{warnings:?}");
}

#[test]
fn watch_dirs_cover_file_parents_and_include_globs() {
    let dir = fixture("glob");
    let set = load("glob");
    assert_eq!(
        set.watch_dirs(),
        [
            WatchDir {
                path: dir.clone(),
                recursive: false
            },
            WatchDir {
                path: dir.join("conf.d"),
                recursive: false
            },
        ]
    );
}

#[test]
fn watch_dirs_include_missing_and_wildcard_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let ssh = tmp.path().join("ssh");
    let other = tmp.path().join("other");
    fs::create_dir_all(&ssh).unwrap();
    let abs = other.to_string_lossy().replace('\\', "/");
    fs::write(
        ssh.join("config"),
        format!(
            "Include missing/*.conf\nInclude hosts/*/x.conf\nInclude ~/home.d/*.conf\nInclude \"{abs}/*.conf\"\n"
        ),
    )
    .unwrap();
    let opts = ResolveOptions {
        home: tmp.path().to_path_buf(),
        ssh_dir: ssh.clone(),
        max_depth: MAX_INCLUDE_DEPTH,
    };
    let set = ConfigSet::load(&ssh.join("config"), &opts).unwrap();
    assert_eq!(set.includes.len(), 4);

    let mut expected = vec![
        WatchDir {
            path: ssh.clone(),
            recursive: false,
        },
        WatchDir {
            path: ssh.join("missing"),
            recursive: false,
        },
        WatchDir {
            path: ssh.join("hosts"),
            recursive: true,
        },
        WatchDir {
            path: tmp.path().join("home.d"),
            recursive: false,
        },
        WatchDir {
            path: other,
            recursive: false,
        },
    ];
    expected.sort();
    assert_eq!(set.watch_dirs(), expected);
}

#[test]
fn file_included_twice_is_parsed_once_and_walked_twice() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("shared.conf"), "Host shared\n").unwrap();
    fs::write(
        tmp.path().join("config"),
        "Include shared.conf\nHost x\nInclude shared.conf\n",
    )
    .unwrap();
    let opts = ResolveOptions {
        home: tmp.path().to_path_buf(),
        ssh_dir: tmp.path().to_path_buf(),
        max_depth: MAX_INCLUDE_DEPTH,
    };
    let set = ConfigSet::load(&tmp.path().join("config"), &opts).unwrap();
    assert_eq!(set.files.len(), 2);
    assert_eq!(header_names(&set), ["shared", "x", "shared"]);
    // The second inclusion sits inside `Host x`, so it is conditional.
    assert!(set.headers[0].conditions.is_empty());
    assert_eq!(set.headers[2].conditions.len(), 1);
}
