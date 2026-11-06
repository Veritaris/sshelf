//! kubectl discovery and what Settings shows about it (SPEC.md §8 *kubectl*,
//! M13): the binary (override, `PATH`, then the usual install folders), its
//! client version, the kubeconfig files and their contexts. Nothing here
//! contacts a cluster: `kubectl version --client` and `kubectl config view`
//! only read local files.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::paths::KubeEnv;
use crate::process;
use crate::ssh::{QUICK_TIMEOUT, RESOLVE_TIMEOUT, Tool, find_in_path};
use crate::tunnels::ResourceKind;

/// Why the tour's sandbox has no kubectl (SPEC.md §10 *kubectl tunnels*).
pub const NOT_IN_TOUR: &str = "Not part of the tour";

/// The official install page, for *How to install kubectl*.
pub const INSTALL_URL: &str = "https://kubernetes.io/docs/tasks/tools/";

/// Where the kubectl binary came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KubectlSource {
    /// `kubectl_path` in Settings.
    Override,
    Path,
    /// An install folder, by name (e.g. "Docker Desktop").
    Folder(String),
}

/// The kubectl sshelf runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kubectl {
    pub tool: Tool,
    pub source: KubectlSource,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocateError {
    #[error("the kubectl binary set in Settings does not exist: {}", .0.display())]
    OverrideMissing(PathBuf),
    #[error("kubectl not found")]
    NotFound,
}

/// The install folders searched after `PATH`, for this OS: Docker Desktop and
/// Rancher Desktop on Windows and macOS, and on macOS Homebrew's and
/// `/usr/local/bin` (an app started from Finder doesn't get the shell's
/// `PATH`). Linux has none.
pub fn install_folders(
    home: &Path,
    program_files: Option<&OsStr>,
    local_app_data: Option<&OsStr>,
) -> Vec<(PathBuf, String)> {
    let mut folders = Vec::new();
    let mut add = |path: PathBuf, name: &str| folders.push((path, name.to_string()));
    if cfg!(windows) {
        if let Some(program_files) = program_files {
            add(
                Path::new(program_files).join(r"Docker\Docker\resources\bin"),
                "Docker Desktop",
            );
        }
        add(home.join(r".rd\bin"), "Rancher Desktop");
        if let Some(local) = local_app_data {
            add(
                Path::new(local).join(r"Programs\Rancher Desktop\resources\resources\win32\bin"),
                "Rancher Desktop",
            );
        }
    } else if cfg!(target_os = "macos") {
        add(
            PathBuf::from("/Applications/Docker.app/Contents/Resources/bin"),
            "Docker Desktop",
        );
        add(home.join(".rd/bin"), "Rancher Desktop");
        add(
            PathBuf::from(
                "/Applications/Rancher Desktop.app/Contents/Resources/resources/darwin/bin",
            ),
            "Rancher Desktop",
        );
        add(PathBuf::from("/opt/homebrew/bin"), "Homebrew");
        add(PathBuf::from("/usr/local/bin"), "/usr/local/bin");
    }
    let _ = (program_files, local_app_data);
    folders
}

/// Finds kubectl: the override (no fallback when it is missing, like ssh),
/// then `PATH`, then the install folders.
pub fn locate(override_path: Option<&Path>, env: &KubeEnv) -> Result<Kubectl, LocateError> {
    let (program, source) = if let Some(path) = override_path {
        if !path.is_file() {
            return Err(LocateError::OverrideMissing(path.to_path_buf()));
        }
        (path.to_path_buf(), KubectlSource::Override)
    } else if let Some(found) = find_in_path("kubectl", &env.path) {
        (found, KubectlSource::Path)
    } else {
        env.folders
            .iter()
            .find_map(|(dir, name)| {
                find_in_path("kubectl", dir.as_os_str())
                    .map(|found| (found, KubectlSource::Folder(name.clone())))
            })
            .ok_or(LocateError::NotFound)?
    };
    let mut tool = Tool::new(program.clone());
    if cfg!(target_os = "macos") {
        tool.env
            .push(("PATH".into(), macos_path(&program, &env.path)));
    }
    Ok(Kubectl { tool, source })
}

/// kubectl's `PATH` on macOS: its own folder, Homebrew's and `/usr/local/bin`
/// ahead of sshelf's, so exec credential plugins (cloud CLIs) are found
/// from an app started by Finder.
pub fn macos_path(program: &Path, path: &OsStr) -> OsString {
    let mut dirs: Vec<PathBuf> = program
        .parent()
        .map(Path::to_path_buf)
        .into_iter()
        .collect();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    dirs.extend(std::env::split_paths(path));
    let mut seen = Vec::new();
    dirs.retain(|d| {
        !d.as_os_str().is_empty() && !seen.contains(d) && {
            seen.push(d.clone());
            true
        }
    });
    std::env::join_paths(dirs).unwrap_or_else(|_| path.to_os_string())
}

/// `kubectl version --client -o json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubectlVersion {
    pub client: String,
    pub kustomize: Option<String>,
}

impl KubectlVersion {
    /// "Client v1.31.2, Kustomize v5.4.2".
    pub fn describe(&self) -> String {
        match &self.kustomize {
            Some(k) => format!("Client {}, Kustomize {k}", self.client),
            None => format!("Client {}", self.client),
        }
    }
}

pub fn parse_version(json: &str) -> Result<KubectlVersion, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("unexpected output: {e}"))?;
    let client = value
        .pointer("/clientVersion/gitVersion")
        .and_then(Value::as_str)
        .ok_or("no client version in kubectl's output")?;
    Ok(KubectlVersion {
        client: client.to_string(),
        kustomize: value
            .get("kustomizeVersion")
            .and_then(Value::as_str)
            .map(String::from),
    })
}

/// The last non-empty line of kubectl's output, for errors.
pub fn last_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(String::from)
}

fn run_json(tool: &Tool, args: &[&str], timeout: std::time::Duration) -> Result<String, String> {
    let mut cmd = tool.command();
    cmd.args(args);
    let out = process::run(cmd, timeout).map_err(|e| e.to_string())?;
    if out.code == Some(0) {
        Ok(out.stdout)
    } else {
        Err(last_line(&out.stderr)
            .or_else(|| last_line(&out.stdout))
            .unwrap_or_else(|| format!("kubectl exited with code {:?}", out.code)))
    }
}

pub fn version(tool: &Tool) -> Result<KubectlVersion, String> {
    parse_version(&run_json(
        tool,
        &["version", "--client", "-o", "json"],
        QUICK_TIMEOUT,
    )?)
}

/// One context of the kubeconfig.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KubeContext {
    pub name: String,
    pub cluster: String,
    /// The cluster's API server URL.
    pub server: String,
    pub user: String,
}

/// `kubectl config view -o json`: the merged kubeconfig, with the secrets
/// kubectl redacts left out entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KubeConfigView {
    pub contexts: Vec<KubeContext>,
    /// `current-context`, if set.
    pub current: Option<String>,
}

impl KubeConfigView {
    pub fn context(&self, name: &str) -> Option<&KubeContext> {
        self.contexts.iter().find(|c| c.name == name)
    }
}

pub fn parse_config_view(json: &str) -> Result<KubeConfigView, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("unexpected output: {e}"))?;
    let text = |v: &Value, path: &str| {
        v.pointer(path)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let list = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let clusters = list("clusters");
    let contexts = list("contexts")
        .iter()
        .map(|c| {
            let cluster = text(c, "/context/cluster");
            let server = clusters
                .iter()
                .find(|k| text(k, "/name") == cluster)
                .map(|k| text(k, "/cluster/server"))
                .unwrap_or_default();
            KubeContext {
                name: text(c, "/name"),
                user: text(c, "/context/user"),
                cluster,
                server,
            }
        })
        .filter(|c| !c.name.is_empty())
        .collect();
    let current = text(&value, "/current-context");
    Ok(KubeConfigView {
        contexts,
        current: (!current.is_empty()).then_some(current),
    })
}

pub fn config_view(tool: &Tool) -> Result<KubeConfigView, String> {
    parse_config_view(&run_json(
        tool,
        &["config", "view", "-o", "json"],
        QUICK_TIMEOUT,
    )?)
}

/// How long a lookup may take on the cluster's side (`--request-timeout`).
pub const LOOKUP_TIMEOUT: &str = "5s";

/// What the tunnel editor asks the cluster for (SPEC.md §7 *kubectl
/// tunnels*, *Lookups for the editor*).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KubeQuery {
    /// Empty: the current context.
    pub context: String,
    pub what: KubeList,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KubeList {
    Namespaces,
    Resources {
        namespace: String,
        kind: ResourceKind,
    },
}

impl KubeQuery {
    /// kubectl's arguments.
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if !self.context.is_empty() {
            args.extend(["--context".to_string(), self.context.clone()]);
        }
        match &self.what {
            KubeList::Namespaces => args.extend(["get".into(), "namespaces".into()]),
            KubeList::Resources { namespace, kind } => args.extend([
                "--namespace".to_string(),
                namespace.clone(),
                "get".into(),
                kind.plural().into(),
            ]),
        }
        args.extend([
            "-o".to_string(),
            "json".into(),
            format!("--request-timeout={LOOKUP_TIMEOUT}"),
        ]);
        args
    }

    /// As the editor's footer shows it: `kubectl get services -n api`.
    pub fn describe(&self) -> String {
        match &self.what {
            KubeList::Namespaces => "kubectl get namespaces".into(),
            KubeList::Resources { namespace, kind } => {
                format!("kubectl get {} -n {namespace}", kind.plural())
            }
        }
    }
}

/// A port a resource offers: its number and name, if named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubePort {
    pub port: u16,
    pub name: Option<String>,
}

/// One namespace, pod, service or deployment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubeItem {
    pub name: String,
    pub ports: Vec<KubePort>,
    /// A pod's phase ("Running"), a deployment's "2/2 ready".
    pub status: Option<String>,
}

/// The items of `kubectl get … -o json` for `what`.
pub fn parse_list(what: &KubeList, json: &str) -> Result<Vec<KubeItem>, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("unexpected output: {e}"))?;
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or("no items in kubectl's output")?;
    let text = |v: &Value, path: &str| v.pointer(path).and_then(Value::as_str).map(String::from);
    let ports = |list: Option<&Value>, key: &str| -> Vec<KubePort> {
        list.and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some(KubePort {
                    port: u16::try_from(p.get(key)?.as_u64()?).ok()?,
                    name: text(p, "/name"),
                })
            })
            .collect()
    };
    let container_ports = |spec: Option<&Value>| -> Vec<KubePort> {
        spec.and_then(|s| s.get("containers"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|c| ports(c.get("ports"), "containerPort"))
            .collect()
    };
    let mut parsed: Vec<KubeItem> = items
        .iter()
        .filter_map(|item| {
            let name = text(item, "/metadata/name")?;
            let (ports, status) = match what {
                KubeList::Namespaces => (Vec::new(), None),
                KubeList::Resources { kind, .. } => match kind {
                    ResourceKind::Service => (ports(item.pointer("/spec/ports"), "port"), None),
                    ResourceKind::Pod => (
                        container_ports(item.get("spec")),
                        text(item, "/status/phase"),
                    ),
                    ResourceKind::Deployment => {
                        let desired = item.pointer("/spec/replicas").and_then(Value::as_u64);
                        let ready = item
                            .pointer("/status/readyReplicas")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        (
                            container_ports(item.pointer("/spec/template/spec")),
                            desired.map(|d| format!("{ready}/{d} ready")),
                        )
                    }
                },
            };
            Some(KubeItem {
                name,
                ports,
                status,
            })
        })
        .collect();
    parsed.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(parsed)
}

/// Runs `query` (blocks: call on a worker thread).
pub fn lookup(tool: &Tool, query: &KubeQuery) -> Result<Vec<KubeItem>, String> {
    let args: Vec<String> = query.args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    parse_list(&query.what, &run_json(tool, &args, RESOLVE_TIMEOUT)?)
}

/// The kubeconfig files kubectl reads: `KUBECONFIG`'s, else `~/.kube/config`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KubeconfigFiles {
    /// Each file and whether it exists.
    pub files: Vec<(PathBuf, bool)>,
    /// From `KUBECONFIG` (not the default file).
    pub from_env: bool,
}

pub fn kubeconfig_files(env: &KubeEnv, home: &Path) -> KubeconfigFiles {
    match &env.kubeconfig {
        Some(list) => KubeconfigFiles {
            files: std::env::split_paths(list)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| {
                    let exists = p.is_file();
                    (p, exists)
                })
                .collect(),
            from_env: true,
        },
        None => {
            let path = home.join(".kube").join("config");
            let exists = path.is_file();
            KubeconfigFiles {
                files: vec![(path, exists)],
                from_env: false,
            }
        }
    }
}

/// Everything Settings and the status bar show about kubectl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubectlInfo {
    pub kubectl: Result<Kubectl, String>,
    /// `None` while it isn't known (no kubectl).
    pub version: Option<Result<KubectlVersion, String>>,
    pub files: KubeconfigFiles,
    pub view: Option<Result<KubeConfigView, String>>,
}

impl KubectlInfo {
    /// Detects everything (runs kubectl; call off the UI and core threads).
    pub fn check(kubectl: Result<Kubectl, String>, env: &KubeEnv, home: &Path) -> Self {
        let (version, view) = match &kubectl {
            Ok(k) => (Some(version(&k.tool)), Some(config_view(&k.tool))),
            Err(_) => (None, None),
        };
        KubectlInfo {
            kubectl,
            version,
            files: kubeconfig_files(env, home),
            view,
        }
    }

    /// The kubeconfig's contexts, when they could be read.
    pub fn contexts(&self) -> Option<&KubeConfigView> {
        self.view.as_ref().and_then(|v| v.as_ref().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An executable file named `kubectl` in `dir`.
    fn fake_kubectl(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("kubectl{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn the_override_wins_and_never_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let on_path = fake_kubectl(&tmp.path().join("bin"));
        let mine = fake_kubectl(&tmp.path().join("mine"));
        let env = KubeEnv {
            path: tmp.path().join("bin").into_os_string(),
            ..KubeEnv::default()
        };
        let found = locate(Some(&mine), &env).unwrap();
        assert_eq!(
            (found.tool.program, found.source),
            (mine, KubectlSource::Override)
        );
        let missing = tmp.path().join("nope");
        assert_eq!(
            locate(Some(&missing), &env),
            Err(LocateError::OverrideMissing(missing))
        );
        let found = locate(None, &env).unwrap();
        assert_eq!(
            (found.tool.program, found.source),
            (on_path, KubectlSource::Path)
        );
    }

    #[test]
    fn install_folders_come_after_path_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let second = fake_kubectl(&tmp.path().join("rd"));
        fake_kubectl(&tmp.path().join("brew"));
        let env = KubeEnv {
            path: OsString::new(),
            kubeconfig: None,
            folders: vec![
                (tmp.path().join("docker"), "Docker Desktop".into()),
                (tmp.path().join("rd"), "Rancher Desktop".into()),
                (tmp.path().join("brew"), "Homebrew".into()),
            ],
        };
        let found = locate(None, &env).unwrap();
        assert_eq!(found.tool.program, second);
        assert_eq!(
            found.source,
            KubectlSource::Folder("Rancher Desktop".into())
        );
        assert_eq!(
            locate(None, &KubeEnv::default()),
            Err(LocateError::NotFound)
        );
    }

    #[test]
    fn the_folders_per_os() {
        let home = Path::new("home");
        let folders = install_folders(home, Some(OsStr::new("PF")), Some(OsStr::new("LAD")));
        let names: Vec<&str> = folders.iter().map(|(_, n)| n.as_str()).collect();
        if cfg!(windows) {
            assert_eq!(folders[0].0, Path::new(r"PF\Docker\Docker\resources\bin"));
            assert_eq!(folders[1].0, home.join(r".rd\bin"));
            assert_eq!(
                names,
                ["Docker Desktop", "Rancher Desktop", "Rancher Desktop"]
            );
        } else if cfg!(target_os = "macos") {
            assert_eq!(folders[1].0, home.join(".rd/bin"));
            assert!(names.contains(&"Homebrew"));
        } else {
            assert!(folders.is_empty());
        }
    }

    #[test]
    fn macos_path_puts_kubectls_folders_first() {
        let path = std::env::join_paths(["/usr/bin", "/usr/local/bin"]).unwrap();
        let joined = macos_path(Path::new("/k/bin/kubectl"), &path);
        let dirs: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(
            dirs,
            ["/k/bin", "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from)
        );
    }

    #[test]
    fn versions_parse() {
        let json = r#"{"clientVersion":{"major":"1","minor":"31","gitVersion":"v1.31.2",
            "platform":"windows/amd64"},"kustomizeVersion":"v5.4.2"}"#;
        let v = parse_version(json).unwrap();
        assert_eq!(v.describe(), "Client v1.31.2, Kustomize v5.4.2");
        let v = parse_version(r#"{"clientVersion":{"gitVersion":"v1.20.0"}}"#).unwrap();
        assert_eq!(v.describe(), "Client v1.20.0");
        assert!(parse_version("{}").is_err());
        assert!(parse_version("Client Version: v1.31").is_err());
    }

    #[test]
    fn the_kubeconfig_view_parses() {
        let json = r#"{
          "kind": "Config",
          "clusters": [
            {"name": "k3s-lab", "cluster": {"server": "https://10.9.8.7:6443",
              "certificate-authority-data": "DATA+OMITTED"}},
            {"name": "eks-test", "cluster": {"server": "https://abc.eks.example.test"}}
          ],
          "contexts": [
            {"name": "lab", "context": {"cluster": "k3s-lab", "user": "lab-admin"}},
            {"name": "test-eks", "context": {"cluster": "eks-test", "user": "arn-user",
              "namespace": "api"}},
            {"name": "", "context": {}}
          ],
          "users": [{"name": "lab-admin", "user": {"token": "REDACTED"}}],
          "current-context": "lab"
        }"#;
        let view = parse_config_view(json).unwrap();
        assert_eq!(view.current.as_deref(), Some("lab"));
        assert_eq!(view.contexts.len(), 2);
        assert_eq!(
            view.context("lab").unwrap(),
            &KubeContext {
                name: "lab".into(),
                cluster: "k3s-lab".into(),
                server: "https://10.9.8.7:6443".into(),
                user: "lab-admin".into(),
            }
        );
        let empty = parse_config_view(r#"{"current-context":""}"#).unwrap();
        assert_eq!(empty, KubeConfigView::default());
    }

    #[test]
    fn kubeconfig_files_from_kubeconfig_or_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home_config = tmp.path().join(".kube").join("config");
        let files = kubeconfig_files(&KubeEnv::default(), tmp.path());
        assert_eq!(files.files, [(home_config.clone(), false)]);
        assert!(!files.from_env);

        std::fs::create_dir_all(home_config.parent().unwrap()).unwrap();
        std::fs::write(&home_config, "").unwrap();
        let other = tmp.path().join("other");
        let env = KubeEnv {
            kubeconfig: Some(std::env::join_paths([&home_config, &other]).unwrap()),
            ..KubeEnv::default()
        };
        let files = kubeconfig_files(&env, tmp.path());
        assert_eq!(files.files, [(home_config, true), (other, false)]);
        assert!(files.from_env);
    }

    #[test]
    fn lookup_commands() {
        let query = KubeQuery {
            context: "lab".into(),
            what: KubeList::Resources {
                namespace: "api".into(),
                kind: ResourceKind::Service,
            },
        };
        assert_eq!(
            query.args(),
            [
                "--context",
                "lab",
                "--namespace",
                "api",
                "get",
                "services",
                "-o",
                "json",
                "--request-timeout=5s"
            ]
        );
        assert_eq!(query.describe(), "kubectl get services -n api");
        let namespaces = KubeQuery {
            context: String::new(),
            what: KubeList::Namespaces,
        };
        assert_eq!(namespaces.args()[..2], ["get", "namespaces"]);
    }

    /// Fixtures shaped like kubectl's output, with made-up names.
    #[test]
    fn lookups_parse() {
        let namespaces = r#"{"kind":"NamespaceList","items":[
            {"metadata":{"name":"zz-ops"}},{"metadata":{"name":"aa-api"}},{"spec":{}}]}"#;
        let names: Vec<String> = parse_list(&KubeList::Namespaces, namespaces)
            .unwrap()
            .into_iter()
            .map(|i| i.name)
            .collect();
        assert_eq!(names, ["aa-api", "zz-ops"]);

        let resources = |kind| KubeList::Resources {
            namespace: "n".into(),
            kind,
        };
        let services = r#"{"items":[{"metadata":{"name":"web"},"spec":{"ports":[
            {"name":"http","port":80,"targetPort":8080,"protocol":"TCP"},{"port":9090}]}}]}"#;
        let web = &parse_list(&resources(ResourceKind::Service), services).unwrap()[0];
        assert_eq!(
            web.ports,
            [
                KubePort {
                    port: 80,
                    name: Some("http".into())
                },
                KubePort {
                    port: 9090,
                    name: None
                }
            ]
        );
        let pods = r#"{"items":[{"metadata":{"name":"web-0"},"spec":{"containers":[
            {"name":"a","ports":[{"containerPort":8080,"name":"http"}]},{"name":"b"}]},
            "status":{"phase":"Pending"}}]}"#;
        let pod = &parse_list(&resources(ResourceKind::Pod), pods).unwrap()[0];
        assert_eq!(pod.status.as_deref(), Some("Pending"));
        assert_eq!(pod.ports[0].port, 8080);
        let deployments = r#"{"items":[{"metadata":{"name":"web"},"spec":{"replicas":3,
            "template":{"spec":{"containers":[{"ports":[{"containerPort":3000}]}]}}},
            "status":{"readyReplicas":2}},
            {"metadata":{"name":"idle"},"spec":{"replicas":1},"status":{}}]}"#;
        let list = parse_list(&resources(ResourceKind::Deployment), deployments).unwrap();
        assert_eq!(list[0].name, "idle");
        assert_eq!(list[0].status.as_deref(), Some("0/1 ready"));
        assert_eq!(list[1].status.as_deref(), Some("2/3 ready"));
        assert_eq!(list[1].ports[0].port, 3000);
        assert!(parse_list(&KubeList::Namespaces, "{}").is_err());
    }

    #[test]
    fn last_lines() {
        assert_eq!(last_line("a\n\nerror: b \n\n").as_deref(), Some("error: b"));
        assert_eq!(last_line(" \n"), None);
    }
}
