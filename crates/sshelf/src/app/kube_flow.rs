//! A kubectl tunnel made through the editor from empty to *Running*, with the
//! user's real Core (SPEC.md §12 M13 *Tests*, UI): fake-ssh copied as
//! `kubectl` (set as the override), its lookups answered from fixture JSON
//! with made-up names. Nothing reaches a real kubectl, kubeconfig or
//! cluster: the Core's `Paths` carry no `PATH` or `KUBECONFIG`.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sshelf_core::paths::Paths;
use sshelf_core::supervisor::TunnelState;

use super::{SshelfApp, Startup, View};
use crate::test_util::{self, fake_ssh_binary};

const NAMESPACES: &str = r#"{"kind":"NamespaceList","items":[
{"metadata":{"name":"flow-api"}},{"metadata":{"name":"flow-ops"}}]}"#;
const SERVICES: &str = r#"{"kind":"ServiceList","items":[
{"metadata":{"name":"flow-web"},"spec":{"ports":[{"name":"http","port":80}]}}]}"#;

fn settle(harness: &mut Harness<'_, SshelfApp>, what: &str, done: impl Fn(&SshelfApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(harness.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Steps until `text` is drawn.
fn until_drawn(harness: &mut Harness<'_, SshelfApp>, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !test_util::drawn_text(harness).contains(text) {
        assert!(Instant::now() < deadline, "timed out waiting for {text:?}");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn frames(harness: &mut Harness<'_, SshelfApp>) {
    for _ in 0..4 {
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// fake-ssh as `<dir>/kubectl`, with its fixtures in `<dir>/kube/`.
fn fake_kubectl(dir: &Path) -> std::path::PathBuf {
    let kube = dir.join("kube");
    fs::create_dir_all(kube.join("flow-api")).unwrap();
    fs::write(kube.join("namespaces.json"), NAMESPACES).unwrap();
    fs::write(kube.join("flow-api").join("services.json"), SERVICES).unwrap();
    let kubectl = dir.join(format!("kubectl{}", std::env::consts::EXE_SUFFIX));
    fs::copy(fake_ssh_binary(), &kubectl).unwrap();
    kubectl
}

#[test]
fn a_kubectl_tunnel_goes_from_the_editor_to_running() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let paths = Paths::new(&home, home.join(".config").join("sshelf"));
    fs::create_dir_all(&paths.ssh_dir).unwrap();
    fs::create_dir_all(&paths.app_dir).unwrap();
    fs::write(&paths.ssh_config, "Host flow-host\n").unwrap();
    let kubectl = fake_kubectl(&root.path().join("tools"));
    fs::write(
        paths.app_dir.join("settings.toml"),
        format!(
            "version = 1\nssh_path = '{}'\nkubectl_path = '{}'\nreduce_motion = true\n\
             tour = \"done\"\n",
            fake_ssh_binary().display(),
            kubectl.display()
        ),
    )
    .unwrap();
    let temp = root.path().join("temp");
    fs::create_dir_all(&temp).unwrap();
    let startup = Startup {
        tray: false,
        fake_ssh: None,
        temp,
        user_paths: Some(paths),
        show: None,
        updates: None,
    };
    let mut harness = test_util::harness()
        .with_size([1280.0, 900.0])
        .build_eframe(|cc| SshelfApp::with(cc, startup));
    settle(&mut harness, "kubectl found", |app| {
        app.data.is_some() && app.kubectl.as_ref().is_some_and(|k| k.kubectl.is_ok())
    });
    harness.state_mut().view = View::Tunnels;
    frames(&mut harness);

    harness.get_by_label("New tunnel").click();
    frames(&mut harness);
    harness.get_by_label("kubectl").click();
    frames(&mut harness);
    harness.state_mut().tunnels.draft_mut().unwrap().name = "flow api".into();
    frames(&mut harness);

    // The fake's contexts: the current one is shown.
    until_drawn(&mut harness, "fake-lab · current context");
    harness.get_by_label("Namespace").click();
    until_drawn(&mut harness, "flow-ops");
    harness.get_by_label("flow-api").click();
    frames(&mut harness);
    harness.get_by_label("Resource").click();
    until_drawn(&mut harness, "flow-web");
    harness.get_by_label("flow-web").click();
    frames(&mut harness);
    harness.get_by_label("80 http").click();
    frames(&mut harness);
    harness.get_by_label("Ready");
    harness.get_by_label("Save and start").click();
    frames(&mut harness);

    settle(&mut harness, "the tunnel running", |app| {
        let Some(data) = &app.data else { return false };
        let Some(tunnel) = data.tunnels.tunnels.iter().find(|t| t.name == "flow api") else {
            return false;
        };
        app.tunnel_states
            .as_ref()
            .is_some_and(|s| s.state(&tunnel.id) == TunnelState::Running)
    });
    until_drawn(&mut harness, "flow api is running on");
    let tunnel = harness.state().data.as_ref().unwrap().tunnels.tunnels[0].clone();
    assert!(tunnel.is_kubectl());
    assert_eq!(tunnel.kube.resource.to_string(), "service/flow-web");
    assert_eq!(tunnel.kube.forwards[0].listen, 18080);
    // Saved as a kubectl tunnel, with no ssh fields.
    let saved = fs::read_to_string(
        root.path()
            .join("home")
            .join(".config")
            .join("sshelf")
            .join("tunnels.toml"),
    )
    .unwrap();
    assert!(saved.contains("type = \"kubectl\""), "{saved}");
    assert!(!saved.contains("host ="), "{saved}");
}
