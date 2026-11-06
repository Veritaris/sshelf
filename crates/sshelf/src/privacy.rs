//! Streamer mode's state for drawing (SPEC.md §10 *Streamer mode*). The app
//! stores it in the context once per frame; views and widgets read it from
//! there, so no view signature changes.

use std::borrow::Cow;
use std::sync::Arc;

use egui::{Context, Id};
use sshelf_core::redact::{DOTS, Redactor};

#[derive(Clone, Default)]
pub struct Privacy {
    /// Streamer mode is on: values are masked.
    pub on: bool,
    /// What the free-text filter knows (core rebuilds it on every config
    /// load and tunnel change).
    pub redactor: Arc<Redactor>,
    /// *Hide host aliases and tags* (only while `on`): aliases show as
    /// "host N", tags as a `tag` label.
    pub hide_aliases: bool,
}

/// How an alias is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasLook {
    /// As it is.
    Shown,
    /// "host N" (hidden aliases).
    Numbered(usize),
    /// "•••••": an IP address or equal to its HostName, masked whenever
    /// streamer mode is on.
    Dots,
}

impl Privacy {
    /// The toast after copying `name`'s value.
    pub fn copied(&self, name: &str) -> String {
        if self.on {
            format!("Copied {name} · not shown in streamer mode")
        } else {
            format!("Copied {name}")
        }
    }

    /// `text` through the filter for places that only take a string (the
    /// native tray menu, the window title): tokens become "•••••".
    pub fn plain<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if self.on {
            Cow::Owned(self.redactor.mask_plain(text, self.hide_aliases))
        } else {
            Cow::Borrowed(text)
        }
    }

    /// How `alias` is shown now.
    pub fn alias_look(&self, alias: &str) -> AliasLook {
        if !self.on {
            return AliasLook::Shown;
        }
        if self.redactor.alias_always_masked(alias) {
            return AliasLook::Dots;
        }
        match self.redactor.alias_number(alias) {
            Some(n) if self.hide_aliases => AliasLook::Numbered(n),
            // An alias the config doesn't know (an orphan's): masked too.
            None if self.hide_aliases => AliasLook::Dots,
            _ => AliasLook::Shown,
        }
    }

    /// `alias` as text, for places that can't peek (clickable rows, the
    /// tray, accessibility names).
    pub fn alias_text<'a>(&self, alias: &'a str) -> Cow<'a, str> {
        match self.alias_look(alias) {
            AliasLook::Shown => Cow::Borrowed(alias),
            AliasLook::Numbered(n) => Cow::Owned(format!("host {n}")),
            AliasLook::Dots => Cow::Borrowed(DOTS),
        }
    }

    /// A host as ssh is given it (an alias, or an address for a jump host),
    /// as text: its alias look, then the filter.
    pub fn host(&self, name: &str) -> String {
        let shown = self.alias_text(name);
        self.plain(&shown).into_owned()
    }

    /// A kubectl tunnel's namespace or resource name as text: "namespace N"
    /// / "resource N" while names are hidden (they are treated like aliases,
    /// SPEC.md §10 *kubectl tunnels*); one no tunnel uses yet (a lookup's)
    /// is "•••••" then, as an alias the config doesn't know.
    pub fn kube_name<'a>(&self, name: &'a str) -> Cow<'a, str> {
        if !(self.on && self.hide_aliases) || name.is_empty() {
            return Cow::Borrowed(name);
        }
        match self.redactor.kube_name(name) {
            Some(numbered) => Cow::Owned(numbered),
            None => Cow::Borrowed(DOTS),
        }
    }

    /// A tag as text: `tag` while tags are hidden.
    pub fn tag_text<'a>(&self, tag: &'a str) -> &'a str {
        if self.on && self.hide_aliases {
            "tag"
        } else {
            tag
        }
    }
}

fn id() -> Id {
    Id::unique("sshelf_privacy")
}

/// Stores this frame's state; the app calls it at the start of every pass.
pub fn set(ctx: &Context, privacy: Privacy) {
    ctx.data_mut(|d| d.insert_temp(id(), privacy));
}

/// The current state (off until the app has set it, as in widget tests).
pub fn get(ctx: &Context) -> Privacy {
    ctx.data(|d| d.get_temp(id())).unwrap_or_default()
}
