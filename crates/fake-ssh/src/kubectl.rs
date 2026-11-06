//! `--as kubectl` (or the binary copied as `kubectl`): a stand-in for
//! `kubectl` (SPEC.md §7 *kubectl tunnels*, *Testing*). Nothing here knows
//! any real cluster; every name is made up.
//!
//! - `version --client -o json`: a fake client version.
//! - `config view -o json`: `config.json` from the fixture folder, else two
//!   made-up contexts (`fake-lab`, current, and `fake-other`).
//! - `get namespaces|pods|services|deployments -o json`: `namespaces.json`
//!   or `<namespace>/<plural>.json` from the fixture folder, else built-in
//!   made-up lists. A context starting with `forbidden` gets an RBAC error,
//!   one starting with `unreachable` can't reach the server.
//! - `port-forward <kind>/<name> <local>:<port>… --address <a>`: binds every
//!   local port (`unable to listen on any of the requested ports` if one is
//!   taken), prints `Forwarding from <a>:<local> -> <port>` for each (also
//!   `[::1]` for `localhost`), then waits. By the resource's name:
//!   - `gone*`: `Error from server (NotFound)`
//!   - `unauth*`: not logged in (Unauthorized)
//!   - `unreachable*`: `Unable to connect to the server`
//!   - `slow*`: 0.7 s between the Forwarding lines
//!   - `drop-stay*`: up, then `lost connection to pod` after 1 s, and keeps
//!     running (as some kubectl versions do)
//!   - `drop*`: up, then `lost connection to pod` after 1 s, exit 1
//!   - `handling*`: up, then a `Handling connection for` line
//!
//!   A context starting with `missing` doesn't exist.
//!
//! The fixture folder is `FAKE_KUBECTL_DIR`, else `kube/` next to the
//! binary.

use std::env;
use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

/// What `version --client` reports.
pub const CLIENT_VERSION: &str = "v1.31.0-fake";

/// Parsed flags and the remaining words.
#[derive(Debug, Default, PartialEq, Eq)]
struct Call {
    context: Option<String>,
    namespace: Option<String>,
    address: Option<String>,
    output: Option<String>,
    words: Vec<String>,
}

fn parse(args: &[String]) -> Call {
    let mut call = Call::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if arg.starts_with('-') => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| args.next().cloned());
        match flag {
            "--context" => call.context = value(),
            "--namespace" | "-n" => call.namespace = value(),
            "--address" => call.address = value(),
            "-o" | "--output" => call.output = value(),
            "--request-timeout" | "--record" => {
                value();
            }
            "--client" => {}
            _ => call.words.push(arg.clone()),
        }
    }
    call
}

fn fixtures() -> Option<PathBuf> {
    env::var_os("FAKE_KUBECTL_DIR")
        .map(PathBuf::from)
        .or_else(|| Some(env::current_exe().ok()?.parent()?.join("kube")))
}

fn fixture(name: &str) -> Option<String> {
    fs::read_to_string(fixtures()?.join(name)).ok()
}

fn fail(lines: &[&str]) -> ExitCode {
    for line in lines {
        eprintln!("{line}");
    }
    ExitCode::from(1)
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

const UNREACHABLE: &str = "Unable to connect to the server: dial tcp 192.0.2.10:6443: i/o timeout";
const UNAUTHORIZED: &str = "error: You must be logged in to the server (Unauthorized)";

/// Errors every call to the cluster would get from `context`.
fn cluster_error(context: &str) -> Option<String> {
    if context.starts_with("missing") {
        Some(format!("error: context \"{context}\" does not exist"))
    } else if context.starts_with("unreachable") {
        Some(UNREACHABLE.to_string())
    } else if context.starts_with("unauth") {
        Some(UNAUTHORIZED.to_string())
    } else {
        None
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let call = parse(args);
    let words: Vec<&str> = call.words.iter().map(String::as_str).collect();
    let context = call.context.clone().unwrap_or_else(|| "fake-lab".into());
    match words.as_slice() {
        ["version", ..] => {
            say(&format!(
                "{{\"clientVersion\":{{\"major\":\"1\",\"minor\":\"31\",\"gitVersion\":\
                 \"{CLIENT_VERSION}\"}},\"kustomizeVersion\":\"v5.4.2\"}}"
            ));
            ExitCode::SUCCESS
        }
        ["config", "view", ..] => {
            say(&fixture("config.json").unwrap_or_else(|| CONFIG.to_string()));
            ExitCode::SUCCESS
        }
        ["get", what, ..] => {
            if let Some(error) = cluster_error(&context) {
                return fail(&[&error]);
            }
            if context.starts_with("forbidden") {
                return fail(&[&format!(
                    "Error from server (Forbidden): {what} is forbidden: User \"fake-user\" \
                     cannot list resource \"{what}\" in API group \"\" at the cluster scope"
                )]);
            }
            let namespace = call.namespace.as_deref().unwrap_or("default");
            let json = if *what == "namespaces" {
                fixture("namespaces.json").unwrap_or_else(|| NAMESPACES.to_string())
            } else {
                fixture(&format!("{namespace}/{what}.json"))
                    .unwrap_or_else(|| builtin_list(what).to_string())
            };
            say(&json);
            ExitCode::SUCCESS
        }
        ["port-forward", resource, forwards @ ..] => {
            port_forward(&context, resource, forwards, call.address.as_deref())
        }
        _ => fail(&[&format!("error: unknown command {:?}", call.words)]),
    }
}

fn port_forward(
    context: &str,
    resource: &str,
    forwards: &[&str],
    address: Option<&str>,
) -> ExitCode {
    if let Some(error) = cluster_error(context) {
        return fail(&[&error]);
    }
    let (kind, name) = resource.split_once('/').unwrap_or(("pod", resource));
    if name.starts_with("gone") {
        return fail(&[&format!(
            "Error from server (NotFound): {kind}s \"{name}\" not found"
        )]);
    }
    if name.starts_with("unauth") {
        return fail(&[UNAUTHORIZED]);
    }
    if name.starts_with("unreachable") {
        return fail(&[UNREACHABLE]);
    }
    let address = address.unwrap_or("localhost");
    let bind = if address == "localhost" {
        "127.0.0.1"
    } else {
        address
    };
    let mut held = Vec::new();
    let mut pairs = Vec::new();
    for forward in forwards {
        let Some((local, target)) = forward.split_once(':') else {
            return fail(&[&format!("error: invalid port spec {forward}")]);
        };
        let Ok(local) = local.parse::<u16>() else {
            return fail(&[&format!("error: invalid port spec {forward}")]);
        };
        match TcpListener::bind((bind, local)) {
            Ok(listener) => held.push(listener),
            Err(_) => {
                return fail(&[
                    &format!(
                        "Unable to listen on port {local}: Listeners failed to create with the \
                         following errors: [unable to create listener: Error listen tcp4 \
                         {bind}:{local}: bind: address already in use]"
                    ),
                    &format!(
                        "error: unable to listen on any of the requested ports: [{{{local} {target}}}]"
                    ),
                ]);
            }
        }
        pairs.push((local, target.to_string()));
    }
    for (local, target) in &pairs {
        if name.starts_with("slow") {
            thread::sleep(Duration::from_millis(700));
        }
        say(&format!("Forwarding from {bind}:{local} -> {target}"));
        if address == "localhost" {
            say(&format!("Forwarding from [::1]:{local} -> {target}"));
        }
    }
    if name.starts_with("handling")
        && let Some((local, _)) = pairs.first()
    {
        say(&format!("Handling connection for {local}"));
    }
    if name.starts_with("drop") {
        thread::sleep(Duration::from_millis(1000));
        let (local, target) = pairs.first().cloned().unwrap_or_default();
        eprintln!(
            "E1106 18:33:02.123456   4242 portforward.go:413] an error occurred forwarding \
             {local} -> {target}: error forwarding port {target} to pod 0fake, uid : lost \
             connection to pod"
        );
        if !name.starts_with("drop-stay") {
            return ExitCode::from(1);
        }
    }
    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}

/// Built-in `config view`: made-up contexts.
const CONFIG: &str = r#"{"kind":"Config","clusters":[
{"name":"fake-lab-cluster","cluster":{"server":"https://192.0.2.10:6443"}},
{"name":"fake-other-cluster","cluster":{"server":"https://fake-other.example.test"}}],
"contexts":[
{"name":"fake-lab","context":{"cluster":"fake-lab-cluster","user":"fake-admin"}},
{"name":"fake-other","context":{"cluster":"fake-other-cluster","user":"fake-user"}}],
"users":[{"name":"fake-admin","user":{"token":"REDACTED"}}],
"current-context":"fake-lab"}"#;

const NAMESPACES: &str = r#"{"kind":"List","items":[
{"metadata":{"name":"default"}},{"metadata":{"name":"fake-api"}},
{"metadata":{"name":"fake-monitoring"}}]}"#;

fn builtin_list(what: &str) -> &'static str {
    match what {
        "services" => {
            r#"{"kind":"List","items":[
{"metadata":{"name":"fake-web"},"spec":{"ports":[{"name":"http","port":80},{"name":"metrics","port":9090}]}},
{"metadata":{"name":"fake-db"},"spec":{"ports":[{"port":5432}]}}]}"#
        }
        "pods" => {
            r#"{"kind":"List","items":[
{"metadata":{"name":"fake-web-0"},"spec":{"containers":[{"name":"web","ports":[{"name":"http","containerPort":8080}]}]},"status":{"phase":"Running"}}]}"#
        }
        "deployments" => {
            r#"{"kind":"List","items":[
{"metadata":{"name":"fake-web"},"spec":{"replicas":2,"template":{"spec":{"containers":[{"name":"web","ports":[{"containerPort":8080}]}]}}},"status":{"readyReplicas":2}}]}"#
        }
        _ => r#"{"kind":"List","items":[]}"#,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn flags_are_taken_out() {
        let call = parse(&words(
            "--context lab --namespace api port-forward service/web 1:80 2:metrics --address \
             127.0.0.1 --request-timeout=5s",
        ));
        assert_eq!(call.context.as_deref(), Some("lab"));
        assert_eq!(call.namespace.as_deref(), Some("api"));
        assert_eq!(call.address.as_deref(), Some("127.0.0.1"));
        assert_eq!(
            call.words,
            ["port-forward", "service/web", "1:80", "2:metrics"]
        );
        let call = parse(&words("get pods -n x -o json"));
        assert_eq!(call.output.as_deref(), Some("json"));
        assert_eq!(call.words, ["get", "pods"]);
    }

    #[test]
    fn cluster_errors_by_context() {
        assert!(
            cluster_error("missing-ctx")
                .unwrap()
                .contains("does not exist")
        );
        assert!(
            cluster_error("unreachable")
                .unwrap()
                .contains("Unable to connect")
        );
        assert_eq!(cluster_error("fake-lab"), None);
    }
}
