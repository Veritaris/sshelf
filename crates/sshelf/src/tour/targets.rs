//! Tour targets (SPEC.md §10 *Onboarding tour*): views register the rect of
//! each widget a step may spotlight, every frame, whether or not the tour
//! runs. Ids are an enum, so a renamed widget fails to compile instead of
//! silently losing its spotlight.

use std::collections::HashMap;

use egui::{Context, Id, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// The host list's rows.
    HostRows,
    /// The whole host list with its header (*Check all*).
    HostList,
    /// *Check all*.
    CheckAll,
    /// The host detail's *Connect*.
    Connect,
    /// The host detail's Health line.
    HealthLine,
    /// The host detail's tabs and what they show.
    DetailTabs,
    /// The *Effective config* tab.
    EffectiveConfigTab,
    /// The host detail's *Edit*.
    Edit,
    /// The host editor's tabs and form.
    EditorForm,
    /// The editor's *Raw text* tab.
    RawTextTab,
    /// The editor's *Changes to write*.
    EditorChanges,
    /// The editor's *Save*.
    EditorSave,
    /// Rail items.
    RailLogo,
    RailTunnels,
    RailGroups,
    /// A tunnel row, by id.
    TunnelRow(&'static str),
    /// A tunnel row's state button (Start, Retry…), by id.
    TunnelAction(&'static str),
    /// *New tunnel*.
    NewTunnel,
    /// The tunnel editor's first forward.
    ForwardRow,
    /// The tunnel editor's footer, and its *Save*.
    TunnelEditorFooter,
    TunnelSave,
    /// The selected tunnel's failure banner, and its *Generate key…*.
    FailureBanner,
    BannerGenerateKey,
    /// The key window, its *Generate* and *Install on server*.
    KeyWindow,
    KeyGenerate,
    KeyInstall,
    /// A group row and its switch, by id.
    GroupRow(&'static str),
    GroupSwitch(&'static str),
    /// The group detail's *Edit*.
    GroupEdit,
    /// The group editor, and its *Save*.
    GroupEditor,
    GroupSave,
    /// The window's *Close*.
    WindowClose,
    /// The close dialog, and its *Minimize to tray*.
    CloseDialog,
    CloseMinimize,
    /// The status bar, and its running count.
    StatusBar,
    StatusRunning,
}

/// Rects registered this frame (and the last, for the overlay).
#[derive(Debug, Clone, Default)]
pub struct Registry {
    pub frame: u64,
    pub rects: HashMap<Target, Rect>,
}

fn id() -> Id {
    Id::unique("sshelf_tour_targets")
}

/// Registers `target` at `rect` for this frame.
pub fn register(ctx: &Context, target: Target, rect: Rect) {
    let frame = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let registry = d.get_temp_mut_or_default::<Registry>(id());
        if registry.frame != frame {
            registry.frame = frame;
            registry.rects.clear();
        }
        // A later registration (e.g. a row painted over) grows it.
        registry
            .rects
            .entry(target)
            .and_modify(|r| *r = r.union(rect))
            .or_insert(rect);
    });
}

/// Registers `target` at `response`'s widget and tells whether it was
/// clicked: by the user, or by the tour's *Show me* ([`click`]).
pub fn clicked(ctx: &Context, target: Target, response: &egui::Response) -> bool {
    register(ctx, target, response.rect);
    response.clicked() || take_click(ctx, target)
}

/// [`clicked`] for a widget whose `changed()` is its action (a switch).
pub fn changed(ctx: &Context, target: Target, response: &egui::Response) -> bool {
    register(ctx, target, response.rect);
    response.changed() || take_click(ctx, target)
}

fn click_id() -> Id {
    Id::unique("sshelf_tour_click")
}

/// *Show me*: the widget registered as `target` acts as if clicked the next
/// time it is drawn.
pub fn click(ctx: &Context, target: Target) {
    ctx.data_mut(|d| d.insert_temp(click_id(), Some(target)));
    ctx.request_repaint();
}

/// Whether *Show me* clicked `target` (once).
pub fn take_click(ctx: &Context, target: Target) -> bool {
    ctx.data_mut(|d| {
        let pending = d.get_temp_mut_or_default::<Option<Target>>(click_id());
        if *pending == Some(target) {
            *pending = None;
            true
        } else {
            false
        }
    })
}

/// Where `target` was registered this frame or the frame before.
pub fn rect(ctx: &Context, target: Target) -> Option<Rect> {
    let frame = ctx.cumulative_pass_nr();
    ctx.data(|d| {
        d.get_temp::<Registry>(id())
            .filter(|r| r.frame + 1 >= frame)
            .and_then(|r| r.rects.get(&target).copied())
    })
}
