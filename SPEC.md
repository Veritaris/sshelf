# sshelf — Project Specification

sshelf is a cross-platform desktop app (Windows, macOS, Linux) that puts a visual layer over the user's real `~/.ssh/config`: browse hosts, see their effective configuration, edit them safely, launch SSH sessions, and manage port-forwarding tunnels and groups of tunnels.

- Language: Rust
- GUI: egui / eframe
- License: MIT
- Signing: none for now (unsigned builds for all platforms)

This document is the source of truth for scope and design. It describes the **MVP**, the **iterations** after it, and the **final shape**. Implement milestone by milestone; do not pull later-milestone features forward unless this document is updated first.

---

## 1. Core principles (hard rules)

1. **`ssh_config` is the source of truth for hosts.** The app never keeps its own list of connectable machines. A host exists if and only if it exists in the user's ssh config (including `Include`d files).
2. **Everything else lives in the app's own data folder**, linked to hosts by alias. If app data references an alias that no longer exists, that is an *orphan* and is surfaced to the user (see §6), never silently deleted.
3. **Lossless config editing.** Lines the user did not change are written back byte-for-byte: comments, blank lines, indentation, tabs, CRLF, ordering, unknown directives — all preserved.
4. **Shell out to the system `ssh`.** No SSH library for connections or tunnels. This keeps agent, `ProxyJump`, `ProxyCommand`, `known_hosts` and all user config behavior identical to the command line.
5. **Never read existing private keys.** The app only references `IdentityFile` paths and defers to the agent. It never stores passwords or passphrases. It *may* generate new keys (milestone 7); key material exists only in memory during generation and is zeroized afterward.
6. **Effective configuration comes from `ssh -G <alias>`**, not from reimplementing ssh's resolution rules.
7. **Never bypass host key checks silently.** Unknown and changed host keys always go through an explicit user confirmation (see §9).
8. **Safe writes.** Every write to a user file is atomic (write temp file, then rename). Before the first write to an ssh config file in a session, a timestamped backup is created.
9. **Small, verifiable steps.** Each milestone ends in a state that can be verified by hand and by tests before moving on.

### Non-goals

- Embedded terminal emulator (sessions open in the user's terminal app).
- Cloud sync, accounts, or any network service of our own.
- Password storage or password-based auth automation.
- PuTTY/plink, WSL, Cygwin, MSYS2 or Git-for-Windows ssh support (see §8).
- Features depending on `ControlMaster` / `ControlPersist` (unsupported by Windows OpenSSH).

---

## 2. Architecture

Cargo workspace with UI-independent core crates:

| Crate | Responsibility | Depends on egui? |
|---|---|---|
| `sshconf` | Lossless `ssh_config` parser/editor (CST), `Include` resolution, write-back | No |
| `sshelf-core` | App data model (metadata, tunnels, groups, settings, history), storage + migrations, ssh binary discovery, `ssh -G` resolution, host key checks, tunnel supervisor | No |
| `sshelf` (app) | egui/eframe frontend; renders state, sends commands to core | Yes |
| `fake-ssh` (test bin) | Scriptable stand-in for `ssh` used by supervisor tests | No |

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
| `settings.toml` | ssh binary override, terminal template, close behavior choice, UI prefs |
| `hosts.toml` | Per-host metadata keyed by alias: tags, notes, favorite, color, display order |
| `tunnels.toml` | Tunnel definitions and groups |
| `history.toml` | Recent connections launched from the app (for the tray menu) |

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
- Tunnels stop when the app quits (no detached background processes).

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
- **Auth under `BatchMode`:** when a tunnel fails because a key is not available in the agent, show a clear message ("key not loaded in agent") with a hint, not a raw error.

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
  - As built (M9): typing the host's alias; *Don't connect* is the primary button and *Replace key* uses Dark Red.
- Replace = `ssh-keygen -R <host> -f <known_hosts>` (creates `.old` backup), then the unknown-key flow above to fetch and store the new key.
- Never replace automatically.

### How sshelf applies this

- Every tunnel or group start first checks, without connecting, each host on the way (jump hosts first, then the destination): `ssh -G` for the lookup name and known-hosts files, `ssh-keygen -F` in each user and global file. The first unknown key is fetched and shown; *Accept* stores it and continues with the next host, *Reject* starts nothing (for a group: none of its tunnels). The fetch runs the command above, which logs in and runs `exit` if the key is accepted for authentication.
- A changed key can only be seen when ssh connects: the tunnel fails, and its failure is classified from ssh's messages (host key changed / unknown, authentication, other). *Review changed key…* walks the same chain, fetches each host's key (of the stored key's type when the server offers it) and shows the first host whose key no longer matches.
- `known_hosts` files are written only by these flows: appends are atomic (temp file + rename), and each file gets a timestamped backup (`known_hosts.sshelf-<time>.bak`) before its first change in a session; `ssh-keygen -R` additionally keeps `.old`. The global (system) files are never written; a key stored only there cannot be replaced from the app.
- Interactive sessions (Connect) are not checked: ssh asks about host keys in the terminal itself.

---

## 10. UI

### Layout

- **Sidebar:** an icon rail with Hosts · Tunnels · Groups · Settings, plus the orphan warning indicator (see *Visual design*).
- **Main area:** list + detail pane.
- **Feedback:** successes (settings saved, host saved/added/renamed, terminal opened) show as a toast in the bottom-right corner that fades after ~3 s; errors stay in the status bar until dismissed.
  - Hosts: list (search; filter chips *All*, *Favorites* and one per tag, one filter at a time; *New host*) with a separate collapsible *Patterns* section; detail pane with source file, metadata, actions (Connect, New tunnel, Edit, Rename…), and the `ssh -G` resolved panel.
  - *Edit* replaces the detail pane with the editor (§5): *Form* and *Raw text* tabs over the same block, the problems found, the changed lines as they will be written, and Save / Check / Discard. Picking another host closes an editor without changes. *New host* opens the same editor with an alias field and the target file picker (§4).
  - Tunnels: list with status, start/stop, forwards, stderr log.
  - Groups: list + detail pane. Each row has the group's switch and its aggregate status (running / degraded / stopped, with counts); the detail shows the per-tunnel states.

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
- Cmd+Q on macOS and Quit from the tray go through the same logic. Quit from the tray never minimizes: with nothing running it quits; with a remembered *Minimize to tray* and running tunnels it asks, with the window shown, offering *Stop and quit* and *Cancel*.
- The close decision runs in `App::logic`, so it works while the window is hidden; asking shows the window first.
- A host key prompt (§9) brings the window back, since a start from the tray may wait for it. Other errors while hidden show in the status bar when the window is shown again.

### Tray (milestone 6)

- Menu: last N connected hosts (click → open terminal session), groups with start/stop, active tunnels with stop, Show window, Quit.
  - *Connect* submenu: the last 8 distinct hosts launched from sshelf that still exist (any alias counts for its host), newest first.
  - *Groups*: "Start X", or "Stop X" with "(starting)" / "(2/3 running)" while not fully up.
  - *Active tunnels*: "Stop X" for a tunnel started by hand (it releases that start, §7); a tunnel only groups hold is shown disabled, "X (used by Work)".
  - Clicking the icon itself shows the window; the menu opens on right click. The tooltip counts running tunnels, and the icon gets a green dot while any run.
- **Tray menu (redesign):** left click on the icon shows and focuses the window; right click opens the **sshelf menu**, a small borderless, always-on-top viewport docked above the icon (16 px corners, same widgets and states as the window: group switches, active tunnels with stop, Connect list, *Show window*, *Quit*). It closes on focus loss or `Esc`.
  - The native `tray-icon` menu with the same items stays as the fallback: used when the icon's position isn't known (`TrayIconEvent` gives no rect, e.g. some KSNI hosts) and when *Settings › Window & tray › Tray menu* is set to *System menu* (`tray_menu = "sshelf" | "system"`, default `"sshelf"`).
  - The flyout must work while the main window is hidden, so it is driven from `App::logic` like the rest of the tray. Verify during implementation that eframe renders a secondary viewport while the root viewport is hidden; if it can't, the fallback menu is used while hidden.
- The tray works from `App::logic`, which eframe keeps calling while the window is hidden (core events and tray clicks request a repaint): the menu stays current and actions run without the window.
- Backends (`tray-icon` crate): native on Windows and macOS; on Linux the KSNI backend (StatusNotifierItem over D-Bus, no GTK).
- If the platform has no tray (e.g. GNOME without an extension), don't offer *Minimize to tray*. On Linux a tray exists when `org.kde.StatusNotifierWatcher` has an owner on the session bus; this is checked at startup and with *Refresh* in Settings, which shows the result.
- macOS: create on the main thread; consider hiding the Dock icon while minimized.

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
  - Host colours (*Notes & tags*) are swatches that never collide with status colours: Blue `#61AFEF`, Cyan `#56B6C2`, Magenta `#C678DD`, Dark Yellow `#D19A66`, Comment Grey `#5C6370`, and *none*. A colour stored in `hosts.toml` outside these shows as a sixth, selected swatch; nothing is rewritten until the user picks another.
- **Radii:** 1 px (table rows, code bars, the inner side of joined pieces), 4 px (checkboxes only), 8 px (controls, cards, menus, chips), 16 px (window, dialogs, tray flyout, large panels). No other values.
- **Type:** UI font Inter (Ubuntu acceptable), mono JetBrains Mono for values, paths, aliases and logs. Sizes: 22/600 titles, 20/600 editor, Settings and cards-pane titles, 17/600 dialog titles, 16/600 pane headings, 13/600 sections, 14 list items, 13 body and controls, 12 secondary, 11 overlines. Spacing in 4/8 px steps.
- **Icons:** Lucide (ISC licence) everywhere, loaded as an icon font (`lucide.ttf` from the `lucide-static` package) in its own `FontFamily::Name("lucide")`. No emoji and no self-drawn icons; status dots and the app icon are painted shapes. Sizes: 16 default, 14 in small buttons and inline text, 12 in keyboard hints, 20–28 in empty states. The icon names per action are on the canvas (*Components › Icons*).
- **Status marks:** a filled dot is a settled state (running green, stopped grey, failed red); a ring is a transition (starting yellow, stopping grey, waiting for the user blue). The app and tray icon carry one dot: green while anything runs, yellow while starting or degraded, red when something failed.
- **Destructive confirmations:** the safe action is the primary button and gets focus; the destructive one uses Dark Red.

### Interaction and motion

All timings below are final and live as constants in one place (`theme.rs`). egui's `animate_value_with_time` only interpolates linearly, so animations go through a small tween helper (`motion.rs`) that applies these curves to egui's frame time and requests repaints only while something moves. Easing "in-out" is `cubic-bezier(.65,0,.35,1)`, "out" is `cubic-bezier(.2,.8,.2,1)`, "in" is `cubic-bezier(.4,0,1,1)`. The playable reference is the canvas board *Interaction and motion*.

| Element | Cursor | Motion | Duration | Easing |
|---|---|---|---|---|
| Buttons, rail items, list rows, cards, tabs, switches, chips, star, menu items | `PointingHand` | hover background fade | 80 ms | out |
| Selection in rail, lists, tabs, segmented controls | — | **sliding highlight**: the active background (or tab underline) moves and resizes from the old item to the new one; text colour cross-fades | 180 ms | in-out |
| Hover highlight inside dropdowns and context menus | — | slides between rows; fades out in place when the pointer leaves; on re-entry it appears on the hovered row without sliding | 50 ms slide, 120 ms fade | in-out / out |
| Switch | `PointingHand` | knob slides, track colour fades | 200 ms | in-out |
| Tooltip | — | appears after 400 ms of hover, then fades in with a 3 px slide; fades out the same way; the next tooltip within 300 ms shows at once | 120 ms | out (in) / in (out) |
| Context menu (right click) | — | fades in and scales 98 → 100 % from the click point; fades out | 120 ms | out |
| Copyable value row | `Copy` | row background and copy icon fade in; click copies, toast "Copied <field>" | 80 ms | out |
| Text inputs, raw editor | `Text` | focus outline fade | 80 ms | out |
| Disabled controls | `NotAllowed` | none; a tooltip says why it is disabled | — | — |
| Toast | — | fades in with a 6 px rise; fades out after 3 s; hover pauses | 120 ms | out |
| Starting / waiting ring | — | opacity pulses 100 → 40 → 100 % (`request_repaint_after`, only while visible) | 1 s loop | in-out |

- **Reduce motion** (*Settings › Appearance*, `reduce_motion = true`): every duration becomes 0 and the pulse is static.
- **Switch colour:** the on-colour is a parameter, Magenta when not passed. The boards use the default everywhere, group switches included (running state is shown by the status dot next to them).
- **Favorites:** the star next to the host title is a toggle button (adds / removes the favorite); its tooltip says what a click will do; the fill fades in 120 ms.
- **Copy on hover:** in host detail, effective config and tunnel forwards, hovering a value row shows a copy icon 8 px right after the value. A long value truncates earlier to leave room for the icon, so the icon is always visible. A click anywhere on the row copies the full value (never the truncated text).
- **Truncated text:** every alias, path, value and log line that may not fit goes through one wrapper (`truncated_label`). It truncates with an ellipsis (in the middle for paths, so the file name stays visible) and shows a tooltip with the full value, with the tooltip fade above, only when the text was actually cut.
- **Forwards** are edited as separate fields, never as one `host:port` string: *L*: bind address, local port → IP / host on the remote machine : remote port. *R* mirrors it: bind on server, remote port → IP / host on this machine : local port. *D*: bind address and local port. The host editor writes them back as one directive line (`LocalForward 6443 127.0.0.1:6443`); the bind address of a host's `LocalForward` stays in *Raw text*.
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
- **Key generation:** *Generate key…* (Lucide `key-round`) sits next to the *Form / Raw text* tabs in the host editor. The dialog uses the standard dropdown for the algorithm (Ed25519 marked "recommended"; security keys in their own group, noted as opening a terminal), password fields with a show/hide eye, a stepper for KDF rounds, a file field (typed path; no file picker), and a yellow banner while the passphrase is empty. It stays a window, not a modal (§11). The result dialog shows fingerprint, type and passphrase state, the public key with a copy icon, and *Next steps* as rows (button + one-line explanation): *Use in this host*, *Add to agent* (disabled with the reason when the agent isn't available), *Install on server*.
- **Switches, not checkboxes:** the tunnel's *Reconnect automatically* and the group's *Start with sshelf* are Magenta switches. A group's tunnels are picked with the multi-select dropdown (rows show tunnel name and host). Groups that start with sshelf show a "starts with sshelf" chip (Lucide `power`) in their detail.

---

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
  - Lists: a tunnel status dot on host rows; the search match highlighted in the alias; "N of M match"; a "+wildcard" chip; "1 of 3 running" in Tunnels; two-line tunnel rows with *Cancel* / *Retry* / *Review…*; "running · 2/2" on groups. Cards show `user@host:port` only once `ssh -G` has run for that host.
  - Right-click menus with existing actions only: hosts (*Connect*, *New tunnel…*, *Edit*, *Rename…*, favorite, *Copy alias*, *Copy ssh command*), tunnels (*Start*, *Stop*, *Edit*, *Copy ssh command*, *Delete…*).
  - Host detail: *Overview* / *Effective config* / *Tunnels · N* tabs; "ssh -G · N s ago" and *Refresh* on the effective config.
  - Tunnels: uptime in the status chip and `HH:MM:SS` on log lines (core exposes the supervisor's existing start `Instant` and adds a timestamp per `LogLine`); a failure banner with *Show log*; the editor's command preview (from the real ssh arguments) and *Save and start*.
  - Groups: *Stop group* and *Add tunnel* in the detail; *New group* disabled with the reason while no tunnels exist.
  - Dialogs: deletes confirm in a dialog; rename lists what else changes and says up front which running tunnel blocks it (§5); the unknown-key dialog shows the jump hop it is on, the `[host]:port` form, *Copy* for the compare command and how many lines it adds.
  - Settings: one *Agent* row; *Copy command* for the agent hint.
  - Tray: a status header, *Retry* on degraded groups, a failed tunnel opens the window on that tunnel, a banner while a host key waits; tooltip "sshelf · N tunnels running".
  - *Check all* stops when clicked again (above) through a new `CancelHealth` command: queued checks are dropped, running ones finish.
- **Done when:** every screen matches its board (layout, colours, radii, icons, timings); *Reduce motion* turns all animation off; the sshelf tray menu works with the window hidden or falls back to the system menu; UI tests and CI pass on all three OSes.

---

## 13. Final shape (summary)

A small, trustworthy desktop tool where:
- the ssh config remains the single source of truth and is edited with minimal, comment-preserving diffs;
- every host shows its effective configuration and health at a glance;
- tunnels and groups of tunnels start with one click from the window or the tray, with clear status and logs;
- host keys and keys in general are handled safely and transparently;
- all app data is plain, versioned TOML that the user can read, back up and version-control.

## 14. Ideas parked for later (not scheduled)

- `kubectl port-forward` profiles alongside SSH tunnels (k3s workflows).
- Embedded terminal.
- Support for alternative ssh distributions on Windows.
- macOS: hide the Dock icon while minimized to the tray (activation policy *accessory*), and show the window again from the Dock (needs Objective-C calls; not done in M6).
- Re-detect a tray host appearing or disappearing on Linux without *Refresh* (watch `NameOwnerChanged` for `org.kde.StatusNotifierWatcher`).
- From the M9 design boards (`docs/design/`), not built yet:
  - Hosts: *Delete host…* and *Show in config file* in the host menu (Hosts-Interactions).
  - Keyboard shortcuts beyond `Ctrl+1…4`: `Ctrl+K` search, `Esc` clears, `Enter` / `E` / `F2` / `Del` on rows, `Ctrl+S` / `Esc` in editors, ↑↓ in lists (Hosts-Interactions, Host-Edit, Empty-States, Tunnel-States).
  - Effective config: "Set for this host / All" with a "From" column naming the line each value came from; "Open line N" from an `ssh -G` error (Host-Config).
  - Host editor: undo per field (Host-Edit).
  - Host editor: "Re-apply my edit" onto a file that changed on disk, with the disk diff (Host-Edit-Raw).
  - Raw editor: syntax highlighting, error marks in the gutter, "Did you mean …?" (Host-Edit-Raw).
  - *Browse…* file pickers for `IdentityFile` and the key file; needs a new dependency (Host-New, Dialog-KeyGen).
  - Health: *Cancel* for a single running check (Host-Health).
  - Health: *Review host key…* from the health banner; the health check never prompts today (Host-Health).
  - Toasts with *Undo* (Feedback).
  - An error queue ("+1 more") and action links in errors such as "Change in Settings" (Feedback).
  - Tunnels: *Duplicate* and *Add to group ›* in the tunnel menu (Tunnel-States).
  - Tunnels: a "waiting · confirm host key" row state with *Show* (Tunnel-States).
  - Tunnels: the process holding a busy port (pid and name), and a live port check while editing (Tunnel-States, Tunnel-Edit).
  - Tunnels: an "I want to listen on all interfaces" switch that gates Save (Tunnel-Edit).
  - Changed host key: the `known_hosts` line number of the stored key (Dialog-HostKey-Changed).
  - Unknown host key: "host 1 of 2" counts in the jump chain (Dialog-HostKey).
  - Settings: *Open* the app data folder in the file manager (A-Settings).
