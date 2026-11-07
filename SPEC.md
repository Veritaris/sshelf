# sshelf — Project Specification

sshelf is a cross-platform desktop app (Windows, macOS, Linux) that puts a visual layer over the user's real `~/.ssh/config`: browse hosts, see their effective configuration, edit them safely, launch SSH sessions, and manage port-forwarding tunnels and groups of tunnels.

- Language: Rust
- GUI: egui / eframe
- License: MIT
- Signing: none for now (unsigned builds for all platforms; the macOS `.app` is signed ad hoc, which Apple Silicon needs, not with a Developer ID)
- Release packages: Windows `.7z` (the `.exe` carries its icon and version information); Linux `.tar.gz` with a desktop entry and icon under `share/`; macOS a `.dmg` with `sshelf.app` (the full icon set as `sshelf.icns`, drawn by `examples/iconset.rs`; `Info.plist` from `assets/macos/`, bundle id `me.veritaris.sshelf`) and an Applications shortcut, laid out by `dmgbuild` (`assets/macos/dmg-settings.py`: a 640 × 400 window, 128 px icons, sshelf left and Applications right) over a background from `examples/dmg_background.rs` (light, so Finder's black icon labels stay readable; a Magenta arrow and "Drag sshelf to Applications"; 1× and @2x joined by `tiffutil`)

This document is the source of truth for scope and design. It describes the **MVP**, the **iterations** after it, and the **final shape**. Implement milestone by milestone; do not pull later-milestone features forward unless this document is updated first.

---

## 1. Core principles (hard rules)

1. **`ssh_config` is the source of truth for hosts.** The app never keeps its own list of connectable machines. A host exists if and only if it exists in the user's ssh config (including `Include`d files).
2. **Everything else lives in the app's own data folder**, linked to hosts by alias. If app data references an alias that no longer exists, that is an *orphan* and is surfaced to the user (see §6), never silently deleted.
3. **Lossless config editing.** Lines the user did not change are written back byte-for-byte: comments, blank lines, indentation, tabs, CRLF, ordering, unknown directives — all preserved.
4. **Shell out to the system `ssh`.** No SSH library for connections or tunnels. This keeps agent, `ProxyJump`, `ProxyCommand`, `known_hosts` and all user config behavior identical to the command line. kubectl tunnels (M13) follow the same rule: they run the user's `kubectl`, never a Kubernetes client library, so kubeconfig, contexts and credential plugins behave as on the command line.
5. **Never read existing private keys.** The app only references `IdentityFile` paths and defers to the agent. It never stores passwords or passphrases. It *may* generate new keys (milestone 7); key material exists only in memory during generation and is zeroized afterward.
6. **Effective configuration comes from `ssh -G <alias>`**, not from reimplementing ssh's resolution rules.
7. **Never bypass host key checks silently.** Unknown and changed host keys always go through an explicit user confirmation (see §9).
8. **Safe writes.** Every write to a user file is atomic (write temp file, then rename). Before the first write to an ssh config file in a session, a timestamped backup is created.
9. **Small, verifiable steps.** Each milestone ends in a state that can be verified by hand and by tests before moving on.

### Non-goals

- Embedded terminal emulator (sessions open in the user's terminal app).
- Cloud sync, accounts, or any network service of our own. The one request sshelf makes itself is the update check (§10 *Update check*, M14): it asks GitHub's public API for the latest release, can be turned off, and sends nothing about the user.
- Downloading or installing updates.
- Password storage or password-based auth automation.
- PuTTY/plink, WSL, Cygwin, MSYS2 or Git-for-Windows ssh support (see §8).
- Features depending on `ControlMaster` / `ControlPersist` (unsupported by Windows OpenSSH).

---

## 2. Architecture

Cargo workspace with UI-independent core crates:

| Crate | Responsibility | Depends on egui? |
|---|---|---|
| `sshconf` | Lossless `ssh_config` parser/editor (CST), `Include` resolution, write-back | No |
| `sshelf-core` | App data model (metadata, tunnels, groups, settings, history), storage + migrations, ssh binary discovery, `ssh -G` resolution, host key checks, tunnel supervisor; kubectl discovery and lookups (M13) | No |
| `sshelf` (app) | egui/eframe frontend; renders state, sends commands to core | Yes |
| `fake-ssh` (lib + test bin) | Scriptable stand-in for `ssh`: used by supervisor tests, and (M10) by the onboarding tour, embedded in the app as `sshelf --fake-ssh`; (M13) also a stand-in for `kubectl` (`--as kubectl`) | No |

Rules:
- The UI never spawns processes or touches files directly; it sends commands to core and renders state/events from core.
- The **supervisor runs independently of the UI loop** (own thread or async runtime). eframe stops calling `update()` when the window is hidden, and the tray (milestone 6) must keep working then. The UI is just one subscriber to supervisor events.
- Core → UI communication: a channel of events; the worker that emits an event calls `egui::Context::request_repaint()`. No polling timers, no idle CPU burn.
- Code style: follow the owner's reference Rust + egui projects listed in `CLAUDE.md`.
- CI: GitHub Actions building and testing on Windows, macOS and Linux, based on the workflows of those reference projects. No signing step.

Suggested crates (verify fitness before adopting): `directories` (platform config dirs), `serde` + `toml`, `notify` (file watching), `tray-icon` (tray, milestone 6), `ssh-key` from RustCrypto (key generation, milestone 7), `zeroize`.

---

## 3. App data storage

Location: the platform config directory for `sshelf` (via `directories`). Human-readable TOML files the user can inspect and version:

| File | Contents |
|---|---|
| `settings.toml` | ssh binary override, terminal template, close behavior choice, UI prefs, onboarding tour state (`tour`, M10), streamer mode (`streamer_mode`, `streamer_hide_aliases`, M11), kubectl override (`kubectl_path`, M13), update check (`update_check`, `update_check_noted`, M14) |
| `hosts.toml` | Per-host metadata keyed by alias: tags, notes, favorite, color, display order |
| `tunnels.toml` | Tunnel definitions and groups (version 2 from M13, for kubectl tunnels) |
| `history.toml` | Recent connections launched from the app (for the tray menu) |
| `updates.toml` | The update check's state (M14): last check time and result, ETag, the latest release (version, URL, date), the skipped version. Written by sshelf only |
| `running.toml` | The tunnels running now: their ssh process (pid, start time, arguments) and owners. Written by the supervisor whenever that changes, empty after a clean quit; read once at start to take tunnels over after a crash (§7 *Surviving a crash*, M12) |

Rules:
- **Every file has a top-level `version` integer from the very first commit.**
- On load, files with an older version are migrated step by step (`v1 → v2 → …`). Before writing a migrated file, the original is backed up next to it.
- A file with a *newer* version than the app understands is never overwritten; the app shows an error and runs read-only for that data.
- The same read-only treatment applies to a file that is not valid TOML, has no `version` (or a non-positive-integer one), or fails to migrate: it is shown where possible and never written. A missing file means defaults.
- Migration backups are named `<file>.bak-v<old version>` (an existing backup is never overwritten).
- All writes are atomic and only ever target files directly inside the app data dir.

---

## 4. ssh_config handling (`sshconf`)

### Parser model

A line-oriented concrete syntax tree. Each line is one of:
`Blank`, `Comment`, `HostHeader`, `MatchHeader`, `Include`, `Directive`, `Invalid`.
Each node stores the **original raw text** plus parsed key/value(s), the **source file**, and its **position**. Blocks group a header with its following lines.

`Invalid` is a line ssh would reject (unterminated quote, `Host`/`Match`/`Include` without arguments, empty host pattern, missing keyword). It is kept verbatim, round-trips like any other line and is reported as an error diagnostic. Syntax problems never make parsing fail; only I/O errors and non-UTF-8 files do (a non-UTF-8 file is shown as an error and never written). A UTF-8 BOM is preserved. Lines split on `\n`; `\r\n` is recorded per line, a lone `\r` stays in the line text.

Tokenization follows OpenSSH `readconf.c` (verified against OpenSSH_for_Windows 9.5p1 with `ssh -G`):
- Trailing ` \t\r\f` is ignored for parsing.
- Keyword, then whitespace and/or at most one `=`.
- Arguments split like `argv_split`: `"…"`/`'…'` quotes anywhere in a word, escapes `\"`, `\'`, `\\`, `\ `; a `#` at the start of a word begins a trailing comment (OpenSSH ≥ 8.7 semantics).
- `ProxyCommand`, `RemoteCommand`, `LocalCommand`, `KnownHostsCommand` take the raw rest of the line as one value (`#` and quotes are part of the command).

Must handle:
- Case-insensitive keywords, `Key value` and `Key=value` syntax, tabs, trailing whitespace, CRLF and LF.
- Quoted values, multi-value directives, comments at any position.
- `Host a b c` multi-alias headers, negated patterns (`!foo`), wildcards (`*`, `?`).
- `Match` blocks (preserved and displayed; not edited structurally).
- `Include` with globs, relative paths resolved against `~/.ssh`, `Include` inside a `Host` block (conditional), recursion limits.

`Include` resolution:
- Several paths per line, each glob-expanded in sorted order; `*` does not match a leading `.`; directories are skipped. A path matching nothing is skipped silently (info diagnostic), like ssh.
- `~/` expands to the home directory; relative paths resolve against the ssh dir (`~/.ssh`), not the including file. `~user`, `%` tokens and `${VAR}` are not expanded (ssh 9.5p1 uses them literally); a warning is shown.
- Nesting is limited to 16 levels (ssh's `READCONF_MAX_DEPTH`); include cycles are reported and not followed. A file included twice is parsed once.
- Headers reached through an `Include` inside a `Host`/`Match` block are marked **conditional**.

### Host list semantics

- Concrete aliases (no wildcards) are listed as **hosts**.
- Blocks with wildcard patterns (`Host *`, `Host *.prod`) and `Match` blocks are listed separately as **patterns**; they are not connectable and cannot carry metadata.
- A multi-alias block (`Host prod-db db1`) is one host; metadata links by the **first alias**, but lookups resolve any alias.
- A block mixing concrete aliases and wildcards (`Host web1 *.corp`) is a host with only its concrete aliases, flagged as also having wildcards. Negated patterns never make a block a host; a block with only negations is a pattern.
- Every concrete alias belongs to exactly one host: the first block in evaluation order that names it (the one ssh applies first). A later block naming an already-owned alias creates no second entry for it; it is listed as **"also configured at file:line"** on the owner (ssh merges such blocks, first value wins; the `ssh -G` panel shows the merged result). A later block keeps only its new aliases (`Host a` … `Host b a` → hosts `a` and `b`); a block with no new aliases creates no entry. Alias lookup is case-sensitive, like ssh's `Host` matching.
- Each host shows which file it comes from.

### Effective config

The read-only "resolved" panel shows `ssh -G <alias>` output for the selected host. This is also used to obtain `hostname`, `port`, `user`, `hostkeyalias`, `userknownhostsfile`, `proxyjump`, etc. for all other features.

### Writing (milestone 5)

- Edit blocks **in place in whichever file they live**.
- New hosts go to a user-chosen file (default: main config). Within the file, insert **before the first wildcard `Host *` / `Match all` block**, never simply appended at the end — otherwise wildcard defaults override the new host's values because the first match wins.
  - "Wildcard block" means a `Host` line with a bare, non-negated `*` pattern, or `Match all`; narrower wildcards (`Host *.corp`) do not count. The new block also goes above the comment lines directly over that block, and is separated by blank lines. Without such a block it is appended.
  - A missing main config is created by the first new host (mode `600` on Unix).
- Blocks from files that are not writable are read-only, with the reason shown (the file has the read-only attribute or is not a regular file). Only files of the loaded config set are ever written.
- Only changed lines are rewritten; everything else is byte-identical. A changed line keeps the line ending of the line it replaces; added lines use the file's usual ending; whether the file ends with a newline is kept. A changed directive keeps its indentation, keyword spelling, separator and trailing comment.
- Backup before first write per session; atomic writes.
  - Backups go to `backups/<file>.<time>.bak` in the app data dir, **not** next to the file: a backup in e.g. `~/.ssh/conf.d/` could be matched by an `Include conf.d/*` glob and loaded by ssh.
  - A symlinked config file is written through to its target (the link stays); on Unix the file keeps its permissions.

### External changes

- Watch **directories**, not files: the parent directory of every loaded file, plus the deepest non-wildcard directory of every `Include` glob (e.g. `~/.ssh/conf.d` for `Include conf.d/*.conf`; recursive when a directory component has wildcards). This catches newly added files that match an `Include` and editors that save by renaming a temp file over the original. A directory that does not exist yet is watched through its nearest existing ancestor. `sshconf` computes the list (`ConfigSet::watch_dirs`); the watcher itself lives in `sshelf-core`.
- On a change, debounce (~200 ms), then reload the whole config set.
- If the file changed on disk while there are unsaved in-app edits, do not overwrite: show the conflict (diff) and let the user choose.

---

## 5. Host details and editing

- Structured form for common directives: `HostName`, `User`, `Port`, `IdentityFile`, `IdentitiesOnly`, `ProxyJump`, `LocalForward`, `RemoteForward`, `DynamicForward`, `ServerAliveInterval`, `ForwardAgent`.
- A raw text editor for the whole block, for everything else.
- Both edit the same CST node, so they stay in sync.
- **Validation:** parse with `sshconf`, then write the candidate config to a temp file and run `ssh -G -F <tempfile> <alias>`. Map ssh's errors (with line numbers) back to the editor. Saving is blocked while validation fails.
  - `sshconf` problems (invalid lines) show while typing; `ssh -G -F` runs on *Check* and on *Save* (it starts a process, so not on every keystroke).
  - ssh reports errors for every block of the file (`<file> line N: …`, OpenSSH 9.5p1). Errors it already reports for the file as it was do not block saving, so one old bad line elsewhere never makes every edit unsaveable.
  - The candidate is the edited file alone: an `Include`d file is validated as if it were the whole config. That catches its syntax and option errors, not effects of the files around it; the editor says so.
  - On save the file is read again: if it no longer matches the bytes the edit started from, nothing is written and the conflict is shown (§4 "External changes").
- Renaming a host updates the alias in the config and all app data referencing it, as one operation.
  - *Rename…* changes one alias on the host's own `Host` line (other aliases, patterns and the line's comment stay), validates it like any edit, then moves `hosts.toml`, the tunnels' `host` and `history.toml` entries from the old alias to the new one. If an app data file cannot be saved, the files already saved and the config are put back.
  - The new name must be one plain word (no spaces, `* ? ! ,`, quotes, no leading `-`/`#`) that no host already has. Renaming is refused while a tunnel on that alias runs.
  - Changing the `Host` line in the raw editor does not touch app data; entries for the old alias then show up as orphans (§6).

---

## 6. Metadata and orphans

- Metadata (`hosts.toml`) and tunnels (`tunnels.toml`) reference hosts by alias; groups reference tunnels by stable `id`.
- **Orphans:** metadata for a missing alias, a tunnel whose host alias is missing, a group referencing a missing tunnel.
- UI: a small warning indicator with a counter (e.g. in the sidebar/status bar).
  - **Hover:** tooltip listing what is missing.
  - **Click:** popover with actions per orphan: *Relink to…* (pick an existing alias/tunnel), *Delete*.
- Orphans are never deleted automatically.
- Metadata links to a host through any of its aliases; new entries use the primary alias and existing keys are never renamed silently. A second entry resolving to a host that already has metadata under another alias is also an orphan (*shadowed*).
- *Relink to…* only offers hosts without metadata of their own (relinking never merges or overwrites) and stores the entry under the target's primary alias. *Delete* asks for confirmation once.
- A tunnel with a missing host can be relinked to any existing host (tunnels may share hosts) or deleted. A group entry naming a missing tunnel can be relinked to another tunnel not yet in the group, or removed from the group.

---

## 7. Tunnels and groups

### Model

```toml
version = 1

[[tunnel]]
id = "t-3f9a1c07"         # stable, generated; never derived from the name
name = "k3s API"
host = "k3s-master"       # ssh_config alias
forwards = [
  { kind = "L", bind = "127.0.0.1", listen = 6443, target = "localhost:6443" },
]

[[tunnel]]
id = "t-81d2e4aa"
name = "Prod Postgres"
host = "bastion"
forwards = [
  { kind = "L", bind = "127.0.0.1", listen = 5432, target = "db.internal:5432" },
  { kind = "R", bind = "127.0.0.1", listen = 8080, target = "localhost:3000" },
  { kind = "D", bind = "127.0.0.1", listen = 1080 },
]

[[group]]
id = "g-5b07c9e1"
name = "Work on prod"
tunnels = ["t-3f9a1c07", "t-81d2e4aa"]
```

- **Tunnel** = one host alias + N forwards = **one `ssh` process**.
- Forward kinds: `L` (local), `R` (remote), `D` (dynamic/SOCKS). Every forward has `bind` (listen address; local for `L`/`D`, on the server for `R`), `listen` (port) and, for `L`/`R`, `target` (`host:port` to connect to).
- Ids are random (`t-xxxxxxxx`, `g-xxxxxxxx`).
- Default bind address `127.0.0.1`. Binding to `0.0.0.0` or another non-loopback address requires an explicit choice and shows a warning.
  - As built: since the bind field starts at `127.0.0.1`, a non-loopback bind only exists when the user types one; that is the choice. The field then gets a yellow outline and the warning says who can reach the port.
- **Group** = named set of tunnel ids. Forwards are defined only on tunnels, never inline in groups.

### Process

Spawned command:
```
ssh -v -N -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -o BatchMode=yes <forwards> -- <alias>
```
- `-v` lets the supervisor see when the connection is up: the tunnel is *Running* once ssh logs `Entering interactive session.` (forward setup failures make ssh exit because of `ExitOnForwardFailure`). Debug lines are hidden from the shown log unless the user asks for them.
- Windows: spawn with `CREATE_NO_WINDOW`; kill the whole process tree on stop.
- Keep the last N lines of stderr per tunnel, viewable in the UI.
- Before starting, pre-check local ports by attempting to bind a `TcpListener`; report conflicts without spawning.

### Supervisor semantics

- **Owners and reference counting (from day one):** a tunnel's process runs while at least one owner holds it. Owners are: a manual start, or a group. Starting the same tunnel twice never spawns a second process.
- **Start group:** pre-check all ports first; if any conflict, report it and start nothing. Otherwise start tunnels in parallel.
- **Partial failure:** remaining tunnels keep running; the group shows **degraded** (e.g. 2/3) with a *Retry failed* action.
- **Stop group:** releases the group's ownership; a tunnel stops only when no owners remain. If a tunnel stays up, show why ("still used by: Work on staging").
- States per tunnel: `Stopped`, `Starting`, `Running`, `Stopping`, `Failed(reason)`, plus host key / auth sub-states (§9). `Failed` keeps the tunnel's owners and shows ssh's last non-debug stderr line; starting it (or its group) again retries it. A start that arrives while a tunnel is `Stopping` runs once the old process has exited, so the two never compete for a port.
- The last 200 stderr lines per tunnel are kept in memory (not persisted).
- Tunnels stop when the app quits (no detached background processes). After a crash, see *Surviving a crash*.

### Surviving a crash (milestone 12)

- **Windows:** tunnels run in a Job Object with "kill on close", so if sshelf dies the OS ends them too. Nothing is left to take over.
- **macOS / Linux:** each tunnel leads its own process group, which outlives a crash. The supervisor records what runs in `running.toml` (§3): tunnel id, pid, the process's start time, ssh's arguments and the owners. It is written atomically whenever that changes; a clean quit leaves it empty.
- **At start,** before anything else starts (start-on-launch groups included), each recorded tunnel is taken over only if:
  - its process is alive with the recorded start time, so a reused pid never matches;
  - the tunnel still exists and its ssh arguments now are the recorded ones (an edited tunnel doesn't match).

  Anything else is dropped from the file and never killed.
- **A taken-over tunnel** is *Running* with its recorded owners, and *Stop* ends its process group as usual.
  - Its earlier output went to the sshelf that crashed: the log starts with "Taken over after sshelf restarted (pid N); earlier output isn't available".
  - Its exit is watched without polling: Linux `pidfd` (a 1 s check on kernels before 5.3), macOS `kqueue`. It is then *Failed* with "ssh exited", and auto-reconnect applies as usual; the new run is a normal child with a log.
- **Toast:** "Took over N tunnels that kept running".
- **Not covered:** the tour's sandbox keeps no record; ssh processes sshelf didn't record stay "port in use".

### kubectl tunnels (milestone 13)

A second tunnel type: `kubectl port-forward` to a pod, service or deployment, next to ssh tunnels and run by the same supervisor. UI: §10 *kubectl tunnels*.

```toml
version = 2

[[tunnel]]
id = "t-5c1e9b20"
name = "web api"
type = "kubectl"          # missing means "ssh"
context = "dev-k3s"       # missing means the current context, looked up at each start
namespace = "api"
resource = "service/web"  # pod/<name>, service/<name> or deployment/<name>
address = "127.0.0.1"     # kubectl --address, for every forward
forwards = [
  { listen = 18080, target = "80" },
  { listen = 19090, target = "metrics" },
]
auto_reconnect = true
```

- **Model:** a kubectl tunnel is one context, one namespace, one resource and N forwards = **one `kubectl` process**. A forward is a local port and a port on the resource: a number, or a port name, passed as written. There are no `R` / `D` forwards and no remote host: kubectl forwards to the pod itself (for a service or deployment, kubectl picks one pod).
  - `address` defaults to `127.0.0.1`; a non-loopback address gets the same warning as an ssh bind (§7 *Model*).
  - `namespace` and `resource` are required. An empty `context` follows `kubectl config use-context`; a set one is pinned with `--context`.
  - Ids, owners, groups and `auto_reconnect` work as for ssh tunnels. Groups mix both types freely.
- **File version:** `tunnels.toml` becomes version 2. The v1 → v2 migration changes nothing but the version (with the usual backup, §3). It exists so an older sshelf treats a file with kubectl tunnels as newer and read-only, instead of misreading them as broken ssh tunnels.
- **Command:**
  ```
  kubectl [--context <context>] --namespace <ns> port-forward <kind>/<name> <listen>:<target>… --address <address>
  ```
  - Spawned like ssh (§7 *Process*): `CREATE_NO_WINDOW` and the Job Object on Windows, its own process group on macOS / Linux, stdin closed, local ports pre-checked before spawning.
  - **Environment:** sshelf's own, so `KUBECONFIG` and credential plugins work as in a terminal. On macOS (an app started from Finder has a short `PATH`), `PATH` for kubectl gets kubectl's own folder, `/opt/homebrew/bin` and `/usr/local/bin` added, so exec plugins such as cloud CLIs are found.
- **Running:** kubectl prints `Forwarding from <address>:<listen> -> <target>` on stdout for each forward. The tunnel is *Starting* until every forward has printed it, then *Running*. stdout and stderr both go to the log (200 lines, as for ssh); `Handling connection for <port>` lines are hidden unless *show debug* is on.
- **Drops:** a lost connection to the pod (pod restarted or deleted, cluster connection reset) makes kubectl exit. If a kubectl version keeps running after printing `lost connection to pod`, the supervisor ends it and treats it the same. With *Reconnect* on, the usual backoff applies (§12 M8: 2 s … 60 s); for a service or deployment the new process picks a new pod.
- **Failures** (classified from kubectl's last error line; the friendly line, kubectl's own line and the fix are shown as for ssh):

  | kubectl says | Shown as | Fix | Reconnects |
  |---|---|---|---|
  | `Error from server (NotFound): pods "…" not found` (also services, deployments) | The pod is gone / The service is gone … | *Pick another pod* (opens the editor on *Forward to*) | no |
  | `error: You must be logged in to the server (Unauthorized)`, or an exec plugin error | Not logged in to `<context>` | "Log in with your cluster's tool in a terminal, then retry." · *Retry* | no |
  | `Unable to connect to the server: …` | Can't reach the cluster | *Retry* | yes |
  | `unable to listen on any of the requested ports` | Local port N is in use | *Edit* | yes (like a taken port for ssh) |
  | `error: context "…" does not exist` | Context not in your kubeconfig | *Edit* | no |
  | kubectl not found (no binary) | kubectl not found | *Settings › kubectl* | no |
  | anything else | kubectl exited | *Show log* | yes |
  - As built: "every forward" counts local ports: kubectl prints a line per address family (`127.0.0.1` and `[::1]` for `--address localhost`), and each port counts once. The log starts with `starting kubectl (pid N)`. Classification looks for `(NotFound)`; `You must be logged in`, `(Unauthorized)` or `getting credentials` (an exec plugin); `error: context "`; `unable to listen on any of the requested ports`; `Unable to connect to the server`, in that order. kubectl's own last line is the reason shown. Only *Can't reach the cluster*, *Local port in use* and *kubectl exited* reconnect.
- **Crash survival (M12):** `running.toml` records the program (`ssh` or `kubectl`) with its arguments, so kubectl tunnels are taken over the same way; a missing `program` means `ssh`.
  - As built: a record is taken over only if its program is the tunnel's type now. A taken-over kubectl that dies reads "kubectl exited".
- **Lookups for the editor** run in core on a worker (the UI never spawns), each with `--request-timeout=5s`, and arrive as events:
  - contexts: `kubectl config view -o json` (local; kubectl redacts secrets): context names, each one's cluster and server, the current context;
  - namespaces: `kubectl [--context] get namespaces -o json`;
  - resources: `kubectl [--context] --namespace <ns> get pods|services|deployments -o json` → names with their ports (service `spec.ports`; container ports of the pod or the deployment's template), pod phase, deployment ready / desired.
  - A failed lookup (RBAC `Forbidden`, unreachable cluster) is shown in the list with kubectl's reason; a typed value is always accepted and checked when the tunnel starts.
- **Not in scope:** starting or logging in to clusters, editing kubeconfig, `R` / `D` forwards, health checks, kubectl tunnels in the onboarding tour.
- **Testing:** `fake-ssh --as kubectl` prints `Forwarding from` lines then waits, or prints a given error and exits, or prints `lost connection to pod` (exiting or not). Supervisor tests cover Starting → Running, each failure kind, the reconnect rules and the drop handling on all three OSes. No test talks to a real cluster.

### Testing

- `fake-ssh` is a small Rust binary in the workspace that behaves according to arguments/env: sleep forever, exit with code X after Y ms, print given stderr, simulate host key failure, etc.
- Supervisor tests run against `fake-ssh` on all three OSes in CI.
- Tests that spawn processes live in `crates/fake-ssh/tests/` with `sshelf-core` as a dev-dependency, and get the binary path from `env!("CARGO_BIN_EXE_fake-ssh")` (Cargo only provides it to tests of the package that defines the binary). The supervisor takes the ssh binary path as configuration (§8 override), so tests pass the `fake-ssh` path. Supervisor unit tests that spawn nothing stay in `sshelf-core`.

---

## 8. ssh binary selection

- **Windows:** the built-in OpenSSH at `C:\Windows\System32\OpenSSH\ssh.exe`, located by explicit path (not `PATH`). Companion tools (`ssh-add`, `ssh-keygen`, `ssh-keyscan`) are taken from the same directory.
- **macOS / Linux:** system `ssh` from `PATH`.
- Settings allow overriding the path on all platforms. Companion tools are taken from the overridden binary's directory; if one is missing there, the system's is used (Windows' built-in OpenSSH, else `PATH`).
- Settings show: selected binary, `ssh -V` output, agent reachability and number of loaded keys (`ssh-add -l`).
- **Windows agent:** detect when the `ssh-agent` Windows service is stopped or disabled and explain how to enable it (requires admin; the app does not do it itself).
- **Tour sandbox (M10):** an ssh tool is a program plus prefix arguments and environment, not just a path; companions are found the same way. Only the onboarding tour's sandbox Core sets one: `[<sshelf executable>, "--fake-ssh"]` with `FAKE_SSH_SCENARIOS` pointing at the sample, and `--as ssh-add` for the agent tool. It never comes from `settings.toml` and Settings can't select it. The tour's toasts and fake servers are the only place §1 rule 4 does not apply, since nothing connects (§10 *Onboarding tour*).
- **Auth under `BatchMode`:** when a tunnel fails because a key is not available in the agent, show a clear message ("key not loaded in agent") with a hint, not a raw error.
- **kubectl (M13):** found on `PATH`; then, when not there, the usual install folders: Docker Desktop and Rancher Desktop on Windows and macOS (`~/.rd/bin` and the apps' `resources/bin`), and on macOS `/opt/homebrew/bin` and `/usr/local/bin` (an app started from Finder does not get the shell's `PATH`). Settings can override the path (`kubectl_path`, missing = detect). Detection runs at start, on *Refresh* and when the override is saved.
  - Settings show: the binary and where it came from, `kubectl version --client -o json` (client and Kustomize versions), the kubeconfig files (`KUBECONFIG` or `~/.kube/config`) with the number of contexts, and the current context. Nothing there contacts a cluster.
  - kubectl is optional: without it ssh tunnels and everything else work; kubectl tunnels can be edited and saved but not started.
  - As built: the install folders are, on Windows, `%ProgramFiles%\Docker\Docker\resources\bin`, `~\.rd\bin` and `%LOCALAPPDATA%\Programs\Rancher Desktop\resources\resources\win32\bin`; on macOS `/Applications/Docker.app/Contents/Resources/bin`, `~/.rd/bin`, `/Applications/Rancher Desktop.app/Contents/Resources/resources/darwin/bin`, `/opt/homebrew/bin`, `/usr/local/bin`; Linux uses `PATH` only. The source chip names the folder's app ("Docker Desktop", "Homebrew"). A missing override is "the file set below doesn't exist", with no fallback (like ssh). Settings' *Refresh* re-detects kubectl alone; saving Settings re-detects both.
  - As built (tests): `PATH`, `KUBECONFIG` and the install folders are read from the environment once, when sshelf finds its real paths (`Paths::detect`, `KubeEnv`). Every other `Paths` has none, so no test can find the user's kubectl or kubeconfig.

---

## 9. Host key verification

Before starting a tunnel, determine the host key state using values from `ssh -G` (`hostname`, `port`, `hostkeyalias`, `userknownhostsfile`) and `ssh-keygen -F <host> -f <known_hosts>`. Remember non-22 ports are stored as `[host]:port`.

### Unknown key → confirmation modal

1. Fetch the key through ssh itself (works with `ProxyJump`/`ProxyCommand`, unlike `ssh-keyscan`):
   `ssh -o UserKnownHostsFile=<temp> -o StrictHostKeyChecking=accept-new -o BatchMode=yes <alias> exit`
   Key exchange happens before auth, so the temp file receives the key even if auth fails.
2. Show a modal: host, port, key type, SHA256 fingerprint, and a hint to compare it with the server/provider (`ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` on the server).
3. On confirm, append exactly those lines from the temp file to the real `known_hosts` (format already matches the user's `HashKnownHosts` setting).
4. Start the tunnel with normal strict checking — if the server now presents a different key, ssh refuses.

**Jump chains:** hops must be verified in order (bastion first). Command-line `-o` options may not propagate to the jump connection — **verify this behavior against a real `ProxyJump` setup during implementation.** Show one modal per unknown hop.

### Changed key → replace (with strong confirmation)

- Warning-styled modal, visually distinct from the unknown-key modal: old vs new fingerprint side by side, explanation (server reinstalled **or** connection intercepted).
- *Replace* is not the default button and requires an explicit extra action (checkbox "I know this server was reinstalled" or typing the host name).
  - As built (M9): typing the host's alias; *Don't connect* is the safe button (grey and focused, per §10 *Destructive confirmations*) and *Replace key* uses Dark Red. A key only in a system-wide `known_hosts` can't be replaced from the app; the dialog says an administrator has to.
- Replace = `ssh-keygen -R <host> -f <known_hosts>` (creates `.old` backup), then the unknown-key flow above to fetch and store the new key.
- Never replace automatically.

### How sshelf applies this

- Every tunnel or group start first checks, without connecting, each host on the way (jump hosts first, then the destination): `ssh -G` for the lookup name and known-hosts files, `ssh-keygen -F` in each user and global file. The first unknown key is fetched and shown; *Accept* stores it and continues with the next host, *Reject* starts nothing (for a group: none of its tunnels). The fetch runs the command above, which logs in and runs `exit` if the key is accepted for authentication.
- *Connect* runs the same check first: an unknown key shows the same dialog ("connecting to web waits for it"), and the terminal opens once it is accepted (*Reject*: no terminal). If the check itself can't run, the terminal opens anyway and ssh reports the problem. A *changed* key can't be seen without contacting the server, so Connect leaves that to ssh in the terminal.
- The host detail's health banner offers *Review host key…* (key not known yet) and *Review changed key…* (key changed). Both run a host review: the unknown-key check above (dialog as for a start), then, if every key is known, the changed-key comparison below (it connects, like the fetch). A changed key shows the same changed-key dialog ("host “web”"); *Replace key* only replaces it. When everything matches, a notice says so.
- A changed key can only be seen when ssh connects: the tunnel fails, and its failure is classified from ssh's messages (host key changed / unknown, authentication, other). *Review changed key…* walks the same chain, fetches each host's key (of the stored key's type when the server offers it) and shows the first host whose key no longer matches.
- `known_hosts` files are written only by these flows: appends are atomic (temp file + rename), and each file gets a timestamped backup (`known_hosts.sshelf-<time>.bak`) before its first change in a session; `ssh-keygen -R` additionally keeps `.old`. The global (system) files are never written; a key stored only there cannot be replaced from the app.
- Interactive sessions (Connect) are not checked: ssh asks about host keys in the terminal itself.

---

## 10. UI

### Layout

- **Sidebar:** an icon rail with Hosts · Tunnels · Groups · Settings, plus the orphan warning indicator (see *Visual design*).
- **Main area:** list + detail pane.
- **Feedback:** successes (settings saved, host saved/added/renamed, terminal opened) show as a toast in the bottom-right corner that fades after ~3 s; errors stay in the status bar until dismissed.
  - **Every toast has a close button** (M14): Lucide `x` 14 in a 24 px ghost button at the right end, secondary grey, hover background `#2C313A` and strong text (80 ms). Closing removes the toast at once and the others close the gap; hovering a toast still pauses its fade. Sticky toasts (the update toasts, §10 *Update check*) close only this way or through their actions.
  - Hosts: list (search; filter chips *All*, *Favorites* and one per tag, one filter at a time; *New host*) with a separate collapsible *Patterns* section; detail pane with source file, metadata, actions (Connect, New tunnel, Edit, Rename…), and the `ssh -G` resolved panel.
  - *Edit* replaces the detail pane with the editor (§5): *Form* and *Raw text* tabs over the same block, the problems found, the changed lines as they will be written, and Save / Check / Discard. Picking another host closes an editor without changes. *New host* opens the same editor with an alias field and the target file picker (§4).
  - Tunnels: list with status, start/stop, forwards, stderr log.
    - As built in M9: two-line rows (state mark, name; "Running · 2 h 14 m · L 6443", "Running · group “x”", "Failed · key not accepted", a reconnect countdown) with the state's one action: *Cancel* (a manual start), *Stop* (manual), *Start* (stopped; a tunnel running for a group only has no row action, and its detail and menu offer *Keep running*, which adds a manual owner so it stays up when the group stops), *Retry*, *Review…* (changed host key). Right-click: Start, Stop, Edit, Copy ssh command, Delete…. The detail has a status chip with the uptime, "via <host>" (a link to the host) and the owners, a red failure banner per failure kind (plain words, the hint, *Review changed key…* in Dark Red, *Show log*), the forwards table, and the log with `HH:MM:SS` and a *show debug* switch. Deleting asks in a dialog (*Cancel* grey and focused, *Delete* Dark Red). Switching a forward's kind keeps what was typed (a target set aside while it is *D* comes back). The editor has Name, the host as a searchable dropdown, the *Reconnect* switch, forwards as cards of separate fields (kind as an L / R / D segmented control; a non-loopback bind gets a yellow outline and the warning), the ssh command it will run, and *Save* / *Save and start* / *Cancel*.
  - Groups: list + detail pane. Each row has the group's switch and its aggregate status (running / degraded / stopped, with counts); the detail shows the per-tunnel states.
    - As built in M9: rows "running · 2/2" / "degraded · 1/2" / "stopped" with the switch (on a selected row the off track is the outline colour, so it stays visible). The detail has the name, a "starts with sshelf" chip, a status chip, *Retry failed* (degraded), *Edit* (disabled while running, with the reason), *Start group* / *Stop group*; the tunnels as rows (mark, name, host, first forward, state; a click shows the tunnel in Tunnels; a failed one has a small ghost retry button after "Failed", the same as *Retry failed*) with a failure strip (ssh's line, the reconnect countdown, the hint); *Add tunnel* opens the editor. A card says which tunnels other groups share ("stays up while that runs"), or, once stopped, which still run for other owners. *New group* is disabled with the reason while no tunnels exist. The editor has Name, the *Start with sshelf* switch and the tunnels as the multi-select dropdown, *Save* / *Cancel* and *Delete group…*, which asks in a dialog (*Cancel* grey and focused). A missing tunnel's id stays in the group until the orphans view relinks or removes it.
  - Settings (as built in M9): the title with *Revert* / *Save* (disabled with the reason while nothing changed or `settings.toml` is read-only), then two columns of cards. *Appearance*: host list style and card grouping as segmented controls, the *Reduce motion* switch. *Connect*: the template, its problem, the placeholders, the detected terminal and *Reset to default*. *Window & tray*: closing (dropdown), the tray menu (segmented), whether a tray exists. *ssh*: binary and its source, version, one *Agent* line (the Windows service while it isn't running, else the agent's key count), the override, and while the service is stopped the admin command with *Copy command*. *Files*: paths cut in the middle, and the three files' states with details on hover. Every setting, including the appearance ones, waits for *Save*; the cards' *Group by* in the hosts view still saves at once.
  - Dialogs (as built in M9): every modal has the `rgba(24,26,31,.72)` overlay, a `#21252B` body, 16 px corners and a 1 px outline. The unknown-key dialog: a blue key tile, "First connection to <host>" and what waits for it, the jump steps (the hop asked about, then the final host; "jump hosts first"), host / port (with "stored as [host]:port" off port 22) / key type / fingerprint on an inset card, the compare command with *Copy*, "Accepting adds N lines to <known_hosts> (backed up first)", *Reject* and *Accept and continue*. The changed-key dialog: the red outline and header band, stored and presented keys side by side (the presented side outlined red), the compare command, the typed confirmation, and what replacing runs. The close dialog: "N tunnels are running", the running tunnels with their first port (up to six, then "+N more"), the tray sentence, the *Remember my choice* switch, *Cancel*, *Stop N tunnels and quit* (the main button when there is no tray) and *Minimize to tray* (main). The orphans popover opens beside the rail under the drag strip: a card per orphan with its description, *Relink to* (a dropdown, searchable past eight targets) and *Relink*, and the delete button, which confirms inline (*Cancel* grey and focused, *Delete* Dark Red); `Esc`, a click outside or its close button closes it.

### Terminal launch ("Connect")

- Configurable command template with placeholders, e.g. `{terminal_cmd} {ssh} {alias}` (the default).
  - The template is split into words on whitespace; `"…"` groups a word; there are no escapes and no shell. `{ssh}` (ssh binary path) and `{alias}` are substituted inside words, so an alias never adds arguments or commands.
  - `{terminal_cmd}` is only allowed as the first word: the remaining words run in the detected per-OS terminal. Without it, the template is the full command line of a terminal of the user's choice (e.g. `alacritty -e {ssh} {alias}`).
- Per-OS defaults:
  - Windows: Windows Terminal (`wt.exe new-tab --title <alias> …`) if present, otherwise ssh runs directly in a new console window (equivalent to `cmd`, without its quoting rules).
  - macOS: Terminal.app (via `osascript`).
  - Linux: first available of `$TERMINAL`, `x-terminal-emulator`, common emulators (gnome-terminal, konsole, alacritty, kitty, wezterm…).
- Other commands run in the terminal the same way (installing a key, `ssh-add` for a passphrase-protected key, §11): with `{terminal_cmd}` the command runs as is in the detected terminal (the template's other words are meant for Connect); a full template gets the program for the word `{ssh}` and the arguments, one word each, for the word `{alias}`.
- Each launch from the app is recorded in `history.toml` (only app-launched sessions can be tracked; say so in the UI).

### Close behavior

- If no tunnels are running: close immediately — unless *Minimize to tray* is the remembered choice and a tray is available (then the window hides, as below).
- If tunnels are running: modal "N tunnels are running", the running tunnels with their ports, then "Quitting stops them. In the tray they keep running." when a tray is available, or "Quitting stops them." when it isn't.
  - Buttons: **Minimize to tray** (only when a tray is available — milestone 6) · **Stop N tunnels and quit** · **Cancel**.
  - Switch: *Remember my choice*; resettable in Settings. Only a quitting choice can be remembered (*Cancel* is not): once remembered, closing with running tunnels stops them and quits without asking, until reset in Settings (`close_behavior` in `settings.toml`).
  - *Minimize to tray* can be remembered too (`close_behavior = "minimize-to-tray"`, also selectable in Settings): closing then always hides the window, with or without running tunnels, and the tunnels keep running. When the desktop has no tray at that moment, closing behaves as *Ask* (quit if nothing runs, else the modal without that button); Settings says so.
- One sshelf per user (two supervisors would fight over ports and files). The first start holds an exclusive OS file lock on `instance.lock` in the app folder (released by the OS on any exit, a crash included). A later start asks the running one to show its window (also from the tray) and ends before creating a window or tray of its own. The request travels locally, never over a network interface: a named event in the logon session's namespace on Windows (`Local\sshelf-show-<hash of the app folder>`), a Unix domain socket `instance.sock` in the app folder on Linux and macOS. If the running one doesn't answer, the later start says so (a message box on Windows) and still starts nothing. If the lock can't be taken at all (e.g. an unwritable app folder), sshelf starts anyway. (`sshelf_core::instance`; taken in `main.rs`, so tests and the tour never touch it.)
- Cmd+Q on macOS and Quit from the tray go through the same logic. Quit from the tray never minimizes: with nothing running it quits; with a remembered *Minimize to tray* and running tunnels it asks, with the window shown, offering *Stop and quit* and *Cancel*.
- The close decision runs in `App::logic`, so it works while the window is hidden; asking shows the window first.
- A host key prompt (§9) brings the window back, since a start from the tray may wait for it. Other errors while hidden show in the status bar when the window is shown again.

### Tray (milestone 6)

- Menu: last N connected hosts (click → open terminal session), groups with start/stop, active tunnels with stop, Show window, Quit.
  - *Connect* submenu: the last 8 distinct hosts launched from sshelf that still exist (any alias counts for its host), newest first.
  - *Groups*: "Start X", or "Stop X" with "(starting)" / "(2/3 running)" while not fully up.
  - *Active tunnels*: "Stop X" for a tunnel started by hand (it releases that start, §7); a tunnel only groups hold is shown disabled, "X (used by Work)".
  - Clicking the icon itself shows the window; the menu opens on right click (on macOS the other way round, see below). The tooltip counts running tunnels, and the icon gets a green dot while any run.
- **Tray menu (redesign):** left click on the icon shows and focuses the window; right click opens the **sshelf menu**, a small borderless, always-on-top viewport docked above the icon (16 px corners, same widgets and states as the window: group switches, active tunnels with stop, Connect list, *Show window*, *Quit*). It closes on focus loss or `Esc`.
  - The native `tray-icon` menu with the same items stays as the fallback: used when the icon's position isn't known (`TrayIconEvent` gives no rect, e.g. some KSNI hosts) and when *Settings › Window & tray › Tray menu* is set to *System menu* (`tray_menu = "sshelf" | "system"`, default `"sshelf"`).
  - The flyout must work while the main window is hidden, so it is driven from `App::logic` like the rest of the tray. Verify during implementation that eframe renders a secondary viewport while the root viewport is hidden; if it can't, the fallback menu is used while hidden.
  - As built (M9; checked on Windows 11 22621 with the window hidden): the right click reaches `App::logic`, which opens the menu and sends `ViewportCommand::RequestPaintWhileHidden` to the root. That one full pass of the hidden root creates the flyout, a deferred viewport shown from `App::ui` (eframe makes its window from the event loop; an immediate viewport made from a tray click panicked with "the user callback was never called"), which hands its clicks back through shared state; while it is visible, eframe keeps running the root's passes (a hidden root with a visible child still gets them), and its closing pass removes it. Contents: "sshelf" and "2 running · 1 failed"; a blue banner with *Show* while a host key waits; *Groups* with a switch each (degraded ones show "1/2" and *Retry*); *Active tunnels* with their state mark, ":port", "used by X" or "failed" / "reconnecting", *Stop* for a manual start, and a failed row opens the window on that tunnel; *Connect* with up to five recent hosts; *Show window* and *Quit*. It is placed centred on the icon, above it when the icon is in the lower half of the screen and below otherwise, kept on screen. It closes on focus loss, `Esc`, or a click that opens something (switches keep it open). Linux always uses the native menu (KSNI reports no icon position). The native menu now starts with the same summary line ("sshelf: 2 running · 1 failed") and the host-key item. The icon's dot has all four states (§10 *Status marks*); the tooltip is "sshelf · N tunnels running" (and "· N failed").
- The tray works from `App::logic`, which eframe keeps calling while the window is hidden (core events and tray clicks request a repaint): the menu stays current and actions run without the window.
- Backends (`tray-icon` crate): native on Windows and macOS; on Linux the KSNI backend (StatusNotifierItem over D-Bus, no GTK).
- If the platform has no tray (e.g. GNOME without an extension), don't offer *Minimize to tray*. On Linux a tray exists when `org.kde.StatusNotifierWatcher` has an owner on the session bus; this is checked at startup and with *Refresh* in Settings, which shows the result.
- macOS: create on the main thread.
  - As built: the buttons are swapped, as menu bar items behave there: a left click opens the menu (the sshelf menu, or the system menu when chosen), a right click shows the window.
  - While the window sits in the menu bar (closed with *Minimize to tray*), sshelf has no Dock icon: the activation policy is *accessory* (no Dock icon, no app menu), and *regular* again when the window shows, with the Dock icon's status dot reapplied.
- Windows: the sshelf menu never takes the foreground, so the taskbar's overflow flyout it may have been opened from stays open; a click outside it or `Esc` (seen by low-level mouse and keyboard hooks on their own thread while it is open) closes it. Its window gets the same frameless fix as the main window (no accent-colour border).
- *Settings › Window & tray › Tray icon*: *Colored* (the app icon; default) or *Monochrome* (`tray_icon = "colored" | "monochrome"`). Monochrome is one colour with the bars and the dot's ring cut out: on macOS a template image the system tints for the menu bar (the status dot in the same tint, since a template keeps only the alpha); on Windows white or black for the taskbar's theme (`SystemUsesLightTheme`, read when the icon is drawn), the dot keeping its colour; on Linux white.
- The taskbar button (Windows, X11) and the Dock icon (macOS) carry the same status dot as the tray icon. Windows: the big icon is set with `WM_SETICON` (winit's `set_window_icon` only sets the small one). macOS: `NSApplication.applicationIconImage` (winit ignores window icons there), 512 px with Apple's margin, which Cmd+Tab also shows; eframe sets it once from the window icon on the first frame, so on macOS the window icon is this Dock icon. Built from raw pixels (`NSBitmapImageRep`), never PNG data (egui#7155: a crash in some macOS versions). Wayland has no window icons: docks show the desktop entry's (`assets/linux/`, matched by the app id `sshelf`).

### Visual design

The visual reference is the design canvas "sshelf redesign — wireframes" (private to the owner); its boards are copied into `docs/design/` (see its README). This section is the rule; the boards show it. If they disagree, this section wins until it is updated.

- **Window:** frameless (no OS title bar or border), corners rounded 16 px, flat (no shadows anywhere: not on dialogs, popups, menus, toasts or the tray flyout; they only get a 1 px `#3E4451` outline). The one exception is the changed-host-key dialog (§9), which must look different from the unknown-key one: 1 px Light Red `#E06C75` outline and a red-tinted header band.
  - The top 44 px of the content area is the drag strip (`ViewportCommand::StartDrag`); it holds only *Minimize* and *Close* on the right (hover: Minimize turns Light Yellow, Close Light Red). No maximize button; double-clicking the strip maximizes / restores.
  - Resizing works from all edges and corners (`ViewportCommand::BeginResize` on a 6 px border), since the OS frame is gone. Minimum size 960 × 600.
  - Rounded corners need a transparent viewport (`with_transparent(true)`) with the app painted as a rounded rect. If a platform can't do transparency, fall back to square corners, never to the OS frame. Linux: rounded on Wayland; X11 uses square corners, since telling whether a compositor runs would need an X11 dependency. Maximized windows are square everywhere.
  - Windows: egui-winit always asks winit for its "undecorated shadow" on frameless windows, which keeps a 1 px frame strip at the top of the window; Windows draws a line in it and a drop shadow around the window. sshelf subclasses the root window so all of it is client area while not maximized (`WM_NCCALCSIZE`; maximized windows are left to winit, which clamps them to the work area) and turns DWM's own corner rounding and border off (on Windows 11 they would clip at 8 px and outline the window). The window keeps the caption, sizing-frame, system-menu and minimize/maximize styles that Aero Snap and Win+arrow snapping rely on; the Snap Layouts flyout needs a maximize button the OS knows about, so it only opens with Win+Z. Looked at on Windows 11 22621.
- **Layout:** a 64 px icon rail on the left (app icon, Hosts, Tunnels, Groups; orphan indicator and Settings at the bottom), then list pane + detail pane, then a 28 px status bar. Rail items show a tooltip with the name and shortcut (`Ctrl+1…4`).
  - The status bar is always shown: running and failed tunnel counts, the ssh version, the agent state, and "config reloaded HH:MM". An error takes its place until dismissed: same 28 px height, red tint, Lucide `circle-x`, a dismiss button.
  - Editors (host, tunnel, group) replace the detail pane; the list stays visible, dimmed. Their *Problems* / *Changes to write* column (380 px) sits right of the form while the pane is at least 880 px wide and stacks below the form when it is narrower, so the default 1280 px window shows both side by side.
- **Hosts list style** is a setting (*Settings › Appearance › Host list style*: **List** or **Cards**, default List; cards can be grouped by tag, file or not at all). Stored in `settings.toml` as `host_list_style = "list" | "cards"` and `cards_group_by = "tag" | "file" | "none"`. Both views use the same detail content; in Cards the detail is a right-hand pane.
- **Colours (One Dark):** background `#282C34`; rail, inputs, popups `#21252B`; raised / hover `#2C313A`; selected `#3E4451`; outline `#4B5263`; text `#ABB2BF`; strong text `#D7DAE0`; secondary text `#9AA1AD` (Comment Grey `#5C6370` only for marks and syntax, never body text: it fails contrast). Accent Magenta `#C678DD` (selection, primary button, focus inside inputs, switches, checkboxes). Status: running Green `#98C379`, starting / degraded / warning Light Yellow `#E5C07B`, failed / error Light Red `#E06C75`, destructive buttons Dark Red `#BE5046` with white text, info / links / keyboard focus Blue `#61AFEF`.
  - Tinted banners and status chips use green `#2B3528`, yellow `#3B3527`, red `#3A2A2E` and blue `#24303D` behind the full status colour.
  - Also used: tooltip background `#181A1F`; modal overlay `rgba(24,26,31,.72)`; zebra rows in tables `#2A2E36`; separators inside cards `#353B45`; disabled menu text `#7F848E`; link hover `#8CC4F5`.
  - Host colours (*Notes & tags*) are swatches that never collide with status colours: Blue `#61AFEF`, Cyan `#56B6C2`, Magenta `#C678DD`, Dark Yellow `#D19A66`, Comment Grey `#5C6370`, and *none*. A colour stored in `hosts.toml` outside these shows as a sixth, selected swatch; nothing is rewritten until the user picks another. A "+" swatch after them opens a small picker (saturation / brightness square, hue bar, `#rrggbb` field; egui's own picker has an emoji button) for any other colour, saved only on *Use colour*; such a colour may look like a status colour, which is the user's choice.
- **Radii:** 1 px (table rows, code bars, the inner side of joined pieces), 4 px (checkboxes only), 8 px (controls, cards, menus, chips), 16 px (window, dialogs, tray flyout, large panels). No other values.
- **Type:** UI font Inter (Ubuntu acceptable), mono JetBrains Mono for values, paths, aliases and logs. Sizes: 22/600 titles, 20/600 editor, Settings and cards-pane titles, 17/600 dialog titles, 16/600 pane headings, 13/600 sections, 14 list items, 13 body and controls, 12 secondary, 11 overlines. Spacing in 4/8 px steps.
- **Icons:** Lucide (ISC licence) everywhere, loaded as an icon font (`lucide.ttf` from the `lucide-static` package) in its own `FontFamily::Name("lucide")`. No emoji and no self-drawn icons; status dots and the app icon are painted shapes. Sizes: 16 default, 14 in small buttons and inline text, 12 in keyboard hints, 20–28 in empty states. The icon names per action are on the canvas (*Components › Icons*).
- **Status marks:** a filled dot is a settled state (running green, stopped grey, failed red); a ring is a transition (starting yellow, stopping grey, waiting for the user blue). The app and tray icon carry one dot: green while anything runs, yellow while starting or degraded, red when something failed.
- **Destructive confirmations:** the safe action is a grey (secondary) button and gets focus; the destructive one uses Dark Red. (The boards show the safe action in Magenta; the owner chose grey.)

### Interaction and motion

All timings below are final and live as constants in one place (`theme.rs`). egui's `animate_value_with_time` only interpolates linearly, so animations go through a small tween helper (`motion.rs`) that applies these curves to egui's frame time and requests repaints only while something moves. Easing "in-out" is `cubic-bezier(.65,0,.35,1)`, "out" is `cubic-bezier(.2,.8,.2,1)`, "in" is `cubic-bezier(.4,0,1,1)`. The playable reference is the canvas board *Interaction and motion*.

| Element | Cursor | Motion | Duration | Easing |
|---|---|---|---|---|
| Buttons, rail items, cards, tabs, switches, chips, star | `PointingHand` | hover background fade | 80 ms | out |
| Selection in rail, lists, tabs, segmented controls | — | **sliding highlight**: the active background (or tab underline) moves and resizes from the old item to the new one; text colour cross-fades | 180 ms | in-out |
| Hover highlight in list rows, dropdowns and context menus (`PointingHand`) | — | slides between rows; fades out in place when the pointer leaves; on re-entry it appears on the hovered row without sliding. In lists it is the raised colour `#2C313A` under the selected row's `#3E4451` | 50 ms slide, 120 ms fade | in-out / out |
| Switch | `PointingHand` | knob slides, track colour fades | 200 ms | in-out |
| Tooltip | — | appears after 400 ms of hover, then fades in with a 3 px slide; fades out the same way; the next tooltip within 300 ms shows at once | 120 ms | out (in) / in (out) |
| Context menu (right click) | — | fades in and scales 98 → 100 % from the click point; fades out | 120 ms | out |
| Copyable value row | `Copy` | row background and copy icon fade in; click copies, toast "Copied <field>" | 80 ms | out |
| Text inputs, raw editor | `Text` | focus outline fade | 80 ms | out |
| Disabled controls | `NotAllowed` | none; a tooltip says why it is disabled | — | — |
| Toast | — | fades in with a 6 px rise; fades out after 3 s; hover pauses; the close button removes it at once | 120 ms | out |
| Starting / waiting ring | — | opacity pulses 100 → 40 → 100 % (`request_repaint_after`, only while visible) | 1 s loop | in-out |

- **Reduce motion** (*Settings › Appearance*, `reduce_motion = true`): every duration becomes 0 and the pulse is static.
- **Switch colour:** the on-colour is a parameter, Magenta when not passed. The boards use the default everywhere, group switches included (running state is shown by the status dot next to them).
- **Favorites:** the star next to the host title is a toggle button (adds / removes the favorite); its tooltip says what a click will do. Lucide has no filled star, so the outline's colour fades from secondary grey to Light Yellow in 120 ms.
- **Host detail edits** (star, colour, tags) save at once; notes get *Save notes* / *Revert* while they differ from the saved text. A tag chip shows an × on hover; "+ tag" opens a small field (Enter or leaving it adds, Esc cancels; commas add several).
- **Copy on hover:** in host detail and effective config (not tunnel forwards: the owner found it unneeded there), hovering a value row shows a copy icon 8 px right after the value. A long value truncates earlier to leave room for the icon, so the icon is always visible. A click anywhere on the row copies the full value (never the truncated text).
- **Truncated text:** every alias, path, value and log line that may not fit goes through one wrapper (`truncated_label`). It truncates with an ellipsis (in the middle for paths, so the file name stays visible) and shows a tooltip with the full value, with the tooltip fade above, only when the text was actually cut.
- **Forwards** are edited as separate fields, never as one `host:port` string: *L*: bind address, local port → IP / host on the remote machine : remote port. *R* mirrors it: bind on server, remote port → IP / host on this machine : local port. *D*: bind address and local port. The host editor writes them back as one directive line (`LocalForward 6443 127.0.0.1:6443`); the bind address of a host's `LocalForward` stays in *Raw text*.
  - In the host editor a forward row is written once all its fields are filled in (a half-typed one says "Not written until every field is filled in"); a value that isn't `[bind:]port host:port` (a Unix socket, one combined argument) shows as text with "edit in Raw text". Where the row is too narrow, the target moves below the listen port.
- **Host editor:** title (20/600) with the file and lines, *Form / Raw text* as a segmented control and *Generate key…*; form rows of label (150 px; a yellow dot while the value differs from the saved one) and field, with ssh's problem for that line under its field; IdentitiesOnly / ForwardAgent as dropdowns. *Add IdentityFile* is a menu (270 px wide, long paths cut with an ellipsis): first *Browse…* (Lucide `folder`) and "Type a path" (`pencil`), then the key pairs in `~/.ssh` (found by their `.pub` files, with type and comment; private keys are never read, keys already in the host are disabled). *Browse…* opens the system file dialog (the `rfd` crate: native on Windows / macOS, the xdg desktop portal on Linux; core runs it, like every other side effect). The dialog is modal to sshelf's window (its parent), and the app waits behind an overlay ("Choose a key file in the system dialog") until it closes. A key in `~/.ssh` is written as `~/.ssh/<name>`. The *Problems* card shows invalid lines at once and ssh's after *Check*/*Save* ("checked HH:MM with ssh -G -F"); *Changes to write* shows the diff on tinted rows (for a new host, *Where it goes* shows the block in its file). The footer has *Save* (*Add host*), *Check*, *Discard* (*Close*, *Cancel*) and why *Save* is unavailable. A file changed on disk shows a yellow banner with *Discard my changes*, *Apply my changes to the new version* (only when this block didn't change on disk) and *Overwrite…*, which asks inline with *Keep them* (grey) and *Overwrite* (Dark Red); the disk's changes replace *Changes to write* meanwhile. The list beside an open editor is dimmed but stays clickable.
- **Keyboard:** every action reachable by mouse is reachable by keyboard; focus shows a 2 px Blue ring with a 2 px gap (keyboard focus only, not on click).

### Dropdown

One dropdown component is used for every select. The playable reference is the canvas board *Dropdown*.

- **Parameters:** `max_visible` (rows shown before the list scrolls, default **10**), `searchable` (search field at the top), `multi` (multi-select with checkboxes). Checkboxes exist only in multi-select dropdowns; everywhere else on/off is a switch.
- **Open / close:** the panel is revealed from the top edge downward by clipping, with no fade (**200 ms**, out); closing rolls it back up (**140 ms**, in). It opens upward when there's no room below; the chevron turns with it.
- **Rows:** 30 px each; list height = min(rows, `max_visible`) × 30. The *Save to* dropdown is the exception: two-line 36 px rows (the path, then "main config · 19 hosts" / "included from config:2 · 4 hosts", or a Lucide `lock` and the read-only reason on a disabled row). Beyond that the list scrolls with a thin scrollbar and a 20 px fade at the bottom edge. Opening scrolls the selected row into view.
- **Hover / keyboard highlight:** as in the motion table (50 ms slide). It follows ↑↓ and scrolls the row into view.
- **Single-select:** the selected row shows a Lucide check on the right (fades 120 ms); picking closes the dropdown.
- **Searchable:** the search field is fixed at the top and focused on open. Typing filters by case-insensitive substring; the list height glides to the new row count (120 ms, in-out); the highlight moves to the first match. No result shows one "No … match" row. A clear button appears in the field when it has text.
- **Multi-select:**
  - Toggling keeps the dropdown open. The checkbox (16 px, 4 px corners) fills Magenta and the tick draws itself in (**100 ms**, in-out); unchecking reverses it.
  - The footer shows "N selected" (and "· M shown" when searchable), *Clear* (empties the selection, not the search) and *Done*.
  - Selections hidden by the search are kept and counted.
  - The closed control shows the selected values as chips, as many as fit; a long value is truncated inside its chip. The rest collapse into a Magenta "+N" chip; hovering it for 300 ms shows those values as chips in a tooltip (120 ms fade). A newly added chip scales in (120 ms).
- **Keyboard:** Enter / Space / ↓ on the closed control opens it; typing goes to the search when `searchable`; ↑↓ move; Enter picks (single) or toggles (multi); Space toggles (multi); Esc or a click outside closes (multi-select keeps its changes).
- **Where:** single — IdentitiesOnly, ForwardAgent, Group by, closing behaviour, *Save to* file, key algorithm; single + search — tunnel host; multi — tunnels in a group. The host tag filter is a row of chips, not a dropdown (§10 *Layout*).

### Design of features already in the spec (M7, M8)

- **Health:** a 6 px dot at the right end of every host row and card: Green = login OK, Light Yellow = reachable but sshelf can't log in yet (no key accepted, or host key not known yet), Light Red = unreachable, Comment Grey = not checked yet; a ring while a check runs. Every yellow and red state shows a tinted banner under the Health line with the reason and its fix: *Load key into agent* (disabled with the reason when the agent service is stopped; it runs `ssh-add` in a terminal for the host's first `identityfile` from `ssh -G`) and *Generate key…* for no key; *Connect in terminal* for an unknown host key (ssh asks there; the health check itself never prompts); ssh's last line and *Edit host* for unreachable. "How to start it" next to a stopped agent opens *Settings › ssh*. While *Check all* runs, its button shows a ring and "7 / 19"; clicking it again stops. Hovering the dot shows the result, latency, ssh's reason and when it was checked. The host detail has a *Health* line (dot, result, login time, "checked N min ago", *Check*). *Check all* is an icon button (Lucide `activity`) next to *New host*, with a tooltip.
- **Key generation:** *Generate key…* (Lucide `key-round`) sits next to the *Form / Raw text* tabs in the host editor. The dialog uses the standard dropdown for the algorithm (Ed25519 marked "recommended"; security keys in their own group, noted as opening a terminal), password fields with a show/hide eye, a stepper for KDF rounds, a file field (typed path; no file picker), and a yellow banner while the passphrase is empty. It stays a window, not a modal (§11), styled like the dialogs (640 px, 16 px corners, an icon tile and *Close* in the header, the footer under a line). The KDF stepper is − / a typed number / +. *Copy* (fingerprint, public key) shows the usual toast; "How to start it" next to a stopped agent opens *Settings*. The result dialog shows fingerprint, type and passphrase state, the public key with a copy icon, and *Next steps* as rows (button + one-line explanation): *Use in this host*, *Add to agent* (disabled with the reason when the agent isn't available), *Install on server*.
- **Switches, not checkboxes:** the tunnel's *Reconnect automatically* and the group's *Start with sshelf* are Magenta switches. A group's tunnels are picked with the multi-select dropdown (rows show tunnel name and host). Groups that start with sshelf show a "starts with sshelf" chip (Lucide `power`) in their detail.

### Onboarding tour (milestone 10)

A first-run tour of every main feature, played on the real UI against sample data. Reference boards: `Tour-Welcome` (first-launch screen), `Tour-Step` (playable: the welcome screen and all 27 steps), `Tour-Steps` (storyboard: every step's text and the glossary, which are final). The views under the overlay on `Tour-Step` are simplified sketches. The tour runs on the real screens; only the overlay, popover, glint and welcome screen come from the boards. Implementation notes: `docs/onboarding-tour.md`.

- **Welcome screen:**
  - **When:** shown instead of the main UI while `settings.toml` has no `tour` key. That means new installs, and existing installs once.
  - **Layout:** the app logo (64 px), "Welcome to sshelf", one line about what sshelf is, then two 16 px-cornered cards side by side:
    - *Take the tour*, with a Magenta outline and the primary button *Start the tour*.
    - *Start with my config*, showing the main config path and its host count, with *Open my hosts*. With no config: "No `~/.ssh/config` yet — your first host creates it", and its button is *New host*.
  - **Below the cards:** "You can take the tour again any time from Settings › Help."
  - **State:** either choice writes `tour = "offered"`; ending the tour in any way writes `tour = "done"`. A missing key is a default, so the file version stays the same.
- **Sandbox (hard rules):**
  - **Isolation:** while the tour runs, nothing reads or writes the user's ssh config, `known_hosts`, keys, agent or app data. Nothing connects anywhere, and no terminal opens.
  - **The sandbox Core:** the tour runs a second `Core` with its own temporary home and app data dir (`<temp>/sshelf-tour-<pid>`). It holds a sample `ssh_config`, an empty `known_hosts` and the sample `hosts.toml` / `tunnels.toml`. It is deleted when the tour ends or the app quits; leftovers from a crashed run are deleted at the next start. The user's own `Core` is left alone and takes over again afterwards.
  - **Its ssh is sshelf itself:** the binary plus the prefix argument `--fake-ssh` (§8), for ssh and every companion tool (`ssh-add`), scripted per sample host. Only the sandbox Core can use it; Settings can't select it.
  - **Real code where safe:** key generation uses the real generator (§11) into the temporary `.ssh`.
  - **Toasts instead of side effects:** *Connect*, *Load key into agent*, the security-key terminal, *Stop and quit* and *Minimize to tray* don't run. A toast says what would happen ("Tour: would run `ssh web-01` in your terminal").
  - **Install on server** runs against the fake server, not in a terminal. After it, the tour starts the failed tunnel again.
  - **Groups:** *Start with sshelf* has no effect for sandbox groups.
  - **Ports:** sample tunnels listen on high local ports (15432, 18080, 19090, 12049, 18443). When the sandbox is built, a port that is busy on this computer is moved to a free one, so the tour never fails because of the user's own services.
- **Sample world** (all names made up, `*.example.com`). The tour starts with no tunnels running, so every change is visible.

  | Item | Contents |
  |---|---|
  | Hosts | `web-01`, `web-02`, `db-primary` (via `bastion`), `bastion`, `staging-api`, `ci-runner`, `backup-nas`; a `Host *` block with `IdentityFile ~/.ssh/id_ed25519` and `ServerAliveInterval 30` |
  | fake-ssh scenarios | `web-01`…`bastion`: ok. `staging-api`: no key accepted. `ci-runner`: unreachable. `backup-nas`: no key accepted until a key is installed, then ok. |
  | Tunnels | `db tunnel` 15432 → db-primary:5432 via bastion. `staging api` 18080 → staging-api:80. `ci metrics` 19090 → ci-runner:9090. `backup` 12049 → backup-nas:2049. The tour adds `web admin` 18443 → localhost:443 on staging-api. |
  | Groups | `Work on staging` (staging api, ci metrics), `Databases` (db tunnel) |
- **While the tour runs:**
  - **Drag strip:** a Magenta "Tour · sample data" chip, "nothing here touches your files" and *Exit tour*.
  - **Status bar:** shows the fake ssh version.
  - **Settings:** the rail item is disabled with the tooltip "Not available during the tour".
  - **Tray:** mirrors the sandbox under a "Tour" header.
  - **Closing the window** outside step 23 ends the tour first. The normal close behaviour then applies to the user's own Core.
- **Steps:** 27 in 6 chapters, titles and texts exactly as on `Tour-Steps`.
  - *Watch* steps only explain. *Do it* steps wait for the action.
  - "Done" means the Core state the step asks for, not a click, so the user's own click and *Show me* both work.
  - "Busy" means the action is still running; Next is disabled meanwhile.

  | # | Chapter | Step | Kind | Spotlight | Done when |
  |---|---|---|---|---|---|
  | 1 | Hosts | Your hosts | watch | host rows | — |
  | 2 | Hosts | Connect | do it | *Connect* | clicked (toast) |
  | 3 | Hosts | Health check | watch | Health line | — |
  | 4 | Hosts | Check all hosts | do it | host list with *Check all* | all 7 checked (busy while running) |
  | 5 | Hosts | What ssh really uses | do it | detail tabs and table | *Effective config* tab open |
  | 6 | Editing | Edit safely | do it | *Edit* | editor open |
  | 7 | Editing | Form or raw text | do it | editor tabs and form | *Raw text* open. The tour has changed `User` to `admin` on entering the editor. |
  | 8 | Editing | Review, then save | do it | *Changes to write* and *Save* | saved (sample file) |
  | 9 | Tunnels | Tunnels | do it | Tunnels rail item | Tunnels view open |
  | 10 | Tunnels | Start a tunnel | do it | db tunnel row | Running (busy while Starting) |
  | 11 | Tunnels | Running shows everywhere | watch | status bar count, plus the logo and Tunnels rail item as extra holes | — |
  | 12 | Tunnels | Make your own | do it | *New tunnel* | tunnel editor open |
  | 13 | Tunnels | Three fields per forward | watch | the forward row (pre-filled: web admin, staging-api, 18443 → localhost:443) | — |
  | 14 | Tunnels | Save it | do it | editor footer | saved |
  | 15 | Fixing problems | When it fails | do it | backup row and its failure banner | backup Failed (busy while Starting) |
  | 16 | Fixing problems | Fix it | do it | *Generate key…* in the banner | key window open |
  | 17 | Fixing problems | Generate key | do it | the key window (Ed25519, passphrase pre-filled) | key created |
  | 18 | Fixing problems | Put it to use | do it | the result window | *Install on server* done, backup restarts and reaches Running |
  | 19 | Groups | Groups | do it | Groups rail item | Groups view open |
  | 20 | Groups | One switch | do it | Work on staging row | group running (busy while starting) |
  | 21 | Groups | Edit a group | do it | its *Edit* | group editor open |
  | 22 | Groups | Group editor | do it | the editor (tour adds web admin, turns on Start with sshelf) | saved |
  | 23 | Window & tray | Closing the window | do it | window *Close* | close dialog open |
  | 24 | Window & tray | Quit or keep running | do it | the close dialog | *Minimize to tray* clicked (toast; the window stays) |
  | 25 | Window & tray | From the tray | watch | none; the popover shows a picture of the sshelf tray menu with the sandbox's live state | — |
  | 26 | Window & tray | Status bar | watch | status bar, showing backup's earlier error | — |
  | 27 | Done | You're set | — | none | *Open my hosts* ends the tour |

- **Moving through the tour:**
  - **Entering a step** switches to the view, editor, tab or dialog it needs, also when entered with Back.
  - **Back** never undoes an action. On step 1, Back returns to the welcome screen.
  - **Advancing:** an action that completes at once (a click that opens something) advances by itself. One that takes time (start, check) stays on its step until done.
  - **Ending:** *Skip tour*, *Exit tour*, `Esc`, finishing or quitting ends the tour. That drops the sandbox, writes `tour = "done"` and opens the user's hosts.
- **Overlay:**
  - **Dimming:** the window is dimmed with `rgba(24,26,31,.72)` except one or more holes around the targets: their union plus 6 px, 8 px corners, a 2 px Magenta ring.
  - **Input:** clicks and hovers inside a hole reach the real UI; outside they are ignored.
  - **Glide:** the hole glides between steps (240 ms, in-out, through `motion::tween_rect`).
  - **No-target steps:** keep the dimming with the ring faded out. Never swap the shape mid-glide, or the ring colour smears over the window.
  - **Late targets:** a target that isn't drawn yet (just switched screens) is waited for. After 30 frames without it, the step falls back to no spotlight and logs a warning.
- **Popover:**
  - **Size and placement:** 340 px wide, `#21252B`, 16 px corners, 1 px `#3E4451` outline, no shadow. It is placed right of the spotlight, else below, else left, else above (16 px gap, 12 px from the window edge), and glides with it.
  - **Header:** "*Chapter* · N of 27" with the chapter in Magenta, and *Skip tour*.
  - **Body:** the title (16/600) and the text (13).
  - **Do-it hint:** on *do it* steps, a Magenta hint (12, Lucide `mouse-pointer-click`).
  - **Footer:** a 4 px progress bar (width tweened 240 ms), *Back*, and the primary button. It reads *Show me* while the action is pending (it performs the action), *Next* once done, disabled while busy, and *Open my hosts* on the last step.
- **Glint:** on a *do it* step, the element to click gets a light sweep while the action is pending (not while busy).
  - **Shape:** a diagonal white band, peak alpha 0.32, crossing in about 1 s and repeating every 2.4 s (in-out). It is clipped to the element's rounded shape: egui clips only to rectangles, so the band is built from the rounded outline.
  - **Reduce motion:** a static faint highlight instead.
- **Glossary terms:** words in brackets in step text (13 terms, texts on `Tour-Steps`) get a dotted Magenta underline. Hovering one shows the standard tooltip (§10 *Interaction and motion*) at the popover's full width: below the popover when it sits in the upper part of the window, above otherwise. It never leaves the window.
- **Reduce motion:** no glides (the spotlight and popover jump) and a static glint.
- **Settings › Help:** a new section with two items, both disabled with a reason while one of the user's own tunnels runs (the tour would swap Cores):
  - *Take the tour again*;
  - *Start a chapter…* (dropdown of the 6 chapters). Before the chapter's first step, the sandbox replays the earlier chapters' actions without UI.
- **Small additions to the normal app, used by the tour:**
  - The Tunnels rail item carries the same green status dot as the app icon while any tunnel runs.
  - A tunnel failure banner for "no key accepted" offers the same fixes as the health banner (*Generate key…* for the tunnel's host, *Load key into agent*).

### Streamer mode (milestone 11)

A privacy switch for screen sharing and streaming: while it is on, sshelf masks every value that could identify a machine, an account or the user. Reference boards: `Streamer-Mode` (playable: masking, hold to peek, the chip menu, Settings) and `Streamer-States` (every token state, what is masked, the tray, dialogs, limits). The Privacy card is also on `A-Settings`.

- **Turning it on and off:**
  - **On:** *Settings › Privacy › Streamer mode*, or the *Streamer mode* switch at the top of the sshelf tray menu (it works with the window hidden). There is no keyboard shortcut.
  - **Off:** the same two places, or *Turn off streamer mode* in the drag-strip chip's menu.
  - **Applies at once:** both Privacy switches take effect immediately, outside *Revert* / *Save*.
  - **Toast:** turning it on shows "Streamer mode on · values are masked" (Lucide `eye-off`, Blue); off shows "Streamer mode off · values are visible" (`eye`, Light Yellow).
  - **Stored:** `streamer_mode` and `streamer_hide_aliases` in `settings.toml`. Both default to off; a missing key is the default, so the file version stays the same. Remembered across restarts.
- **The chip:** while it is on, the drag strip shows a chip: Blue `#61AFEF` on `#24303D`, 26 px, 8 px corners, Lucide `eye-off` 14, "Streamer mode", a chevron. With hidden aliases it adds "· names hidden" in `#8CC4F5`. It sits left of the tour chip when both apply.
  - **Its menu** (340 px, the standard menu style): "Hide host aliases and tags" with a switch; a separator; *Turn off streamer mode* (Lucide `eye`); a note "Turning it off shows every value at once. Do it off stream."
- **What is masked:**

  | Masked | Kept visible | Also masked with *Hide host aliases and tags* |
  |---|---|---|
  | `HostName` and every address: IPv4, IPv6, DNS names from the config | Host aliases, unless the alias is an IP address or equals its HostName (then always masked) | Host aliases, shown as "host N" |
  | `User`, `Port`, and every port number sshelf shows | Tunnel and group names | Tags, shown as a `tag` label |
  | Paths: IdentityFile, config, `known_hosts`, key files, the ssh binary, app data, backups, the detected terminal | States, counts, health results and timings | Aliases inside free text (logs, errors) |
  | ProxyJump / ProxyCommand values, tunnel bind addresses and targets | Option names and non-sensitive values (`30`, `no`) | |
  | Fingerprints and public keys (host-key dialogs, key generation) | ssh version, agent state | |
  | Notes, through the free-text filter | | |
- **Masked tokens:** three looks for one widget (`widgets::masked`):
  - **Field value** (detail cards, effective config, tunnel rows, Settings, dialogs): "•••••" (always five dots, never the real length). Mono 12, 18 px tall, 6 px horizontal padding, 8 px corners, background `#2C313A` (`#353B45` on cards), dots `#9AA1AD`, letter spacing 0.06 em.
  - **Free text** (logs, errors, toasts, tooltips, notes): each match becomes a typed label in the same pill: `ip`, `host`, `user`, `port`, `path`, `key` (mono 11). The kind comes from what matched, so the sentence still reads: "connect to host `ip` port `port`".
  - **Alias** (only with *Hide host aliases and tags*): "host N" in the normal text style with a 1 px dotted Blue underline. N is the host's position in the config, stable until the config changes. Tags show as a `tag` label.
- **Hold to peek:**
  - **Peeking:** pressing on a token shows the real value while the button is held, with the yellow tint (`#3B3527` background, `#E5C07B` text). Releasing, or the pointer leaving the token, hides it again. One token at a time. There is no click-to-reveal toggle and no reveal on hover.
  - **Cursor and tooltip:** the cursor is `Grab` on a token and `Grabbing` while held. After 400 ms of hover, the standard tooltip says "Hold to reveal" (it hides while held).
  - **Copy:** copy buttons and copy-on-hover rows copy the real value without showing it; the toast says "Copied HostName · not shown in streamer mode".
  - **Tray:** tokens in the tray menu can't be peeked. The native fallback menu shows "•••••".
  - **Where peeking isn't possible:** clickable rows (host list and cards, tunnel and group rows) and tooltips that only take plain text show "•••••", or "host N" / `tag` for hidden names, without peeking: a press on a row selects it.
- **Free-text filter:** one function used everywhere text from data is drawn: labels, tooltips, logs, toasts, the status bar, the tray menu and tooltip, the window title.
  - **What it knows:** every HostName, User, ProxyJump / ProxyCommand host, IdentityFile (and other file) path, forward bind address and target in the loaded config and tunnels; the home folder path; the local login (`ssh -G` reports it as `user` when the config sets none); and shapes: anything like an IPv4 or IPv6 address, paths under `~` or the home folder, key fingerprints (`SHA256:…`, the MD5 colon form) and public-key blobs (`AAAA…`), matched by pattern rather than by a list of keys shown.
  - **Ports in free text** are masked in context only: after "port ", after "-p ", and after a `:` that follows an address or a `]`. A bare number elsewhere ("14:02:22", "22 min") is left alone. Every structured port field is masked.
  - **Matching:** longest match first, so `10.0.4.12` never leaves `.12` behind. Paths match before their parts.
  - **Updates:** the list is rebuilt on every config load and tunnel change.
- **Editors:** the host editor (form and raw text), *New host* and its *Where it goes* preview show a cover while streamer mode is on.
  - **Cover:** a 44 px Blue lock tile, "Hidden in streamer mode", and "The editor shows the raw config, so it stays covered while streamer mode is on. Hold to peek; turn streamer mode off to edit." Then *Hold to peek* (shows the editor while held) and *Close*.
  - **Other inputs:** text typed into inputs elsewhere (search, a new tag) is shown as typed. Masking applies to data, not to the user's own input. The tunnel editor's command preview is masked. Search still matches the real aliases and tags while they are hidden.
  - **Pre-filled inputs:** a field sshelf fills from stored data (an existing tunnel's bind address, ports and target in the tunnel editor; the ssh binary override in Settings; the key generation fields below) is masked until the user focuses it; from then on it is their input and shows as typed, also after focus leaves.
  - **Notes:** shown through the free-text filter and read-only while streamer mode is on; editing them means turning it off (they are data, like the editor).
- **Dialogs:**
  - **Host key dialogs:** host, port and fingerprint are masked tokens; *Trust* / *Cancel* work as usual. With hidden aliases the changed-key dialog asks to type "host N"; that or the real alias confirms.
  - **Key generation:** the file field (a path under the home folder) and the comment ("<alias> (sshelf)") are pre-filled inputs: masked until the user focuses them, then shown as typed. The result window masks the fingerprint and public key; their copy buttons still work.
  - **Close dialog:** ports are masked.
- **Settings › Privacy** (a card in the left column, below *Window & tray*):
  - **Streamer mode:** "Masks addresses, users, ports, paths and keys everywhere in sshelf. Hold a masked value to peek; copy still copies the real value." with a switch.
  - **Hide host aliases and tags:** "Shows them as host 1, host 2… Only while streamer mode is on." with a switch (dimmed while streamer mode is off).
  - **Footnote:** "Applies at once, without Save. Not covered: the terminal that Connect opens and system file dialogs."
  - **With streamer mode on,** Settings masks its own values: paths in *ssh*, *Connect* and *Files*.
- **Limits:**
  - **Outside sshelf:** the terminal that *Connect* opens, system file dialogs, and anything shown while streamer mode was off are not covered.
  - **Unknown strings:** a hostname the filter can't know (inside a `ProxyCommand` script, in a remote error) is only caught if it looks like an IP address.
- **Tour:** during the onboarding tour (§10 *Onboarding tour*) streamer mode stays as set; the sample data is fake, but masking still applies.

### kubectl tunnels (milestone 13)

The UI for §7 *kubectl tunnels*. Reference boards: `Tunnel-Kubectl` (playable: list, filter, detail, editor), `Tunnel-Kubectl-New` (playable: creating one, with motion) and `Tunnel-Kubectl-New-Steps` (the same flow as static frames, with the rules), `Kubectl-States` (marks, row states, failures, pickers, Settings, tray, groups, streamer), and the updated `A-Tunnels`, `Tunnel-States`, `Tunnel-Edit`, `A-Settings`.

- **Type marks:** every tunnel shows its type as a 24 px tile (8 px corners, `#2C313A`, icon 14): ssh is Lucide `terminal` in secondary grey, kubectl is Lucide `ship-wheel` in Blue `#61AFEF`. 28 px before the name in the detail header; 14 px without the tile in the tray menu and dropdown rows. The tooltip names the type.
- **Tunnel list:** the mark starts each row. Second line for kubectl: `<context> / <namespace> · <kind>/<name> · <first local port>` ("current context" when the context is empty). States, marks and the one action are those of ssh tunnels. Above the rows, an *All / ssh / kubectl* segmented filter, shown only while both types exist and remembered for the session.
  - As built: a row is the type tile, the status dot, then the two lines (as on `A-Tunnels`). A kubectl row's second line is the forward line above while stopped or running (a group's shows its owners, as for ssh), "Starting… waiting for “Forwarding from …”" while starting, "Failed · service not found" / "not logged in to <context>" / "can't reach the cluster" / "a local port is in use" / "context not in your kubeconfig" otherwise. While kubectl isn't found, a stopped kubectl row reads "kubectl not found", and its *Start* (row, header, menu) is disabled with the reason.
- **Detail:** the header mark, name and status chip; "kubectl · context dev-k3s · namespace api · service/web" and the owners; the failure banner (§7 *kubectl tunnels*, *Failures*); a table *Listen on* / *Local port* / *Port on the service* (or *pod*; for a deployment "*pod (picked by kubectl)*"); the log. Right-click: *Copy ssh command* becomes *Copy command* for both types.
  - As built: the failure banner's fixes are *Pick another <kind>* (opens the editor), *Retry* (not logged in, can't reach the cluster), *Edit* (port in use, no context) and *Settings › kubectl*, each with *Show log*; the plain "kubectl exited" has *Show log* only. A pod that is gone says a deleted pod doesn't come back under its name.
- **Editor:** the existing tunnel editor with an **ssh | kubectl** segmented switch (icon and word) at the right of the title.
  - A new tunnel starts as the type used last in this session (ssh at first); *New tunnel* from a host always starts as ssh with that host. Switching keeps the name and the local ports; the other type's fields are kept until Save, so switching back loses nothing. While kubectl is not found the switch stays usable (see below).
  - kubectl fields, in order:
    - **Name.**
    - **Context:** a dropdown of the kubeconfig's contexts with each cluster as a dim note; searchable past ten. Empty (the default) shows the current context's name with the note "current context"; picking a context pins it. Footer: "From your kubeconfig · read locally, nothing connects". Changing it clears namespace, resource and forwards.
    - **Namespace:** a searchable dropdown that lists the cluster's namespaces on open ("Listing namespaces on dev-k3s…" with the pulsing ring), footer "kubectl get namespaces · N · you can also type one". Required. Changing it clears resource and forwards.
    - **Forward to:** *Pod / Service / Deployment* (segmented, Service by default) and a dropdown of that kind in the namespace with their ports (pods: phase; deployments: ready / desired), listed on open like namespaces, footer `kubectl get services -n api`. Disabled until a namespace is set ("Pick a namespace first"). Changing kind or resource clears the forwards.
    - **Reconnect** switch, note "restarts the forward when the pod restarts or the connection drops, with growing delay".
    - **Forwards:** *Listen on* (one address, "kubectl --address, for every forward"), then a card per forward: *Local port* → *Port on the service* (or pod) and the remove button. After a resource is picked one empty forward appears with the resource's ports as chips ("80 http"); a chip fills the port (the number) and suggests the local port: the same port (5432 → 5432), or 18000 + port below 1024 (80 → 18080; binding those needs root on Linux and macOS); if another tunnel already uses it, the next number up. *Add forward* adds a card with the remaining ports. A typed port name is kept as typed. Local ports are checked when the tunnel starts, as for ssh (no live check, §14).
    - **Command:** the exact kubectl command, live; missing parts read `‹namespace›`, `‹name›`, `‹local:port›`.
  - Footer: *Save* / *Save and start* / *Cancel*, disabled with "Missing: namespace, service, a port" until name, namespace, resource and one complete forward are set; then "Ready" in Green.
  - As built:
    - The context list's first row is the current context (note "current context"); picking it unpins a pinned context. The contexts come from the kubeconfig as kubectl detection read it (§8: at start, on *Refresh*, after Save), not from a lookup of their own.
    - A lookup runs the first time its dropdown opens while the editor is open (cached until it closes, so a failed one is retried by reopening the editor); its error shows in the list ("Can't list namespaces: <kubectl's reason> · type a name instead").
    - In the namespace and resource lists, typing something no row has offers "Use “…”" (also Enter).
    - The resource dropdown is named *Resource*; its rows show the ports ("80/http · 9090/metrics") as the second line and the pod phase or "2/3 ready" as the note.
    - A forward card's chips show the resource's ports that no other card uses, while its port is empty ("ports from kubectl").
    - The ssh editor's footer is unchanged ("N problems"); "Missing: …" and "Ready" are kubectl's.
    - The success toast's *Copy* copies the address (`127.0.0.1:18080`), only for kubectl tunnels.
- **After Save:** the editor closes, the tunnel is added to the end of the list and selected; *Save and start* also starts it (*Starting…* with the yellow ring, then *Running*, and the success toast "web api is running on 127.0.0.1:18080" with *Copy*).
- **kubectl not found:** the kubectl type stays selectable; under the switch a banner says "kubectl wasn't found, so this tunnel can be saved but not started." with a link to *Settings › kubectl*. *Save and start* is disabled with that reason, and so is *Start* on kubectl rows (the row reads "kubectl not found"). Lookups in the editor show the same reason.
- **Settings › kubectl:** a card under *ssh*, same layout: Binary (path, source chip *from PATH* / *override* / the install folder found), Version, Kubeconfig (files and context count), Current context, Override ("path to a kubectl binary (empty: detect)"), *Refresh*. Not found: "not found on PATH" in Light Yellow and "kubectl tunnels can't start until kubectl is installed or its path is set above. ssh tunnels aren't affected." with *How to install kubectl* (the official install page). Also: override file missing, no kubeconfig, `KUBECONFIG` with several files. Waits for *Save* like the other settings.
- **Status bar:** while any kubectl tunnel exists, the kubectl client version follows the ssh version, or "kubectl not found" in Light Yellow.
- **Groups and tray:** the group editor's tunnel dropdown shows the mark and, for kubectl, "context / namespace" as the note; group rows and the tray menu list kubectl tunnels like ssh ones, with the mark (the native fallback menu has no icons).
- **Streamer mode (M11):** masked: the context name (field and free text), cluster server URLs and kubeconfig user names (added to the free-text filter from the contexts lookup), every port, the address, the kubectl and kubeconfig paths. Namespace and resource names are treated like host aliases: visible, and "namespace N" / "resource N" with *Hide host aliases and tags*. The editor's prefilled values follow the pre-filled input rule.
  - As built:
    - Core masks the kubeconfig's context names and users and its servers' hosts as soon as kubectl detection has read them. It also masks every kubectl tunnel's context and address, kubectl's binary, the kubeconfig files and the override.
    - Namespaces and resource names are numbered per kind in the order `tunnels.toml` lists them. A name no tunnel uses yet (one from a lookup) is "•••••" while names are hidden.
    - Ports in kubectl's lines are masked in context: after "-> ", "forwarding ", "connection for ", "[{".
    - The leak test (M11) covers the kubectl detail (failed and running, with debug lines), the editor, its context list and the Settings card.
- **Tour (M10):** the tour stays ssh-only. In its sandbox the type switch's kubectl half is disabled with the tooltip "Not part of the tour", and the sandbox never runs kubectl.
- **Motion:** as everywhere (§10 *Interaction and motion*): the type and kind switches slide, the dropdowns roll, a new row and new forward cards fade in with a 6 px drop over 180 ms, the list highlight slides to the new tunnel.

---

### Update check (milestone 14)

sshelf shows its own version and says when a newer release is out on GitHub. It never downloads or installs anything: *What's new* opens the release page. Reference boards: `Updates` (playable: every state, the toast, the rail dot), `Update-States` (states, toasts, version string, first run, how a check works), and the updated `A-Settings`, `Tour-Welcome` and `Feedback` (close buttons on toasts).

- **Version string**, embedded at build time by the sshelf crate's `build.rs`:
  - **Version:** the tag when the build is exactly at a release tag (`git describe --tags --exact-match --match "[0-9]*.[0-9]*.[0-9]*"`, tree clean), otherwise `dev`. Cargo's package version is not used.
  - **Commit:** `git rev-parse --short=7 HEAD`; `unknown` without git.
  - `SSHELF_VERSION` / `SSHELF_COMMIT` in the build environment win; the release workflow sets them from the tag and `GITHUB_SHA`. `build.rs` reruns when `.git/HEAD` or the refs change.
  - As built: "tree clean" means no changed tracked files (`git status --porcelain --untracked-files=no`), so untracked files don't make a tagged build `dev`; an empty override counts as unset. The rules live in `crates/sshelf/src/build_version.rs`, which `build.rs` includes and the app's tests run against temporary repositories. On Windows, `sshelf --version` started from a console (which gives a GUI program no stdout) writes to that console.
  - **Shown:** "sshelf 0.4.0 · a1b2c3d" next to the Settings title (mono 12, secondary text; copy on hover, a click copies "sshelf 0.4.0 (a1b2c3d)" with the "Copied" toast); `sshelf --version` prints "sshelf 0.4.0 (a1b2c3d)" and exits before any window, tray or lock; the update toast's "You have"; the request's `User-Agent`.
- **The check:**
  - `GET https://api.github.com/repos/Veritaris/sshelf/releases/latest` with `Accept: application/vnd.github+json`, `User-Agent: sshelf/<version>`, `If-None-Match` with the stored ETag (a `304` keeps the stored release and doesn't count against GitHub's limit), a 10 s timeout and the system proxy (`HTTPS_PROXY`). HTTPS with rustls; it runs in core on a worker, never on the UI thread, and its result arrives as an event.
  - Reads `tag_name` (a leading `v` is dropped), `html_url` and `published_at`. `/latest` never returns drafts or pre-releases.
  - **Compare:** `X.Y.Z` numerically per part. Only a newer latest release counts; equal or older is *Up to date*. An unparseable tag is a failed check.
  - **When:** at most once per 24 h, across starts too: about 5 s after start (after the window or tray is up) when the last answered check is a day old or there is none, else at that check plus 24 h; then every 24 h while sshelf runs; and on *Check now*. A failed check tries again after 1 h, and a start after a failed check checks after 5 s; GitHub's limit (`403` / `429` with `x-ratelimit-reset` or `retry-after`) waits until that time. A last check dated in the future (a clock set back) delays the first one by at most 24 h.
  - **Not checked:** while `update_check` is off (except *Check now*); in a `dev` build (*Check now* shows the latest release without comparing); in the tour's sandbox; before the first-run note was shown (below).
  - **Sent:** only that request. GitHub sees the IP address and the version in `User-Agent`.
- **Announcing a newer version:**
  - **Toast** (sticky): Lucide `circle-arrow-up` 18 in Blue, "sshelf 0.4.0 is available", "You have 0.3.2 · released 2 Nov", *What's new* (Lucide `external-link`; opens `html_url` in the browser and closes the toast) and *Skip this version*, and the close button. 400 px, bottom right; other toasts stack above it and still fade. Shown once per start for a newer, not skipped version (a daily check finding the same version doesn't bring a closed toast back); with the window hidden it waits until the window is shown.
  - **Rail dot:** the Settings rail item carries a Blue 8 px dot (ring in the rail colour, like the Tunnels dot) while a newer, not skipped version is known; the rail tooltip adds "sshelf 0.4.0 is available". Closing the toast keeps the dot.
  - **Skip this version:** stores it in `updates.toml`; the toast and dot go for that version. The next newer version is announced again. *Undo* in Settings clears it.
- **Settings › Updates:** a card at the top of the right column, above *ssh*.
  - Title with *Check now* (Lucide `refresh-cw`, disabled while checking).
  - *Check for updates* switch: "Asks GitHub for the latest sshelf release at start and once a day. Nothing about you, your hosts or tunnels is sent." It applies at once, without *Save* (like *Privacy*). `update_check`, missing = on.
  - One status line:
    - **Up to date** (Lucide `circle-check`, Green) "· checked 18:33"; a build newer than the latest release adds "this build (0.4.1) is newer than the latest release (0.4.0)".
    - **Checking GitHub…** with the pulsing ring.
    - **Update available:** a Blue-tinted box (`#24303D`): "sshelf 0.4.0 is available · released 2 Nov · you have 0.3.2", *What's new*, *Skip this version*.
    - **Skipped:** "0.4.0 skipped · you'll hear about the next version", *Undo*.
    - **Couldn't check** (Light Yellow, Lucide `triangle-alert`): "Couldn't check for updates", the reason ("No connection", "GitHub didn't answer within 10 s", "GitHub limits checks; trying after 14:05", "Unexpected answer from GitHub (HTTP 502)"), "last success 2 Nov 09:12 · tries again in 1 h".
    - **Off:** "Off · sshelf doesn't contact GitHub. Check now still works."
    - **Development build:** "Development build: no automatic checks. Check now shows the latest release: 0.4.0."
  - A failed check never shows a toast or a status bar error; only this line.
- **First run:**
  - **New users:** the welcome screen (§10 *Onboarding tour*) gets a second line under the cards: "sshelf checks GitHub for new versions at start and once a day. Turn it off in Settings › Updates." (Lucide `circle-arrow-up`). Showing the welcome screen counts as the note.
  - **Existing users** (`tour` already set, no `update_check_noted`): the first start with M14 shows a sticky toast instead of checking: Lucide `info` in Blue, "sshelf now checks for new versions", "It asks GitHub at start and once a day. Nothing about you is sent.", *Keep it on* / *Turn off* and the close button (closing keeps it on). The first check runs once it is answered or closed.
  - Either way `update_check_noted = true` is written and the note never comes back. A missing key is the default, so `settings.toml` keeps its version.
- **Streamer mode:** nothing here is sensitive. **Tour:** the sandbox never checks and shows no update toast.
- **Not in scope:** downloading or installing, pre-releases, release notes inside sshelf, a tray entry.
- **As built (M14):**
  - **HTTP client:** `ureq` 3 (blocking, one request on the checker's own thread) with rustls on ring and the bundled webpki roots; no OpenSSL or platform TLS, no gzip. The proxy comes from `ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` minus `NO_PROXY` (ureq reads them); the Windows system proxy setting is not read. ring compiles C for the target, which CI does natively on each OS.
  - **Core:** the app sends `Command::StartUpdates` (version, URL) on its first `logic` pass; only `main` names GitHub. The tour's sandbox ignores it and *Check now*. Tests use a local `http://` server, reached without a proxy. `SaveSettings` keeps `update_check` and `update_check_noted` as saved: only their own commands (the switch, the note) change them, so a Settings draft or the tour's state saved from older settings can't undo them.
  - **`updates.toml`:** `checked_at`, `succeeded_at` (RFC 3339, UTC), `etag`, `[latest]` (`version`, `url`, `published`), `skipped`, and `[failure]` (`reason` = `offline` / `timeout` / `rate-limited` with `until` / `http` with `status` / `bad-answer` / `other` with `message`). The ETag is sent only together with a stored release. An `html_url` outside `https://github.com/Veritaris/sshelf/` counts as an unexpected answer (it is opened in the browser).
  - **Reasons** besides the four above: "Unexpected answer from GitHub" (no release JSON, a tag that isn't `X.Y.Z`, a foreign page) and "Couldn't reach GitHub (<error>)" (TLS, proxy). A failure stored by an earlier run shows only while checks are on; before any answer the line reads "Not checked yet". "tries again in" rounds to minutes, then hours; a rate limit's line already names its time, so it has no "tries again". *Check now* also asks during a rate-limit wait. A `dev` build that hasn't checked says "Check now shows the latest release." without a version.
  - **Announcing:** the toast and dot need checks on or a check this run (a release stored by an earlier run isn't announced while the check is off). *Skip* from Settings also takes the toast away; *Undo* brings the dot back, not the toast (once per start).
  - **First run:** the existing-user toast and the welcome screen's note only appear in a run that checks (a test without a source shows neither). The welcome line sits 4 px under the tour line.
  - **Toasts:** the close button is 24 px on every toast, the sticky ones too (the boards draw 28 px ghost buttons there; the spec wins); its accessible name is "Close notification", never the toast's text (streamer mode). A message wider than the toast still widens it, the close button after it. Sticky toasts are drawn only outside the tour and fade in like the others.
  - **Rail:** the tooltip reads "Settings · sshelf 0.4.0 is available" before the shortcut (the board puts `Ctrl+4` in the middle); the item's accessible name stays "Settings". The dot fades in 180 ms.

## 11. Key generation (milestone 7)

Available when adding/editing a host.

- Generated in-process with the RustCrypto `ssh-key` crate (writes standard encrypted OpenSSH format; passphrase never appears in process arguments). Zeroize secrets after writing.
  - `ssh-key` 0.6.7 (0.7 is a release candidate). Encrypted keys use bcrypt-pbkdf with AES-256-CTR, like `ssh-keygen`. Generation runs off the UI and core threads (RSA takes seconds).
  - The passphrase travels as a `Secret`: wiped on drop, printed as `***` in debug output. egui may keep copies of typed text in its own buffers (e.g. undo history); that is outside sshelf's control.
  - The private key file is created owner-only *before* anything is written into it: mode `600` on Unix; on Windows its DACL is replaced by one full-access entry for the current user, with inheritance cut. A missing `~/.ssh` is created (`700` on Unix). If writing the `.pub` fails, the private key is removed again.
- Options:
  - Algorithm: **Ed25519 (default)**, ECDSA (256/384/521), RSA (3072/4096). Length field hidden for Ed25519.
  - Passphrase + confirmation; empty allowed with a visible warning.
  - KDF rounds, comment, file name (refuse to overwrite existing files).
    - KDF rounds default to 16 (`ssh-keygen`'s default), 1–1000. The comment defaults to `<alias> (sshelf)` and may not contain quotes, backslashes or line breaks (it ends up in the install command). The file name defaults to `~/.ssh/<id_ed25519|id_ecdsa|id_rsa>_<alias>`; the key or its `.pub` existing refuses the name.
- Hardware keys (`ed25519-sk`, `ecdsa-sk`) require device interaction → run `ssh-keygen` in a terminal. Optional, later.
- File permissions: mode `600` on Unix; on Windows restrict the file ACL to the current user (OpenSSH rejects keys with open permissions).
- As built, the editor's *Generate key…* opens a window (not a modal: the editor stays usable for saving, and the window outlives it) with the options, then the fingerprint, the public key (with Copy) and the follow-ups as buttons rather than checkboxes, since some need the host saved first: *Use in this host* adds the lines below to the draft (saved with the editor's Save and validation; `IdentityFile ~/.ssh/<name>` when the key is in `~/.ssh`), *Add to agent*, *Install on server* (enabled once the alias is in the saved config). While a new host's alias is typed, the window follows it (install target, default file name and comment). An existing file is reported when *Generate* is clicked: the UI does not look at the disk.
- Follow-up actions (checkboxes):
  - Add `IdentityFile` + `IdentitiesOnly yes` to the host block.
  - Add to agent (`ssh-add`; on macOS `--apple-use-keychain`).
    - Without a passphrase, `ssh-add <key>` runs directly (from the ssh binary's directory, §8) and the agent status refreshes. With one, it runs in the user's terminal, so the passphrase is typed into `ssh-add` itself and never passes through sshelf.
  - Install public key on the server: pipe the public key into `ssh <alias> 'mkdir -p ~/.ssh && chmod 700 ~/.ssh && cat >> ~/.ssh/authorized_keys'`. Runs in a terminal (the first login usually needs a password). No dependency on `ssh-copy-id` (absent on Windows).
    - As built: terminals are launched without a shell (§10), so nothing can pipe. The key line travels inside the remote command instead, in single quotes (it never contains quotes: base64 plus a checked comment): `ssh -t -- <alias> "umask 077 && mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys && (grep -qxF '<key>' ~/.ssh/authorized_keys || echo '<key>' >> ~/.ssh/authorized_keys) && echo '…installed. Press Enter to close.' && read -r _"`.
    - It never adds a duplicate line, creates `~/.ssh` and `authorized_keys` private (`umask 077`), has no `;` (Windows Terminal splits its command line there), and waits for Enter so the result stays visible (`-t` gives the password prompt and `read` a terminal). It needs a POSIX shell on the server; Windows servers are not supported.

---

## 12. Milestones

Each milestone ends with its acceptance criteria met, tests green on all three OSes in CI, and a manual check by the owner before continuing.

### M0 — Scaffold
- Workspace with crates from §2, `fake-ssh` stub, CI for Windows/macOS/Linux, MIT license, `CLAUDE.md` pointing to this spec and the style reference repo.
- **Done when:** empty app window opens on all platforms; CI builds and runs (empty) tests.

### M1 — Lossless parser
- `sshconf` CST, `Include` resolution, host vs pattern classification.
- Round-trip test corpus: tabs, `Key=value`, multi-alias, negations, quotes, CRLF, trailing whitespace, comments everywhere, `Match`, `Include` with globs and inside `Host`.
- **Done when:** parse → write is byte-identical for every corpus file and for the owner's real config.

### M2 — Host browser (read-only)
- Sidebar layout, host list with source file, patterns section, `ssh -G` resolved panel, file watching and reload.
- ssh binary discovery + Settings page showing binary, version, agent status (incl. Windows service check).
- `hosts.toml` metadata (tags, notes, favorite, color) with `version` field and migration scaffolding.
- Orphan detection, counter, tooltip, relink/delete popover.
- Terminal launch ("Connect") with per-OS defaults and template setting; history recording.
- **Done when:** the owner's real config displays correctly and nothing is ever written to it.

### M3 — Tunnels and groups
- `tunnels.toml`, tunnel and group CRUD in the UI.
- Supervisor with owners/refcounting, port pre-checks, `L`/`R`/`D` forwards, bind-address warning, stderr log, degraded groups, stop-reason display.
- Supervisor tests against `fake-ssh`.
- **Done when:** real tunnels and groups start/stop reliably on all three OSes; overlapping groups behave per §7.

### M4 — Host keys, auth feedback, close behavior → **MVP complete**
- Unknown-key confirmation modal and changed-key replace modal (§9), including jump-chain ordering.
- Clear "key not in agent" feedback.
- Close modal (without tray option yet), remembered choice + reset.
- **Done when:** a brand-new server and a reinstalled server can both be handled entirely from the app; quitting with running tunnels always asks (unless the user chose to remember "stop and quit").

### M5 — Config editing
- Structured form + raw editor on the same CST node, `ssh -G -F` validation, rename with app-data update.
- Write policy (§4): in place, new hosts before first wildcard block, target-file picker, read-only unwritable files, backups, atomic writes, external-change conflict handling.
- **Done when:** edits produce minimal diffs (verified with `git diff` on a versioned config) and invalid edits cannot be saved.

### M6 — Tray
- Tray icon and menu (§10), *Minimize to tray* in the close modal, supervisor fully functional with the window hidden.
- **Done when:** tunnels and groups can be controlled from the tray with the window hidden on all supported platforms.

### M7 — Key generation
- Everything in §11.
- **Done when:** a new host can be created, a key generated, installed on the server, and connected to without leaving the app (except the terminal for the first password login).

### M8 — Final polish
- Connection health indicator per host: `ssh -o BatchMode=yes -o ConnectTimeout=3 <alias> true` → reachable / auth OK / latency.
  - As built: only on request — *Check* in the host detail, *Check all* in the list (the hosts the list shows, after search and filters). Never automatically: that would log in to every server (fail2ban and the like). At most 4 run at once; each is limited to 15 s overall (jump chains). Results live in memory for the session.
  - Outcomes, classified like tunnel failures (§7): *Auth OK* with the login time; *reachable, no key accepted* (with the §8 agent hint); *host key not known yet* / *host key changed* (hints point to Connect or a tunnel, where §9 handles them; the check itself never prompts); *unreachable* with ssh's reason. A painted mark per host in the list (yellow ring while checking, then green / yellow / red), the details on hover and in the detail pane.
- Optional auto-reconnect for dropped tunnels (with backoff).
  - As built: per tunnel, `auto_reconnect = true` in `tunnels.toml` (missing means off; the file stays version 1), set in the tunnel editor. When such a tunnel exits on its own while it still has owners and the failure is not authentication or a host key (those need the user, §8–§9), the supervisor restarts it after 2 s, then 4, 8, 16, 32, 60 s; a run that lasted 60 s starts the waits over. The port pre-check runs before each restart; a taken port waits for the next attempt.
  - While waiting the tunnel stays *Failed* (groups count it as down) with "Reconnecting in N s (attempt k)" in the tunnel, group and tray views and the log. A manual start, Stop, or the last owner leaving cancels the wait. The supervisor sleeps until the next restart with a channel timeout, not a poll.
- "Start on app launch" per group.
  - As built: `start_on_launch = true` on a group in `tunnels.toml` (missing means off), set in the group editor ("Start when sshelf starts"; the list says "starts with sshelf"). After the first config load, core starts those groups once, through the normal start: host keys are checked first, and an unknown key brings the window up to ask (§9). A config or ssh problem is reported instead.
- `ProxyJump` chain visualization (breadcrumb first, graph later).
  - As built: "Route: you › bastion › db-gw › web" in the host detail, for hosts whose effective config has a `ProxyJump` (no extra `ssh -G` for the rest). Core walks the chain like the host key checks do (§9: nested jumps first, each hop once, depth-limited) with `ssh -G` per hop, off the UI thread. Hops that are hosts of the config are links that select them; others show their address on hover. A `ProxyCommand` shows as a "ProxyCommand" step with the command on hover. No graph yet.
- Optional hardware-key generation via terminal `ssh-keygen`.
  - As built: *Ed25519-SK* and *ECDSA-SK (security key)* in the key dialog. *Generate* opens the user's terminal with `ssh-keygen -t ed25519-sk -a <rounds> -C <comment> -f <file>` (§10's terminal rules); the device touch, PIN and passphrase all happen there, so sshelf never sees them. *Done* then reads only `<file>.pub` (never the private key file, §1) and shows the fingerprint and the usual follow-ups; *Add to agent* uses the terminal, since sshelf cannot tell whether a passphrase was set. Windows' built-in OpenSSH talks to the key through Windows Hello.

### M9 — UI redesign
- Everything in §10 *Visual design*, *Interaction and motion* and *Dropdown*, plus the M7/M8 design notes there, applied to every existing screen and the tray. Reference boards: `docs/design/` (its README lists what each one shows).
- Order: theme constants in `theme.rs` (colours, radii, timings), the Lucide font, the frameless rounded window (drag strip, edge resize), the shared widgets (icon rail, sliding highlight, switch, dropdown, truncated label, copy-on-hover row, animated tooltip and context menu, toast), then the screens, then the sshelf tray menu. Since `sshelf` is a binary crate (unused code fails `-D warnings`), each widget and constant lands in the commit of the first screen that uses it.
- No behaviour changes beyond those sections. New `settings.toml` keys (`host_list_style`, `cards_group_by`, `reduce_motion`, `tray_menu`) default when missing, so the file version stays the same.
- Display extras taken from the boards (they show data the app already has or reuse existing commands):
  - Lists: a tunnel status dot on host rows; the search match highlighted in the alias; "N of M match"; a "+wildcard" chip; "1 of 3 running" in Tunnels; two-line tunnel rows with *Cancel* / *Retry* / *Review…*; "running · 2/2" on groups. Cards show `user@host:port` from `ssh -G`, which a card runs (once per config load) when it scrolls into view; `ssh -G` only reads the config and never connects.
  - Right-click menus with existing actions only: hosts (*Connect*, *New tunnel…*, *Edit*, *Rename…*, favorite, *Copy alias*, *Copy ssh command*), tunnels (*Start*, *Stop*, *Edit*, *Copy ssh command*, *Delete…*).
  - Host detail: *Overview* / *Effective config* / *Tunnels · N* tabs; "ssh -G · N s ago" and *Refresh* on the effective config.
  - Tunnels: uptime in the status chip and `HH:MM:SS` on log lines (core exposes the supervisor's existing start `Instant` and adds a timestamp per `LogLine`); a failure banner with *Show log*; the editor's command preview (from the real ssh arguments) and *Save and start*.
  - Groups: *Stop group* and *Add tunnel* in the detail; *New group* disabled with the reason while no tunnels exist.
  - Dialogs: deletes confirm in a dialog; rename lists what else changes and says up front which running tunnel blocks it (§5); the unknown-key dialog shows the jump hop it is on, the `[host]:port` form, *Copy* for the compare command and how many lines it adds.
  - Settings: one *Agent* row; *Copy command* for the agent hint.
  - Tray: a status header, *Retry* on degraded groups, a failed tunnel opens the window on that tunnel, a banner while a host key waits; tooltip "sshelf · N tunnels running".
  - *Check all* stops when clicked again (above) through a new `CancelHealth` command: queued checks are dropped, running ones finish.
- **Done when:** every screen matches its board (layout, colours, radii, icons, timings); *Reduce motion* turns all animation off; the sshelf tray menu works with the window hidden or falls back to the system menu; UI tests and CI pass on all three OSes.

### M10 — Onboarding tour
- Everything in §10 *Onboarding tour*, with the §8 sandbox ssh and the `fake-ssh` changes below. Reference boards: `docs/design/Tour-*.dc.html`; how-to and authoring guide: `docs/onboarding-tour.md`.
- Order:
  1. `fake-ssh` becomes a library (`fake_ssh::run(args) -> ExitCode`) plus its thin test binary. The existing tests pass unchanged. It gains:
     - a scenario file (`FAKE_SSH_SCENARIOS`: alias → scenario, with `after_install`);
     - per-host `-G` output from the sample bundle;
     - a fake `authorized_keys` per sample server;
     - `--as ssh-add`.
  2. `sshelf --fake-ssh …` in `main.rs`, before any window, tray or lock. Check it early on a Windows release build (GUI subsystem, piped stdio, exit codes).
  3. Core:
     - the ssh tool becomes program + prefix args + env, and so do companions (§8);
     - the sandbox options on `Core::start` (paths, ssh, no terminals, `TourWouldRun` events, install through fake-ssh, no start-on-launch groups);
     - the `tour` settings key.
  4. UI:
     - the `tour/` module (state machine, steps and glossary as data, target registry, overlay and popover, glint, the sandbox builder and the sample bundle, the welcome screen);
     - target registration in the views;
     - the drag strip chip, the disabled Settings rail item and the tray header;
     - then the chapters one by one.
  5. Settings › Help, and *Start a chapter…* with prerequisite replay.
  6. The two small additions (Tunnels rail dot; key fixes in the "no key accepted" tunnel banner).
- Tests:
  - **Isolation:** the whole tour is run headless (`egui_kittest`, *Reduce motion*) with *Show me* on every step. It asserts every step's target is registered, and that nothing outside the sandbox dir was opened: point `Paths` for the user's Core at a dir that fails on access, and put an `ssh` that panics on `PATH`.
  - **fake-ssh:** unit tests for the scenarios, `after_install` and `--as`.
  - **Glossary:** a test fails when a `[term]` has no glossary entry, or an entry is unused.
- **Done when:**
  - a first launch shows the welcome screen;
  - the tour plays all 27 steps on the real screens with the overlay as on `Tour-Step`;
  - nothing outside the sandbox is touched, and it is deleted afterwards;
  - the tour and each chapter can be replayed from Settings;
  - CI passes on all three OSes.

### M11 — Streamer mode
- Everything in §10 *Streamer mode*. Reference boards: `docs/design/Streamer-Mode.dc.html` (playable), `docs/design/Streamer-States.dc.html`, and the Privacy card on `A-Settings.dc.html`.
- Order:
  1. Core: the `streamer_mode` / `streamer_hide_aliases` settings keys. A `Redactor` built from the loaded config, tunnels and home path: known values by kind, the IPv4/IPv6 patterns, alias numbering. It is rebuilt on every config load and tunnel change, and is pure and unit-tested.
  2. UI plumbing: one place the views read the current redactor and state from. The `masked` widget (field, free text, alias) with hold to peek, the cursor and the tooltip. Copy paths that bypass masking.
  3. Settings › Privacy, the drag-strip chip and its menu, the toasts, the tray switch and tray masking.
  4. Structured fields everywhere (host detail, cards, effective config, tunnel rows and editor preview, groups, Settings, dialogs), then the free-text filter (logs, status bar, toasts, tooltips, notes, health results, window title), then the editor cover.
  5. *Hide host aliases and tags*.
- Tests:
  - **Redactor:** unit tests: every kind, longest match first, IPv6 forms, an alias that is an IP, numbering stays stable.
  - **Nothing leaks:** a kittest walk renders every main screen and dialog with streamer mode on, over a config whose sensitive values are all distinctive markers. It asserts that no marker appears in any rendered text (the egui output's text shapes), the tray menu model, the window title or the toasts.
  - **Peek and copy:** pressing a token shows the value only while held; copy puts the real value on the clipboard.
- **Done when:** with streamer mode on, no sensitive value from the config, tunnels or home path is visible anywhere in sshelf without holding it; turning it on or off works from Settings, the tray and the chip; the setting survives a restart; CI passes on all three OSes.

### M12 — Surviving a crash
- Everything in §7 *Surviving a crash*: `running.toml`, taking tunnels over at start on macOS and Linux, watching their exit, the toast.
- Tests (`crates/fake-ssh/tests/adopt.rs`), with a real crash: the test binary runs itself again as a sshelf that starts a tunnel and aborts.
  - A tunnel left by a crash is taken over, and *Stop* ends it.
  - An adopted tunnel that dies fails with "ssh exited" and reconnects as a new process.
  - A changed tunnel is neither taken over nor killed; a reused pid is dropped.
  - A clean quit leaves nothing recorded.
- **Done when:** on macOS and Linux a tunnel survives `kill -9` of sshelf and shows as *Running* after the next start; *Stop* ends it; CI passes on all three OSes.

### M13 — kubectl tunnels
- Everything in §7 *kubectl tunnels*, §8 *kubectl* and §10 *kubectl tunnels*. Reference boards: `docs/design/Tunnel-Kubectl.dc.html`, `Tunnel-Kubectl-New.dc.html` (both playable), `Tunnel-Kubectl-New-Steps.dc.html`, `Kubectl-States.dc.html`, and the updated `A-Tunnels`, `Tunnel-States`, `Tunnel-Edit`, `A-Settings`.
- Order:
  1. Core model: the tunnel type, the kubectl fields, `tunnels.toml` v2 with its migration, `kubectl_path` in `settings.toml`, `program` in `running.toml`.
  2. kubectl discovery and the Settings card; the status bar entry.
  3. `fake-ssh --as kubectl`; the supervisor runs kubectl tunnels: command, Forwarding lines, failures, drops, reconnect, crash survival.
  4. Tunnels view: marks, rows, filter, detail, *Copy command*.
  5. The editor: the type switch, the lookups (core worker + events), the pickers, forwards with suggestions, the command preview, validation, the not-found state.
  6. Groups and tray, streamer mode, the tour sandbox's disabled switch.
- Tests:
  - **Model:** v1 → v2 migration with backup; round trip of both types; an older version stays read-only.
  - **Parsing:** command building; Forwarding lines (one and several forwards); every failure kind from fixture lines; the lookups' JSON (fixtures with made-up names).
  - **Supervisor** (`crates/fake-ssh/tests/`): Starting → Running only after every forward; each failure; reconnect yes / no per kind; `lost connection to pod` with kubectl still running; a group mixing both types.
  - **UI** (`egui_kittest`): create a kubectl tunnel through the editor from empty to *Running* against `fake-ssh --as kubectl` with canned lookups; the not-found state; the streamer walk extended with kubectl markers (context, server, ports, paths).
  - **Tour isolation:** the M10 isolation test also puts a `kubectl` that panics on `PATH`.
    - As built: a child process's panic can't fail the test by itself, so the panicking kubectl (fake-ssh copied as `kubectl` next to a `panic-if-run` file) first leaves a `ran` file, and the test checks it isn't there.
    - The app-level isolation test puts it on the user's `PATH`, with a `kubectl_path` that doesn't exist (no fallback, as for ssh).
    - `fake-ssh/tests/sandbox.rs` puts it on the sandbox Core's own `PATH` and checks detection ("Not part of the tour"), a lookup and a kubectl tunnel's start ("kubectl not found").
- **Done when:** a kubectl tunnel to a real cluster (the owner checks with k3s) can be created, started, stopped, reconnects after its pod is deleted, and works in a group and from the tray; Settings shows kubectl found and not found; ssh tunnels behave as before; CI passes on all three OSes.

### M14 — Update check and version
- Everything in §10 *Update check*, and the close button on every toast (§10 *Layout*, *Feedback*). Reference boards: `docs/design/Updates.dc.html` (playable), `Update-States.dc.html`, and the updated `A-Settings`, `Tour-Welcome`, `Feedback`, `Interaction`.
- Order:
  1. The version: `build.rs` (git, the `SSHELF_*` overrides), a `version` module the app reads, `sshelf --version` in `main.rs`, the release workflow setting `SSHELF_VERSION` and `SSHELF_COMMIT`.
  2. The close button on every toast, and sticky toasts in the toast stack.
  3. Core: `update_check` / `update_check_noted` settings keys, `updates.toml` (version 1), the checker (HTTP client with rustls and proxy support; pick a small blocking one such as `ureq`, verify before adopting), the schedule, ETag and rate-limit handling, events.
  4. UI: the Settings version chip and *Updates* card, the update toast, the rail dot and tooltip, *Skip* / *Undo*, the first-run line and toast.
- Tests:
  - **Version:** the tag / dev / unknown rules on a temporary git repo and with the overrides.
  - **Checker** (no request to GitHub in any test; a local HTTP server or an injected transport): parsing a release JSON fixture; compare (newer, equal, older, `v` prefix, bad tag); `304` with ETag; `403` / `429` waiting until the reset; timeout; dev build and off never request; the 24 h and 1 h schedule with a fake clock.
  - **UI** (`egui_kittest`): every Settings state; the toast's actions; the dot; closing any toast; the first-run toast for an existing user and none for a new one; the tour sandbox never checks.
- **Done when:** a release build shows its tag and commit in Settings and `--version`, a local build shows `dev`; with an older build installed, the next start shows the toast and dot for the latest release, *Skip* silences it, *What's new* opens the release page; turning the check off stops all requests; every toast can be closed; CI passes on all three OSes.

---

## 13. Final shape (summary)

A small, trustworthy desktop tool where:
- the ssh config remains the single source of truth and is edited with minimal, comment-preserving diffs;
- every host shows its effective configuration and health at a glance;
- tunnels and groups of tunnels start with one click from the window or the tray, with clear status and logs;
- host keys and keys in general are handled safely and transparently;
- all app data is plain, versioned TOML that the user can read, back up and version-control.

## 14. Ideas parked for later (not scheduled)

- Embedded terminal.
- Support for alternative ssh distributions on Windows.
- Re-detect a tray host appearing or disappearing on Linux without *Refresh* (watch `NameOwnerChanged` for `org.kde.StatusNotifierWatcher`).
- From the M9 design boards (`docs/design/`), not built yet:
  - Hosts: *Delete host…* and *Show in config file* in the host menu (Hosts-Interactions).
  - Keyboard shortcuts beyond `Ctrl+1…4`: `Ctrl+K` search, `Esc` clears, `Enter` / `E` / `F2` / `Del` on rows, `Ctrl+S` / `Esc` in editors, ↑↓ in lists (Hosts-Interactions, Host-Edit, Empty-States, Tunnel-States).
  - Effective config: "Set for this host / All" with a "From" column naming the line each value came from; "Open line N" from an `ssh -G` error (Host-Config).
  - Host editor: undo per field (Host-Edit).
  - Host editor: "Re-apply my edit" onto a file that changed on disk, with the disk diff (Host-Edit-Raw).
  - Raw editor: syntax highlighting, error marks in the gutter, "Did you mean …?" (Host-Edit-Raw).
  - A *Browse…* file picker for the generated key's file (Dialog-KeyGen); `IdentityFile` has one now (§10 *Host editor*).
  - Health: *Cancel* for a single running check (Host-Health).
  - Toasts with *Undo* (Feedback).
  - An error queue ("+1 more") and action links in errors such as "Change in Settings" (Feedback).
  - Tunnels: *Duplicate* and *Add to group ›* in the tunnel menu (Tunnel-States).
  - Tunnels: a "waiting · confirm host key" row state with *Show* (Tunnel-States).
  - Tunnels: the process holding a busy port (pid and name), and a live port check while editing (Tunnel-States, Tunnel-Edit).
  - Tunnels: an "I want to listen on all interfaces" switch that gates Save (Tunnel-Edit).
  - Changed host key: the `known_hosts` line number of the stored key (Dialog-HostKey-Changed).
  - Unknown host key: "host 1 of 2" counts in the jump chain (Dialog-HostKey).
  - Settings: *Open* the app data folder in the file manager (A-Settings).
