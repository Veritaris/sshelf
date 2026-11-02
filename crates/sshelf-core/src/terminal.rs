//! "Connect": open the user's terminal running ssh for a host (SPEC.md §10).
//!
//! The command template is split into words on whitespace (`"…"` groups a
//! word; there are no escapes and no shell), then `{ssh}` and `{alias}` are
//! substituted inside each word, so an alias can never inject arguments or
//! commands. A leading `{terminal_cmd}` hands the remaining words to the
//! detected terminal.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use crate::ssh::find_in_path;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("the terminal command is empty")]
    Empty,
    #[error("the terminal command has an unterminated quote")]
    UnterminatedQuote,
    #[error("`{{terminal_cmd}}` may only be the first word of the terminal command")]
    TerminalCmdNotFirst,
    #[error("no terminal found; set a full terminal command in Settings")]
    NoTerminal,
}

/// How a launched program gets its window on Windows (ignored elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    /// Default process creation.
    Inherit,
    /// A new console window (ssh run directly).
    New,
    /// No console window (the program opens its own UI).
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    pub console: Console,
}

/// How a Linux terminal emulator takes the command to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecStyle {
    /// `term -e cmd args…`
    DashE,
    /// `term -- cmd args…`
    DoubleDash,
    /// `term -x cmd args…`
    DashX,
    /// `term cmd args…`
    Direct,
    /// `wezterm start -- cmd args…`
    WeztermStart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminal {
    WindowsTerminal(PathBuf),
    /// No Windows Terminal: ssh runs directly in a new console window.
    WindowsConsole,
    /// Terminal.app via `osascript`.
    MacTerminal,
    Linux {
        program: PathBuf,
        style: ExecStyle,
    },
}

/// Linux emulators in preference order, after `$TERMINAL`.
const LINUX_TERMINALS: &[(&str, ExecStyle)] = &[
    ("x-terminal-emulator", ExecStyle::DashE),
    ("gnome-terminal", ExecStyle::DoubleDash),
    ("konsole", ExecStyle::DashE),
    ("xfce4-terminal", ExecStyle::DashX),
    ("alacritty", ExecStyle::DashE),
    ("kitty", ExecStyle::Direct),
    ("wezterm", ExecStyle::WeztermStart),
    ("foot", ExecStyle::Direct),
    ("xterm", ExecStyle::DashE),
];

impl Terminal {
    /// The per-OS default terminal (SPEC.md §10).
    pub fn detect() -> Option<Self> {
        let path_var = env::var_os("PATH").unwrap_or_default();
        let on_path = |name: &str| find_in_path(name, &path_var);
        if cfg!(windows) {
            let apps = env::var_os("LOCALAPPDATA")
                .map(|d| Path::new(&d).join(r"Microsoft\WindowsApps\wt.exe"))
                .filter(|p| p.is_file());
            return Some(match on_path("wt").or(apps) {
                Some(wt) => Terminal::WindowsTerminal(wt),
                None => Terminal::WindowsConsole,
            });
        }
        if cfg!(target_os = "macos") {
            return Some(Terminal::MacTerminal);
        }
        let env_terminal = env::var("TERMINAL").ok();
        linux_terminal(env_terminal.as_deref(), on_path)
    }

    pub fn describe(&self) -> String {
        match self {
            Terminal::WindowsTerminal(path) => format!("Windows Terminal ({})", path.display()),
            Terminal::WindowsConsole => "console window (Windows Terminal not found)".to_string(),
            Terminal::MacTerminal => "Terminal.app".to_string(),
            Terminal::Linux { program, .. } => program.display().to_string(),
        }
    }

    /// Runs `command` (program and arguments) in a new terminal window.
    fn wrap(&self, command: Vec<String>, alias: &str) -> LaunchPlan {
        match self {
            Terminal::WindowsTerminal(wt) => {
                let mut args = vec!["new-tab".into(), "--title".into(), alias.to_string()];
                args.extend(command);
                LaunchPlan {
                    program: wt.display().to_string(),
                    args,
                    console: Console::Hidden,
                }
            }
            Terminal::WindowsConsole => {
                let mut words = command.into_iter();
                LaunchPlan {
                    program: words.next().unwrap_or_default(),
                    args: words.collect(),
                    console: Console::New,
                }
            }
            Terminal::MacTerminal => {
                let script = format!(
                    "tell application \"Terminal\" to do script \"{}\"",
                    applescript_escape(&shell_join(&command))
                );
                LaunchPlan {
                    program: "osascript".into(),
                    args: vec![
                        "-e".into(),
                        script,
                        "-e".into(),
                        "tell application \"Terminal\" to activate".into(),
                    ],
                    console: Console::Inherit,
                }
            }
            Terminal::Linux { program, style } => {
                let prefix: &[&str] = match style {
                    ExecStyle::DashE => &["-e"],
                    ExecStyle::DoubleDash => &["--"],
                    ExecStyle::DashX => &["-x"],
                    ExecStyle::Direct => &[],
                    ExecStyle::WeztermStart => &["start", "--"],
                };
                let mut args: Vec<String> = prefix.iter().map(|s| s.to_string()).collect();
                args.extend(command);
                LaunchPlan {
                    program: program.display().to_string(),
                    args,
                    console: Console::Inherit,
                }
            }
        }
    }
}

/// `$TERMINAL` (name or path, run with `-e`), else the first known emulator.
pub fn linux_terminal(
    env_terminal: Option<&str>,
    on_path: impl Fn(&str) -> Option<PathBuf>,
) -> Option<Terminal> {
    if let Some(term) = env_terminal.map(str::trim).filter(|t| !t.is_empty()) {
        let path = Path::new(term);
        let program = if path.is_absolute() {
            Some(path.to_path_buf())
        } else {
            on_path(term)
        };
        if let Some(program) = program {
            return Some(Terminal::Linux {
                program,
                style: ExecStyle::DashE,
            });
        }
    }
    LINUX_TERMINALS
        .iter()
        .find_map(|&(name, style)| on_path(name).map(|program| Terminal::Linux { program, style }))
}

/// Splits a template into words: whitespace separates, `"…"` groups.
pub fn split_template(template: &str) -> Result<Vec<String>, TemplateError> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quoted = false;
    for c in template.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quoted {
        return Err(TemplateError::UnterminatedQuote);
    }
    if in_word {
        words.push(word);
    }
    if words.is_empty() {
        return Err(TemplateError::Empty);
    }
    Ok(words)
}

/// What to run for "Connect" to `alias`.
pub fn plan(
    template: &str,
    ssh: &Path,
    alias: &str,
    terminal: Option<&Terminal>,
) -> Result<LaunchPlan, TemplateError> {
    let words = split_template(template)?;
    let ssh = ssh.display().to_string();
    let fill = |word: &str| word.replace("{ssh}", &ssh).replace("{alias}", alias);

    if words[0] == "{terminal_cmd}" {
        let command: Vec<String> = words[1..].iter().map(|w| fill(w)).collect();
        if command.iter().any(|w| w == "{terminal_cmd}") {
            return Err(TemplateError::TerminalCmdNotFirst);
        }
        if command.is_empty() {
            return Err(TemplateError::Empty);
        }
        let terminal = terminal.ok_or(TemplateError::NoTerminal)?;
        return Ok(terminal.wrap(command, alias));
    }
    if words.iter().any(|w| w.contains("{terminal_cmd}")) {
        return Err(TemplateError::TerminalCmdNotFirst);
    }
    let mut words = words.iter().map(|w| fill(w));
    Ok(LaunchPlan {
        program: words.next().expect("template has words"),
        args: words.collect(),
        console: if cfg!(windows) {
            Console::New
        } else {
            Console::Inherit
        },
    })
}

/// Starts `plan` detached from sshelf; the process is reaped in the background.
pub fn spawn(plan: &LaunchPlan) -> Result<(), String> {
    let mut cmd = Command::new(&plan.program);
    cmd.args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        match plan.console {
            Console::Inherit => {}
            Console::New => {
                cmd.creation_flags(CREATE_NEW_CONSOLE);
            }
            Console::Hidden => {
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", plan.program))?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// POSIX shell words: each in single quotes, `'` as `'\''`.
fn shell_join(words: &[String]) -> String {
    words
        .iter()
        .map(|w| format!("'{}'", w.replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn applescript_escape(text: &str) -> String {
    text.replace('\\', r"\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn words(t: &str) -> Vec<String> {
        split_template(t).unwrap()
    }

    fn ssh() -> PathBuf {
        PathBuf::from("/usr/bin/ssh")
    }

    #[test]
    fn split_on_whitespace_with_quotes() {
        assert_eq!(words("a  b\tc"), ["a", "b", "c"]);
        assert_eq!(
            words(r#""C:\Program Files\x.exe" -e {ssh}"#),
            [r"C:\Program Files\x.exe", "-e", "{ssh}"]
        );
        assert_eq!(words(r#"a"b c"d """#), ["ab cd", ""]);
        assert_eq!(split_template("  "), Err(TemplateError::Empty));
        assert_eq!(
            split_template(r#"x "open"#),
            Err(TemplateError::UnterminatedQuote)
        );
    }

    #[test]
    fn custom_template_runs_directly() {
        let plan = plan("alacritty -e {ssh} -t {alias}", &ssh(), "my host", None).unwrap();
        assert_eq!(plan.program, "alacritty");
        assert_eq!(plan.args, ["-e", "/usr/bin/ssh", "-t", "my host"]);
    }

    #[test]
    fn placeholders_inside_words_do_not_split() {
        let plan = plan(
            "term --title=ssh:{alias} {ssh} {alias}",
            &ssh(),
            "a b; rm -rf ~",
            None,
        )
        .unwrap();
        assert_eq!(
            plan.args,
            ["--title=ssh:a b; rm -rf ~", "/usr/bin/ssh", "a b; rm -rf ~"]
        );
    }

    #[test]
    fn terminal_cmd_only_first() {
        let term = Terminal::MacTerminal;
        assert_eq!(
            plan("x {terminal_cmd} {ssh}", &ssh(), "a", Some(&term)),
            Err(TemplateError::TerminalCmdNotFirst)
        );
        assert_eq!(
            plan("{terminal_cmd} {terminal_cmd}", &ssh(), "a", Some(&term)),
            Err(TemplateError::TerminalCmdNotFirst)
        );
        assert_eq!(
            plan("{terminal_cmd}", &ssh(), "a", Some(&term)),
            Err(TemplateError::Empty)
        );
        assert_eq!(
            plan("{terminal_cmd} {ssh} {alias}", &ssh(), "a", None),
            Err(TemplateError::NoTerminal)
        );
    }

    #[test]
    fn windows_terminal_and_console() {
        let wt = Terminal::WindowsTerminal(PathBuf::from(r"C:\wt.exe"));
        let p = plan("{terminal_cmd} {ssh} {alias}", &ssh(), "web", Some(&wt)).unwrap();
        assert_eq!(p.program, r"C:\wt.exe");
        assert_eq!(p.args, ["new-tab", "--title", "web", "/usr/bin/ssh", "web"]);
        assert_eq!(p.console, Console::Hidden);

        let p = plan(
            "{terminal_cmd} {ssh} {alias}",
            &ssh(),
            "web",
            Some(&Terminal::WindowsConsole),
        )
        .unwrap();
        assert_eq!(p.program, "/usr/bin/ssh");
        assert_eq!(p.args, ["web"]);
        assert_eq!(p.console, Console::New);
    }

    #[test]
    fn mac_terminal_quotes_for_shell_and_applescript() {
        let p = plan(
            "{terminal_cmd} {ssh} {alias}",
            Path::new("/usr/bin/ssh"),
            r#"it's "x""#,
            Some(&Terminal::MacTerminal),
        )
        .unwrap();
        assert_eq!(p.program, "osascript");
        assert_eq!(
            p.args[1],
            r#"tell application "Terminal" to do script "'/usr/bin/ssh' 'it'\\''s \"x\"'""#
        );
        assert_eq!(p.args[3], r#"tell application "Terminal" to activate"#);
    }

    #[test]
    fn linux_terminal_styles() {
        let gnome = Terminal::Linux {
            program: PathBuf::from("/usr/bin/gnome-terminal"),
            style: ExecStyle::DoubleDash,
        };
        let p = plan("{terminal_cmd} {ssh} {alias}", &ssh(), "web", Some(&gnome)).unwrap();
        assert_eq!(p.args, ["--", "/usr/bin/ssh", "web"]);
        let wez = Terminal::Linux {
            program: PathBuf::from("wezterm"),
            style: ExecStyle::WeztermStart,
        };
        let p = plan("{terminal_cmd} {ssh} {alias}", &ssh(), "web", Some(&wez)).unwrap();
        assert_eq!(p.args, ["start", "--", "/usr/bin/ssh", "web"]);
    }

    #[test]
    fn linux_detection_order() {
        let installed = |names: &[&'static str]| {
            let map: HashMap<&str, PathBuf> = names
                .iter()
                .map(|n| (*n, PathBuf::from(format!("/usr/bin/{n}"))))
                .collect();
            move |name: &str| map.get(name).cloned()
        };
        assert_eq!(
            linux_terminal(None, installed(&["xterm", "konsole"])),
            Some(Terminal::Linux {
                program: "/usr/bin/konsole".into(),
                style: ExecStyle::DashE
            })
        );
        assert_eq!(
            linux_terminal(Some("kitty"), installed(&["kitty", "gnome-terminal"])),
            Some(Terminal::Linux {
                program: "/usr/bin/kitty".into(),
                style: ExecStyle::DashE
            })
        );
        // An unusable $TERMINAL falls back to the list.
        assert_eq!(
            linux_terminal(Some("nope"), installed(&["foot"])),
            Some(Terminal::Linux {
                program: "/usr/bin/foot".into(),
                style: ExecStyle::Direct
            })
        );
        assert_eq!(linux_terminal(None, installed(&[])), None);
    }
}
