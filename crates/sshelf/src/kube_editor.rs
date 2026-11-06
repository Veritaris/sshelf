//! The kubectl half of the tunnel editor (SPEC.md §10 *kubectl tunnels*,
//! *Editor*; boards Tunnel-Kubectl-New, Tunnel-Kubectl-New-Steps): the
//! context from the kubeconfig, the namespace and the resource listed from
//! the cluster on open (lookups run in core and come back as events), the
//! listen address, forwards with the resource's ports as chips and a
//! suggested local port, and the command it runs.

use std::collections::HashMap;

use egui::{Align, FontId, Frame, Layout, Margin, RichText, Ui, vec2};
use sshelf_core::core::Command;
use sshelf_core::kubectl::{KubeItem, KubeList, KubeQuery, KubectlInfo};
use sshelf_core::redact::DOTS;
use sshelf_core::tunnels::{KubeForward, ResourceKind, Tunnel};

use crate::theme::{Palette, radius};
use crate::tunnels_view::{LABEL, capitalized, editor_row, exposed_hint, field_errors};
use crate::widgets::button::Button;
use crate::widgets::dropdown::{self, Choice, Dropdown, Picked};
use crate::widgets::field::TextField;
use crate::widgets::switch::switch;
use crate::widgets::{marks, masked, nav};
use crate::{fonts, icons, privacy};

/// The fields' width.
const FIELD: f32 = 420.0;
/// The context row that follows `kubectl config use-context`.
const CURRENT: usize = usize::MAX - 1;

/// The editor's lookups (SPEC.md §7 *Lookups for the editor*): `None`
/// while core is still listing.
#[derive(Default)]
pub struct Lookups {
    results: HashMap<KubeQuery, Option<Result<Vec<KubeItem>, String>>>,
}

/// Where a lookup is.
enum Lookup<'a> {
    NotAsked,
    Loading,
    Done(&'a Result<Vec<KubeItem>, String>),
}

impl Lookups {
    /// Core's answer to a [`Command::KubeLookup`].
    pub fn on_result(&mut self, query: KubeQuery, result: Result<Vec<KubeItem>, String>) {
        if let Some(slot) = self.results.get_mut(&query) {
            *slot = Some(result);
        }
    }

    /// Forgets everything (the editor closed).
    pub fn clear(&mut self) {
        self.results.clear();
    }

    /// `query`'s list; asks core for it the first time its dropdown is
    /// `open`.
    fn get(&mut self, query: &KubeQuery, open: bool, commands: &mut Vec<Command>) -> Lookup<'_> {
        if open && !self.results.contains_key(query) {
            self.results.insert(query.clone(), None);
            commands.push(Command::KubeLookup(query.clone()));
        }
        match self.results.get(query) {
            None => Lookup::NotAsked,
            Some(None) => Lookup::Loading,
            Some(Some(result)) => Lookup::Done(result),
        }
    }

    fn items(&self, query: &KubeQuery) -> &[KubeItem] {
        match self.results.get(query) {
            Some(Some(Ok(items))) => items,
            _ => &[],
        }
    }
}

/// The local port suggested for remote `port` (SPEC.md §10): 18000 + port
/// below 1024, port + 10000 below 10000, else the port itself; the next one
/// up while `taken` has it.
pub fn suggest_local(port: u16, taken: &[u16]) -> u16 {
    let mut local = match port {
        0..1024 => 18000 + port,
        1024..10000 => port + 10000,
        _ => port,
    };
    while taken.contains(&local) && local < u16::MAX {
        local += 1;
    }
    local
}

/// Switches the draft to `kind`, keeping the name and carrying the local
/// ports over to a type that has no forwards yet; the other type's fields
/// stay until Save.
pub fn switch_type(draft: &mut Tunnel, to: sshelf_core::tunnels::TunnelType) {
    use sshelf_core::tunnels::{Forward, ForwardKind, TunnelType};
    if draft.kind == to {
        return;
    }
    let ports = draft.listen_ports();
    let ports: Vec<u16> = ports.into_iter().filter(|p| *p != 0).collect();
    draft.kind = to;
    match to {
        TunnelType::Kubectl => {
            let empty = draft
                .kube
                .forwards
                .iter()
                .all(|f| f.listen == 0 && f.target.is_empty());
            if empty && !ports.is_empty() {
                draft.kube.forwards = ports
                    .into_iter()
                    .map(|listen| KubeForward {
                        listen,
                        target: String::new(),
                    })
                    .collect();
            }
        }
        TunnelType::Ssh => {
            let empty = draft
                .forwards
                .iter()
                .all(|f| f.listen == 0 && f.target.as_deref().is_none_or(str::is_empty));
            if empty && !ports.is_empty() {
                draft.forwards = ports
                    .into_iter()
                    .map(|listen| Forward {
                        kind: ForwardKind::L,
                        bind: sshelf_core::tunnels::DEFAULT_BIND.into(),
                        listen,
                        target: Some(String::new()),
                    })
                    .collect();
            }
        }
    }
}

/// What the form needs besides the draft.
pub struct Env<'a> {
    pub info: Option<&'a KubectlInfo>,
    /// Local ports other tunnels use (suggestions skip them).
    pub taken: Vec<u16>,
    pub problems: &'a [String],
    /// Scroll *Forward to* into view once (*Pick another pod*).
    pub scroll_to_resource: bool,
}

/// The kubectl fields, in the spec's order (the name comes before, from the
/// editor).
pub fn form(
    ui: &mut Ui,
    draft: &mut Tunnel,
    lookups: &mut Lookups,
    env: &Env,
    commands: &mut Vec<Command>,
) {
    let p = &Palette::ONE_DARK;
    let privacy = privacy::get(ui.ctx());
    let masked = privacy.on;
    let errors_for = |prefix: &str| -> Vec<String> {
        env.problems
            .iter()
            .filter(|p| p.starts_with(prefix))
            .map(|p| capitalized(p))
            .collect()
    };
    let view = env.info.and_then(KubectlInfo::contexts);
    let current = view.and_then(|v| v.current.clone());
    let context_name = if draft.kube.context.is_empty() {
        current
            .clone()
            .unwrap_or_else(|| "the current context".into())
    } else {
        draft.kube.context.clone()
    };
    let shown_context = if masked {
        DOTS.to_string()
    } else {
        context_name.clone()
    };

    editor_row(ui, ("Context", true, LABEL), |ui, _| {
        let contexts = view.map(|v| v.contexts.as_slice()).unwrap_or(&[]);
        let hide = |text: &str| {
            if masked {
                DOTS.to_string()
            } else {
                text.to_string()
            }
        };
        let mut choices = vec![Choice {
            note: Some("current context".into()),
            ..Choice::new(
                CURRENT,
                current
                    .as_deref()
                    .map(hide)
                    .unwrap_or("current context".into()),
            )
        }];
        choices.extend(contexts.iter().enumerate().map(|(i, c)| Choice {
            note: (!c.cluster.is_empty()).then(|| hide(&c.cluster)),
            ..Choice::new(i, hide(&c.name))
        }));
        let selected = if draft.kube.context.is_empty() {
            CURRENT
        } else {
            contexts
                .iter()
                .position(|c| c.name == draft.kube.context)
                .unwrap_or(usize::MAX)
        };
        let trigger = match (draft.kube.context.is_empty(), &current) {
            (true, Some(current)) => format!("{} · current context", hide(current)),
            (true, None) => "current context".into(),
            (false, _) => hide(&draft.kube.context),
        };
        let mut dropdown = Dropdown::with_choices("kube_context", "Context", choices, trigger)
            .field(FIELD)
            .mono()
            .panel_width(FIELD)
            .with_caption("From your kubeconfig · read locally, nothing connects");
        if contexts.len() > 10 {
            dropdown = dropdown.searchable("contexts");
        }
        dropdown = match env.info.and_then(|i| i.view.as_ref()) {
            None => dropdown.loading("Reading your kubeconfig…"),
            Some(Err(e)) => dropdown.error(format!("Can't read the kubeconfig: {e}")),
            Some(Ok(_)) => dropdown,
        };
        let picked = if contexts.len() > 10 {
            dropdown.show_typed(ui, selected)
        } else {
            dropdown.show(ui, selected).map(Picked::Choice)
        };
        let new = match picked {
            Some(Picked::Choice(CURRENT)) => Some(String::new()),
            Some(Picked::Choice(i)) => contexts.get(i).map(|c| c.name.clone()),
            Some(Picked::Typed(text)) => Some(text),
            None => None,
        };
        if let Some(new) = new.filter(|new| *new != draft.kube.context) {
            // Another cluster: what was picked on this one goes.
            draft.kube.context = new;
            draft.kube.namespace.clear();
            draft.kube.resource.name.clear();
            draft.kube.forwards.clear();
        }
        field_errors(ui, &errors_for("the context"));
    });

    editor_row(ui, ("Namespace", true, LABEL), |ui, _| {
        let query = KubeQuery {
            context: draft.kube.context.clone(),
            what: KubeList::Namespaces,
        };
        let open = dropdown::is_open(ui.ctx(), "kube_namespace");
        let lookup = lookups.get(&query, open, commands);
        let items: Vec<String> = match &lookup {
            Lookup::Done(Ok(items)) => items.iter().map(|i| i.name.clone()).collect(),
            _ => Vec::new(),
        };
        let choices: Vec<Choice<usize>> = items
            .iter()
            .enumerate()
            .map(|(i, name)| Choice::new(i, privacy.kube_name(name)))
            .collect();
        let trigger = if draft.kube.namespace.is_empty() {
            "Pick or type a namespace".to_string()
        } else {
            privacy.kube_name(&draft.kube.namespace).into_owned()
        };
        let selected = items
            .iter()
            .position(|n| *n == draft.kube.namespace)
            .unwrap_or(usize::MAX);
        let mut dropdown = Dropdown::with_choices("kube_namespace", "Namespace", choices, trigger)
            .field(FIELD)
            .mono()
            .panel_width(FIELD)
            .searchable("namespaces");
        dropdown = match &lookup {
            Lookup::Loading => dropdown.loading(format!("Listing namespaces on {shown_context}…")),
            Lookup::Done(Err(e)) => {
                dropdown.error(format!("Can't list namespaces: {e} · type a name instead"))
            }
            Lookup::Done(Ok(items)) => dropdown.with_caption(format!(
                "kubectl get namespaces · {} · you can also type one",
                items.len()
            )),
            Lookup::NotAsked => dropdown,
        };
        let new = match dropdown.show_typed(ui, selected) {
            Some(Picked::Choice(i)) => items.get(i).cloned(),
            Some(Picked::Typed(text)) => Some(text),
            None => None,
        };
        if let Some(new) = new.filter(|new| *new != draft.kube.namespace) {
            draft.kube.namespace = new;
            draft.kube.resource.name.clear();
            draft.kube.forwards.clear();
        }
        field_errors(ui, &errors_for("the namespace"));
    });

    let kind = draft.kube.resource.kind;
    let resources = KubeQuery {
        context: draft.kube.context.clone(),
        what: KubeList::Resources {
            namespace: draft.kube.namespace.clone(),
            kind,
        },
    };
    let row_top = ui.cursor().top();
    editor_row(ui, ("Forward to", true, LABEL), |ui, _| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            let kinds = ResourceKind::ALL;
            let at = kinds.iter().position(|k| *k == kind).unwrap_or(1);
            if let Some(picked) = nav::segmented(
                ui,
                egui::Id::unique("kube_kind"),
                &["Pod", "Service", "Deployment"],
                at,
            ) {
                draft.kube.resource.kind = kinds[picked];
                draft.kube.resource.name.clear();
                draft.kube.forwards.clear();
            }
            let word = draft.kube.resource.kind.word();
            if draft.kube.namespace.is_empty() {
                ui.add_enabled_ui(false, |ui| {
                    Dropdown::<usize>::with_choices(
                        "kube_resource",
                        "Resource",
                        Vec::new(),
                        format!("Pick a {word}"),
                    )
                    .field(FIELD)
                    .show(ui, usize::MAX);
                });
                ui.label(
                    RichText::new("Pick a namespace first")
                        .size(12.0)
                        .color(p.dim),
                );
                return;
            }
            let open = dropdown::is_open(ui.ctx(), "kube_resource");
            let lookup = lookups.get(&resources, open, commands);
            let items: Vec<KubeItem> = match &lookup {
                Lookup::Done(Ok(items)) => items.clone(),
                _ => Vec::new(),
            };
            let choices: Vec<Choice<usize>> = items
                .iter()
                .enumerate()
                .map(|(i, item)| Choice {
                    detail: (!item.ports.is_empty()).then(|| ports_line(item, masked)),
                    note: item.status.clone(),
                    ..Choice::new(i, privacy.kube_name(&item.name))
                })
                .collect();
            let trigger = if draft.kube.resource.name.is_empty() {
                format!("Pick a {word}")
            } else {
                format!("{word}/{}", privacy.kube_name(&draft.kube.resource.name))
            };
            let selected = items
                .iter()
                .position(|i| i.name == draft.kube.resource.name)
                .unwrap_or(usize::MAX);
            let plural = draft.kube.resource.kind.plural();
            let mut dropdown =
                Dropdown::with_choices("kube_resource", "Resource", choices, trigger)
                    .field(FIELD)
                    .mono()
                    .panel_width(FIELD)
                    .searchable(plural);
            dropdown = match &lookup {
                Lookup::Loading => {
                    dropdown.loading(format!("Listing {plural} in {}…", draft.kube.namespace))
                }
                Lookup::Done(Err(e)) => {
                    dropdown.error(format!("Can't list {plural}: {e} · type a name instead"))
                }
                Lookup::Done(Ok(_)) => dropdown.with_caption(resources.describe()),
                Lookup::NotAsked => dropdown,
            };
            let new = match dropdown.show_typed(ui, selected) {
                Some(Picked::Choice(i)) => items.get(i).map(|i| i.name.clone()),
                Some(Picked::Typed(text)) => Some(text),
                None => None,
            };
            if let Some(new) = new.filter(|new| *new != draft.kube.resource.name) {
                draft.kube.resource.name = new;
                // One empty forward, with the resource's ports as chips.
                draft.kube.forwards = vec![KubeForward::default()];
            }
            field_errors(ui, &errors_for(&format!("the {word}")));
        });
    });
    if env.scroll_to_resource {
        let rect = ui.min_rect().with_min_y(row_top);
        ui.scroll_to_rect(rect, Some(Align::Min));
    }

    editor_row(ui, ("Reconnect", true, LABEL), |ui, _| {
        let size = vec2(ui.available_width(), 32.0);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            switch(ui, &mut draft.auto_reconnect, "Reconnect automatically");
            ui.label(
                RichText::new(
                    "restarts the forward when the pod restarts or the connection drops, with \
                     growing delay",
                )
                .size(12.0)
                .color(p.dim),
            );
        });
    });

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Forwards")
                .font(fonts::semibold(13.0))
                .color(p.text_strong),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new("one kubectl process for all forwards")
                    .size(12.0)
                    .color(p.dim),
            );
        });
    });
    editor_row(ui, ("Listen on", false, LABEL), |ui, label| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let exposed = draft.kube.is_exposed();
                ui.add(
                    TextField::new(&mut draft.kube.address)
                        .mono()
                        .prefilled()
                        .width(160.0)
                        .warning(exposed),
                )
                .labelled_by(label);
                ui.label(
                    RichText::new("kubectl --address, for every forward")
                        .size(12.0)
                        .color(p.dim),
                );
            });
            if draft.kube.is_exposed() {
                ui.horizontal_wrapped(|ui| {
                    icons::show(ui, icons::TRIANGLE_ALERT, icons::INLINE, p.yellow);
                    let style = masked::Style {
                        font: FontId::proportional(12.0),
                        color: p.yellow,
                        on_card: false,
                    };
                    let text = format!(
                        "{}: {}",
                        draft.kube.address,
                        exposed_hint(sshelf_core::tunnels::ForwardKind::L)
                    );
                    masked::text(ui, &text, &style, true);
                });
            }
            field_errors(ui, &errors_for("the listen address"));
        });
    });
    field_errors(ui, &errors_for("add at least"));
    let ports: Vec<sshelf_core::kubectl::KubePort> = lookups
        .items(&resources)
        .iter()
        .find(|i| i.name == draft.kube.resource.name)
        .map(|i| i.ports.clone())
        .unwrap_or_default();
    if draft.kube.forwards.is_empty() && draft.kube.resource.name.is_empty() {
        ui.label(
            RichText::new(format!(
                "Pick a {} to see its ports.",
                draft.kube.resource.kind.word()
            ))
            .size(12.0)
            .color(p.dim),
        );
    }
    let mut remove = None;
    let count = draft.kube.forwards.len();
    for index in 0..count {
        let prefix = format!("forward {}: ", index + 1);
        let errors: Vec<String> = env
            .problems
            .iter()
            .filter_map(|p| p.strip_prefix(&prefix))
            .map(capitalized)
            .collect();
        // Ports already used by the other forwards: no chips, no suggestion.
        let others: Vec<&KubeForward> = draft
            .kube
            .forwards
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, f)| f)
            .collect();
        let used_targets: Vec<String> = others.iter().map(|f| f.target.clone()).collect();
        let mut taken = env.taken.clone();
        taken.extend(others.iter().map(|f| f.listen));
        let free: Vec<&sshelf_core::kubectl::KubePort> = ports
            .iter()
            .filter(|p| !used_targets.contains(&p.port.to_string()))
            .collect();
        let forward = &mut draft.kube.forwards[index];
        if forward_card(ui, (index, forward), (&free, &taken), (&errors, kind)) {
            remove = Some(index);
        }
    }
    if let Some(index) = remove {
        draft.kube.forwards.remove(index);
    }
    if ui
        .add(Button::new("Add forward").icon(icons::PLUS).small())
        .clicked()
    {
        draft.kube.forwards.push(KubeForward::default());
    }

    command_preview(ui, draft);
}

/// "80/http · 9090/metrics" for a resource's ports (masked in streamer
/// mode).
fn ports_line(item: &KubeItem, masked: bool) -> String {
    item.ports
        .iter()
        .map(|p| {
            let port = if masked {
                DOTS.to_string()
            } else {
                p.port.to_string()
            };
            match &p.name {
                Some(name) => format!("{port}/{name}"),
                None => port,
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// One forward: *Local port* → *Port on the <kind>*, the remove button, and
/// while the remote port is empty the resource's `free` ports as chips. A
/// chip fills the port and suggests a local one not in `taken`. Returns
/// whether it is to be removed.
fn forward_card(
    ui: &mut Ui,
    (index, forward): (usize, &mut KubeForward),
    (free, taken): (&[&sshelf_core::kubectl::KubePort], &[u16]),
    (errors, kind): (&[String], ResourceKind),
) -> bool {
    let p = &Palette::ONE_DARK;
    const LOCAL: f32 = 110.0;
    const REMOTE: f32 = 180.0;
    let masked = privacy::get(ui.ctx()).on;
    let mut remove = false;
    Frame::new()
        .fill(p.surface)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin {
            left: 10,
            right: 10,
            top: 8,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.visuals_mut().extreme_bg_color = p.bg_dark;
            ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
            let small = |ui: &mut Ui, text: &str, width: f32| -> egui::Id {
                let (rect, _) = ui.allocate_exact_size(vec2(width, 14.0), egui::Sense::hover());
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                child
                    .add(
                        egui::Label::new(RichText::new(text).size(11.0).color(p.dim))
                            .selectable(false),
                    )
                    .id
            };
            let remote_label = match kind {
                ResourceKind::Service => "Port on the service",
                _ => "Port on the pod",
            };
            let mut ids = [egui::Id::NULL; 2];
            ui.horizontal(|ui| {
                ids[0] = small(ui, "Local port", LOCAL);
                ui.add_space(22.0);
                ids[1] = small(ui, remote_label, REMOTE);
            });
            ui.horizontal(|ui| {
                let mut listen = if forward.listen == 0 {
                    String::new()
                } else {
                    forward.listen.to_string()
                };
                let response = ui
                    .add(
                        TextField::new(&mut listen)
                            .mono()
                            .prefilled()
                            .hint("auto")
                            .width(LOCAL)
                            .error(
                                errors
                                    .iter()
                                    .any(|e| e.to_lowercase().contains("local port")),
                            ),
                    )
                    .labelled_by(ids[0]);
                if response.changed() {
                    if listen.trim().is_empty() {
                        forward.listen = 0;
                    } else if let Ok(port) = listen.trim().parse() {
                        forward.listen = port;
                    }
                }
                icons::show(ui, icons::ARROW_RIGHT, 14.0, p.dim);
                ui.add(
                    TextField::new(&mut forward.target)
                        .mono()
                        .prefilled()
                        .hint("port or name")
                        .width(REMOTE),
                )
                .labelled_by(ids[1]);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let name = format!("Remove forward {}", index + 1);
                    remove = ui
                        .add(Button::icon_only(icons::X, &name).ghost().small())
                        .clicked();
                });
            });
            if forward.target.is_empty() && !free.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for port in free {
                        let number = if masked {
                            DOTS.to_string()
                        } else {
                            port.port.to_string()
                        };
                        let text = match &port.name {
                            Some(name) => format!("{number} {name}"),
                            None => number,
                        };
                        if marks::filter_chip(ui, None, &text, false).clicked() {
                            forward.target = port.port.to_string();
                            if forward.listen == 0 {
                                forward.listen = suggest_local(port.port, taken);
                            }
                        }
                    }
                    ui.label(RichText::new("ports from kubectl").size(11.0).color(p.dim));
                });
            }
            for error in errors {
                ui.label(RichText::new(error).size(12.0).color(p.red));
            }
        });
    remove
}

/// The exact kubectl command, live (SPEC.md §10): missing parts read
/// `‹namespace›`, `‹name›`, `‹local:port›`; in streamer mode the context,
/// the ports and the address are masked tokens.
fn command_preview(ui: &mut Ui, draft: &Tunnel) {
    let p = &Palette::ONE_DARK;
    let kube = &draft.kube;
    // (text, sensitive)
    let mut parts: Vec<(String, bool)> = vec![("kubectl".into(), false)];
    if !kube.context.is_empty() {
        parts.push(("--context".into(), false));
        parts.push((kube.context.clone(), true));
    }
    // Names as aliases: "namespace N" while hidden.
    let privacy = privacy::get(ui.ctx());
    parts.push(("--namespace".into(), false));
    parts.push(if kube.namespace.is_empty() {
        ("‹namespace›".into(), false)
    } else {
        (privacy.kube_name(&kube.namespace).into_owned(), false)
    });
    parts.push(("port-forward".into(), false));
    let name = if kube.resource.name.is_empty() {
        "‹name›".to_string()
    } else {
        privacy.kube_name(&kube.resource.name).into_owned()
    };
    parts.push((format!("{}/{name}", kube.resource.kind.word()), false));
    let complete: Vec<&KubeForward> = kube
        .forwards
        .iter()
        .filter(|f| f.listen != 0 && !f.target.is_empty())
        .collect();
    if complete.is_empty() {
        parts.push(("‹local:port›".into(), false));
    }
    for forward in complete {
        parts.push((format!("{}:{}", forward.listen, forward.target), true));
    }
    parts.push(("--address".into(), false));
    parts.push((kube.address.clone(), true));
    let masked = privacy::get(ui.ctx()).on;
    Frame::new()
        .fill(p.bg_dark)
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("Command").size(12.0).color(p.dim));
                if !masked {
                    let line: Vec<&str> = parts.iter().map(|(t, _)| t.as_str()).collect();
                    ui.label(
                        RichText::new(line.join(" "))
                            .font(FontId::monospace(12.0))
                            .color(p.text),
                    );
                    return;
                }
                let style = masked::Style {
                    font: FontId::monospace(12.0),
                    color: p.text,
                    on_card: false,
                };
                for (text, sensitive) in &parts {
                    if *sensitive {
                        masked::value(ui, text, &style);
                    } else {
                        ui.label(
                            RichText::new(text)
                                .font(FontId::monospace(12.0))
                                .color(p.text),
                        );
                    }
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use sshelf_core::tunnels::{Forward, ForwardKind, Kube, TunnelType};

    use super::*;

    #[test]
    fn local_ports_are_suggested_as_the_spec_says() {
        assert_eq!(suggest_local(80, &[]), 18080);
        assert_eq!(suggest_local(443, &[]), 18443);
        assert_eq!(suggest_local(5432, &[]), 15432);
        assert_eq!(suggest_local(9090, &[]), 19090);
        assert_eq!(suggest_local(30080, &[]), 30080);
        // Taken by another tunnel: the next one up.
        assert_eq!(suggest_local(80, &[18080, 18081]), 18082);
        assert_eq!(suggest_local(u16::MAX, &[u16::MAX]), u16::MAX);
    }

    #[test]
    fn switching_keeps_the_name_and_the_local_ports() {
        let mut draft = Tunnel::ssh(
            "t",
            "web",
            "bastion",
            vec![Forward {
                kind: ForwardKind::L,
                bind: "127.0.0.1".into(),
                listen: 18080,
                target: Some("localhost:80".into()),
            }],
        );
        switch_type(&mut draft, TunnelType::Kubectl);
        assert_eq!(draft.kind, TunnelType::Kubectl);
        assert_eq!(draft.name, "web");
        assert_eq!(
            draft.kube.forwards,
            [KubeForward {
                listen: 18080,
                target: String::new()
            }]
        );
        // The ssh fields are kept until Save: switching back loses nothing.
        draft.kube.namespace = "api".into();
        switch_type(&mut draft, TunnelType::Ssh);
        assert_eq!(draft.host, "bastion");
        assert_eq!(draft.forwards[0].target.as_deref(), Some("localhost:80"));
        switch_type(&mut draft, TunnelType::Kubectl);
        assert_eq!(draft.kube.namespace, "api");
        // A type that has forwards of its own keeps them.
        let mut kube = Tunnel::kubectl(
            "k",
            "k",
            Kube {
                forwards: vec![KubeForward {
                    listen: 1,
                    target: "80".into(),
                }],
                ..Kube::default()
            },
        );
        kube.forwards.push(Forward {
            kind: ForwardKind::D,
            bind: "127.0.0.1".into(),
            listen: 1080,
            target: None,
        });
        switch_type(&mut kube, TunnelType::Ssh);
        assert_eq!(kube.forwards.len(), 1);
        assert_eq!(kube.forwards[0].listen, 1080);
    }

    #[test]
    fn lookups_are_asked_once_and_answered() {
        let mut lookups = Lookups::default();
        let query = KubeQuery {
            context: String::new(),
            what: KubeList::Namespaces,
        };
        let mut commands = Vec::new();
        assert!(matches!(
            lookups.get(&query, false, &mut commands),
            Lookup::NotAsked
        ));
        assert!(matches!(
            lookups.get(&query, true, &mut commands),
            Lookup::Loading
        ));
        assert!(matches!(
            lookups.get(&query, true, &mut commands),
            Lookup::Loading
        ));
        assert_eq!(commands, [Command::KubeLookup(query.clone())]);
        lookups.on_result(query.clone(), Err("Forbidden".into()));
        assert!(matches!(
            lookups.get(&query, true, &mut commands),
            Lookup::Done(Err(_))
        ));
        // An answer nobody asked for (the editor closed) is dropped.
        lookups.clear();
        lookups.on_result(query.clone(), Ok(Vec::new()));
        assert!(matches!(
            lookups.get(&query, false, &mut commands),
            Lookup::NotAsked
        ));
    }
}
