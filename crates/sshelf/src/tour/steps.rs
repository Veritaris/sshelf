//! The tour as data (SPEC.md §10 *Onboarding tour*): 27 steps in 6 chapters,
//! and the glossary of the `[terms]` in their texts. Titles, texts and terms
//! are the final ones from the `Tour-Step` board.

use crate::tour::targets::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chapter {
    Hosts,
    Editing,
    Tunnels,
    Fixing,
    Groups,
    Window,
    Done,
}

impl Chapter {
    pub fn title(self) -> &'static str {
        match self {
            Chapter::Hosts => "Hosts",
            Chapter::Editing => "Editing",
            Chapter::Tunnels => "Tunnels",
            Chapter::Fixing => "Fixing problems",
            Chapter::Groups => "Groups",
            Chapter::Window => "Window & tray",
            Chapter::Done => "Done",
        }
    }
}

/// What a *do it* step waits for: the sandbox state it asks for, not a
/// click (SPEC.md: "Done" means the Core state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Connect web-01 (a toast in the tour).
    Connect,
    /// Every sample host checked.
    CheckAll,
    /// The *Effective config* tab open.
    EffectiveConfig,
    /// The host editor open (web-01).
    Edit,
    /// The editor's *Raw text* tab open.
    RawText,
    /// The edit saved.
    SaveEdit,
    /// The Tunnels view open.
    OpenTunnels,
    /// db tunnel running.
    StartDbTunnel,
    /// The tunnel editor open.
    NewTunnel,
    /// web admin saved.
    SaveTunnel,
    /// backup failed (it has no key yet).
    StartBackup,
    /// The key window open.
    OpenKeygen,
    /// The key created.
    GenerateKey,
    /// The key installed and backup running again.
    InstallKey,
    /// The Groups view open.
    OpenGroups,
    /// Work on staging running.
    StartGroup,
    /// The group editor open.
    EditGroup,
    /// The group saved.
    SaveGroup,
    /// The close dialog open.
    Close,
    /// *Minimize to tray* clicked (a toast in the tour).
    MinimizeToTray,
}

/// Which screen a step needs, applied when it is entered (also with Back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Hosts, web-01 selected; the editor and dialogs closed.
    Hosts,
    /// Hosts, web-01's detail on its *Effective config* tab… as the user left it.
    HostsKeep,
    /// web-01's editor open on *Form*.
    HostEditor,
    /// web-01's editor open (as left).
    HostEditorKeep,
    /// Tunnels, no editor; `select` a tunnel.
    Tunnels,
    /// The tunnel editor with the tour's web admin draft.
    TunnelEditor,
    /// Tunnels with backup selected (its failure banner shows).
    Backup,
    /// The key window for backup-nas (passphrase filled in).
    Keygen,
    /// The key window's result.
    KeyResult,
    /// Groups, no editor, Work on staging selected.
    Groups,
    /// The group editor for Work on staging, with the tour's changes.
    GroupEditor,
    /// The view as it is, dialogs closed.
    Any,
    /// The close dialog.
    CloseDialog,
}

#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub chapter: Chapter,
    pub title: &'static str,
    pub text: &'static str,
    /// What the spotlight surrounds (their union); empty: none.
    pub targets: &'static [Target],
    /// Extra holes beside the spotlight ("Running shows everywhere").
    pub extra: &'static [Target],
    pub screen: Screen,
    /// A *do it* step's action, and its hint.
    pub action: Option<(Action, &'static str)>,
    /// The popover shows a picture of the tray menu.
    pub tray_picture: bool,
}

const fn watch(
    chapter: Chapter,
    title: &'static str,
    text: &'static str,
    targets: &'static [Target],
    screen: Screen,
) -> Step {
    Step {
        chapter,
        title,
        text,
        targets,
        extra: &[],
        screen,
        action: None,
        tray_picture: false,
    }
}

const fn do_it(
    chapter: Chapter,
    (title, text): (&'static str, &'static str),
    targets: &'static [Target],
    screen: Screen,
    action: Action,
    hint: &'static str,
) -> Step {
    Step {
        chapter,
        title,
        text,
        targets,
        extra: &[],
        screen,
        action: Some((action, hint)),
        tray_picture: false,
    }
}

use Chapter as C;
use Screen as S;
use Target as T;

pub const STEPS: [Step; 27] = [
    watch(
        C::Hosts,
        "Your hosts",
        "Every [host] from your ssh config. sshelf reads it and writes only when you save an edit. The dot on the right is the last [health check].",
        &[T::HostRows],
        S::Hosts,
    ),
    do_it(
        C::Hosts,
        (
            "Connect",
            "Opens your terminal with ssh and this host. In the tour it only shows the command it would run.",
        ),
        &[T::Connect],
        S::Hosts,
        Action::Connect,
        "Try it: click Connect",
    ),
    watch(
        C::Hosts,
        "Health check",
        "A [health check] logs in once without a terminal and tells you why it fails: no [key] accepted, an unknown [host key], or no answer.",
        &[T::HealthLine],
        S::Hosts,
    ),
    do_it(
        C::Hosts,
        (
            "Check all hosts",
            "Logs in to every host once. Green: OK. Yellow: reachable, but no [key] accepted or [host key] not known yet. Red: no answer. Hover a dot for the reason.",
        ),
        &[T::HostList],
        S::Hosts,
        Action::CheckAll,
        "Click the check button",
    ),
    do_it(
        C::Hosts,
        (
            "What ssh really uses",
            "The [effective config] lists every value ssh resolves for this host, including the ones web-01 doesn\u{2019}t set itself: they come from [Host *] or ssh\u{2019}s defaults.",
        ),
        &[T::DetailTabs],
        S::HostsKeep,
        Action::EffectiveConfig,
        "Click Effective config",
    ),
    do_it(
        C::Editing,
        (
            "Edit safely",
            "The editor shows every change before it touches the file.",
        ),
        &[T::Edit],
        S::Hosts,
        Action::Edit,
        "Click Edit",
    ),
    do_it(
        C::Editing,
        (
            "Form or raw text",
            "We changed User to admin for you. Prefer typing? Raw text is the same host as plain ssh config.",
        ),
        &[T::EditorForm],
        S::HostEditor,
        Action::RawText,
        "Click Raw text",
    ),
    do_it(
        C::Editing,
        (
            "Review, then save",
            "Changes to write shows the exact lines. Save writes the sample file only, after a backup.",
        ),
        &[T::EditorChanges, T::EditorSave],
        S::HostEditorKeep,
        Action::SaveEdit,
        "Click Save",
    ),
    do_it(
        C::Tunnels,
        (
            "Tunnels",
            "A tunnel [forwards] a port through a host. Next you start one and watch its states.",
        ),
        &[T::RailTunnels],
        S::Hosts,
        Action::OpenTunnels,
        "Click Tunnels",
    ),
    do_it(
        C::Tunnels,
        (
            "Start a tunnel",
            "db tunnel [forwards] local port 15432 to port 5432 on db-primary, through [bastion]. Start it and watch Starting turn into Running.",
        ),
        &[T::TunnelRow(TOUR_DB)],
        S::Tunnels,
        Action::StartDbTunnel,
        "Click Start",
    ),
    Step {
        chapter: C::Tunnels,
        title: "Running shows everywhere",
        text: "A running tunnel lights a green dot on the sshelf logo (and the [tray] icon) and on Tunnels, and the status bar counts them. Stop the last one and the dots go away.",
        targets: &[T::StatusRunning],
        extra: &[T::RailLogo, T::RailTunnels],
        screen: S::Tunnels,
        action: None,
        tray_picture: false,
    },
    do_it(
        C::Tunnels,
        ("Make your own", "New tunnel opens the tunnel editor."),
        &[T::NewTunnel],
        S::Tunnels,
        Action::NewTunnel,
        "Click New tunnel",
    ),
    watch(
        C::Tunnels,
        "Three fields per forward",
        "Local port on this computer \u{2192} IP or host as the remote machine sees it : port there. The command line below shows exactly what sshelf will run.",
        &[T::ForwardRow],
        S::TunnelEditor,
    ),
    do_it(
        C::Tunnels,
        (
            "Save it",
            "Save keeps it for later; Save and start would also start it right away. A port that is already taken is reported when the tunnel starts.",
        ),
        &[T::TunnelEditorFooter],
        S::TunnelEditor,
        Action::SaveTunnel,
        "Click Save",
    ),
    do_it(
        C::Fixing,
        (
            "When it fails",
            "backup-nas doesn\u{2019}t know any of your keys yet. sshelf shows what ssh said and offers the fix right there.",
        ),
        &[T::TunnelRow(TOUR_BACKUP), T::FailureBanner],
        S::Backup,
        Action::StartBackup,
        "Click Start on backup",
    ),
    do_it(
        C::Fixing,
        (
            "Fix it",
            "No [key] yet? Generate one without leaving sshelf. (The [agent] fix helps when the key exists but isn\u{2019}t loaded.)",
        ),
        &[T::BannerGenerateKey],
        S::Backup,
        Action::OpenKeygen,
        "Click Generate key\u{2026}",
    ),
    do_it(
        C::Fixing,
        (
            "Generate key",
            "Ed25519 is the safe default. A [passphrase] protects the file if someone copies it. In the tour the key goes to a temporary folder.",
        ),
        &[T::KeyWindow],
        S::Keygen,
        Action::GenerateKey,
        "Click Generate",
    ),
    do_it(
        C::Fixing,
        (
            "Put it to use",
            "Next steps do the rest: use it in the host, add it to the [agent], or append it to the server\u{2019}s [authorized_keys]. Then backup retries by itself.",
        ),
        &[T::KeyWindow],
        S::KeyResult,
        Action::InstallKey,
        "Click Install on server",
    ),
    do_it(
        C::Groups,
        ("Groups", "Groups start several tunnels with one switch."),
        &[T::RailGroups],
        S::Tunnels,
        Action::OpenGroups,
        "Click Groups",
    ),
    do_it(
        C::Groups,
        (
            "One switch",
            "Flip Work on staging: both tunnels start together. Turning it off keeps tunnels another running group still needs.",
        ),
        &[T::GroupRow(TOUR_STAGING_GROUP)],
        S::Groups,
        Action::StartGroup,
        "Flip Work on staging",
    ),
    do_it(
        C::Groups,
        (
            "Edit a group",
            "Pick its tunnels and whether it turns on when sshelf starts.",
        ),
        &[T::GroupEdit],
        S::Groups,
        Action::EditGroup,
        "Click Edit",
    ),
    do_it(
        C::Groups,
        (
            "Group editor",
            "Tunnels come from the multi-select list. Start with sshelf turns the group on whenever sshelf starts.",
        ),
        &[T::GroupEditor],
        S::GroupEditor,
        Action::SaveGroup,
        "Click Save",
    ),
    do_it(
        C::Window,
        (
            "Closing the window",
            "Tunnels are running, so closing asks what to do.",
        ),
        &[T::WindowClose],
        S::Any,
        Action::Close,
        "Click \u{d7} (top right)",
    ),
    do_it(
        C::Window,
        (
            "Quit or keep running",
            "Minimize to tray keeps every tunnel up; Stop and quit ends them. You can remember the choice.",
        ),
        &[T::CloseDialog],
        S::CloseDialog,
        Action::MinimizeToTray,
        "Click Minimize to tray",
    ),
    Step {
        chapter: C::Window,
        title: "From the tray",
        text: "Right-click the sshelf [tray] icon for groups, tunnels and recent hosts. Left-click opens the window again.",
        targets: &[],
        extra: &[],
        screen: S::Any,
        action: None,
        tray_picture: true,
    },
    watch(
        C::Window,
        "Status bar",
        "Running tunnels and the ssh in use at a glance (here [fake-ssh]). An error, like the failed backup from before, takes its place until you dismiss it with \u{d7}.",
        &[T::StatusBar],
        S::Any,
    ),
    watch(
        C::Done,
        "You\u{2019}re set",
        "The sample is deleted and your own config opens. Take the tour again, or one chapter of it, from Settings \u{203a} Help.",
        &[],
        S::Hosts,
    ),
];

/// Ids in the sample bundle the steps refer to.
pub const TOUR_HOST: &str = "web-01";
pub const TOUR_DB: &str = "t-tour-db";
pub const TOUR_BACKUP: &str = "t-tour-backup";
pub const TOUR_BACKUP_HOST: &str = "backup-nas";
pub const TOUR_STAGING_GROUP: &str = "g-tour-staging";
/// The tunnel the tour makes in step 13.
pub const TOUR_WEB_ADMIN: &str = "t-tour-web-admin";

/// The sample's tunnel and group ids a step spotlights, as `'static` (the
/// target ids); `None` for any other id.
pub fn static_id(id: &str) -> Option<&'static str> {
    [TOUR_DB, TOUR_BACKUP, TOUR_STAGING_GROUP]
        .into_iter()
        .find(|known| *known == id)
}

/// The first step of `chapter`.
pub fn first_step(chapter: Chapter) -> usize {
    STEPS.iter().position(|s| s.chapter == chapter).unwrap_or(0)
}

/// Plain-language explanations of the `[terms]` in step texts.
pub const GLOSS: [(&str, &str); 13] = [
    (
        "host",
        "One entry in your ssh config: a short name plus how to reach that server (address, user, key\u{2026}).",
    ),
    (
        "host *",
        "A pattern block that applies to every host. Its values fill in whatever a host doesn\u{2019}t set itself.",
    ),
    (
        "health check",
        "sshelf logs in once in the background, runs nothing, and records whether it worked and why not.",
    ),
    (
        "effective config",
        "The final settings ssh uses for this host, after merging the host\u{2019}s own lines, matching patterns and defaults.",
    ),
    (
        "forwards",
        "Port forwarding: a port on your computer that leads, through ssh, to a port on another machine. localhost:15432 here reaches db-primary:5432 there.",
    ),
    (
        "bastion",
        "A jump host: the one server reachable from outside. ssh logs in there first, then hops on to machines in the private network (ProxyJump).",
    ),
    (
        "agent",
        "ssh-agent keeps your unlocked keys in memory, so ssh can use them without asking for the passphrase every time.",
    ),
    (
        "key",
        "An ssh key pair: a private file that stays on your computer and a public part you put on servers. It replaces typing a password.",
    ),
    (
        "authorized_keys",
        "The file on a server that lists the public keys allowed to log in to that account.",
    ),
    (
        "host key",
        "The server\u{2019}s own key. ssh remembers it in known_hosts, so it can warn you if a different machine answers next time.",
    ),
    (
        "passphrase",
        "A password that encrypts the private key file. The agent can remember it for you.",
    ),
    (
        "tray",
        "The small icons area of the taskbar (Windows) or the menu bar (macOS / Linux).",
    ),
    (
        "fake-ssh",
        "A built-in stand-in for ssh used only by the tour (sshelf --fake-ssh). It pretends to be the sample servers.",
    ),
];

/// The explanation of `term` (any case).
pub fn gloss(term: &str) -> Option<&'static str> {
    let term = term.to_lowercase();
    GLOSS
        .iter()
        .find(|(t, _)| *t == term)
        .map(|(_, text)| *text)
}

/// A step text cut into plain runs and `[terms]`.
pub fn parts(text: &str) -> Vec<(&str, bool)> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        if open > 0 {
            parts.push((&rest[..open], false));
        }
        parts.push((&rest[open + 1..open + close], true));
        rest = &rest[open + close + 1..];
    }
    if !rest.is_empty() {
        parts.push((rest, false));
    }
    parts
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_term_has_an_entry_and_every_entry_is_used() {
        let mut used = BTreeSet::new();
        for step in &STEPS {
            for (part, term) in parts(step.text) {
                if term {
                    assert!(gloss(part).is_some(), "no glossary entry for [{part}]");
                    used.insert(part.to_lowercase());
                }
            }
        }
        for (term, _) in GLOSS {
            assert!(
                used.contains(term),
                "the glossary entry `{term}` is never used"
            );
        }
    }

    #[test]
    fn steps_are_well_formed() {
        assert_eq!(STEPS.len(), 27);
        let last = STEPS.last().unwrap();
        assert_eq!(last.chapter, Chapter::Done);
        for step in &STEPS[..26] {
            if step.chapter != Chapter::Window || !step.tray_picture {
                assert!(
                    !step.targets.is_empty(),
                    "`{}` has no spotlight",
                    step.title
                );
            }
        }
        assert_eq!(first_step(Chapter::Tunnels), 8);
        assert_eq!(first_step(Chapter::Window), 22);
    }

    #[test]
    fn texts_split_into_terms() {
        assert_eq!(
            parts("a [host] and [Host *]."),
            [
                ("a ", false),
                ("host", true),
                (" and ", false),
                ("Host *", true),
                (".", false)
            ]
        );
    }
}
