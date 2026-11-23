//! Native GPUI Desktop App for Job Harness.
//! Zero main-thread I/O: all Engine / SQLite / HTTP / disk calls execute off-thread so
//! every click, toggle, scroll, and drag runs smoothly at 120Hz.
use crate::{
    chat_bridge::{AgentRuntime, StreamEvent, extract_citations},
    theme,
    transport::{
        RoleCard, WorkspaceClient, WorkspaceSnapshot, WorkspaceTarget, column_to_stage,
        open_in_system, reveal_in_file_manager, spawn_change_listener,
    },
};
use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, ClipboardItem, Context, FocusHandle,
    Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render,
    ScrollHandle, ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, TouchPhase,
    Window, div, ease_out_quint, point, prelude::FluentBuilder, px, svg,
};
use notify::RecommendedWatcher;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
struct DragCard {
    record_path: String,
    company: String,
    from_stage: String,
    next_action: String,
    has_scheduled_start: bool,
}

struct DragGhost {
    company: String,
}

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py(px(7.0))
            .rounded(px(8.0))
            .bg(theme::popover_glass())
            .border_1()
            .border_color(theme::lavender_border())
            .shadow_lg()
            .text_xs()
            .text_color(theme::text_primary())
            .child(self.company.clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScrollAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveInput {
    None,
    Search,
    QuickAsk,
    Chat,
    CommandBar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivePopover {
    None,
    CommandBar,
    Bell,
    Settings,
    Budget,
    Health,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DrawerTab {
    Overview,
    Brief,
    Packet,
    Notes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RightDrawer {
    Closed,
    Role,
    Cvs,
    Tidy,
}

#[derive(Clone)]
enum CommandAction {
    OpenRole(Box<RoleCard>),
    ToggleChat,
    OpenCvs,
    OpenTidy,
    OpenBudget,
    OpenSettings,
    ToggleAttention,
    ToggleArchive,
    RefreshWorkspace,
    ImportMasterCv,
    AskAgent(String),
}

struct CommandEntry {
    label: String,
    meta: String,
    action: CommandAction,
}

#[derive(Clone, Debug)]
struct ChatMessage {
    is_user: bool,
    text: String,
    citations: Vec<String>,
}

#[derive(Clone, Debug)]
struct UndoMove {
    record_path: String,
    company: String,
    previous_stage: String,
    current_stage: String,
    next_action: String,
    has_scheduled_start: bool,
    created_at: Instant,
}

pub struct JobHarnessApp {
    client: WorkspaceClient,
    runtime: AgentRuntime,
    snapshot: WorkspaceSnapshot,
    loading: bool,
    refresh_in_flight: bool,
    _watcher: Option<RecommendedWatcher>,
    focus_handle: FocusHandle,

    // Layout & rails
    chat_open: bool,
    chat_scroll: ScrollHandle,
    board_scroll_x: ScrollHandle,
    col_scrolls: HashMap<&'static str, ScrollHandle>,
    scroll_axis_lock: Option<ScrollAxis>,
    right_drawer: RightDrawer,
    drawer_tab: DrawerTab,
    active_popover: ActivePopover,
    expanded_info: Option<&'static str>,
    active_input: ActiveInput,

    // Board filters & Command Bar (instant non-LLM)
    search_query: String,
    command_query: String,
    command_index: usize,
    show_archive: bool,
    only_attention: bool,
    attention_toggled: bool,
    interviews_expanded: bool,
    dismissed_tidy: HashSet<String>,

    // Scoped animation epochs (only trigger when that exact surface opens)
    chat_anim_epoch: u64,
    drawer_anim_epoch: u64,
    cmd_anim_epoch: u64,
    move_anim_epoch: u64,
    interviews_anim_epoch: u64,
    attention_anim_epoch: u64,
    last_moved_record: Option<(String, u64)>,

    // Selected role & citation drilldown
    selected_role: Option<RoleCard>,
    citation_title: Option<String>,
    citation_body: Option<String>,

    // Chat & quick-ask (streaming + queue + cancel)
    chat_messages: Vec<ChatMessage>,
    chat_input: String,
    quick_input: String,
    quick_popover_open: bool,
    quick_reply_preview: String,
    queued_prompt: Option<String>,
    streaming: bool,
    stream_status: String,
    stream_cancel: Option<Arc<AtomicBool>>,

    // Feedback toast + 5s Undo on drag/archive
    toast: Option<(String, Instant)>,
    undo_move: Option<UndoMove>,
}

impl Focusable for JobHarnessApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl JobHarnessApp {
    pub fn new(target: WorkspaceTarget, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let client = WorkspaceClient::new(target.clone());
        let runtime = AgentRuntime::detect();
        let (tx, rx) = mpsc::channel::<()>();
        let watcher = spawn_change_listener(target, tx);
        Self::start_watcher_loop(rx, cx);

        let mut app = Self {
            client,
            runtime,
            snapshot: WorkspaceSnapshot {
                healthy: true,
                status_note: "Loading workspace…".into(),
                ..Default::default()
            },
            loading: true,
            refresh_in_flight: false,
            _watcher: watcher,
            focus_handle: cx.focus_handle(),
            chat_open: false,
            chat_scroll: ScrollHandle::new(),
            board_scroll_x: ScrollHandle::new(),
            col_scrolls: [
                "Saved",
                "Applied",
                "Screening",
                "Interviews",
                "Technical",
                "Final",
                "Offer",
                "Archive",
            ]
            .into_iter()
            .map(|name| (name, ScrollHandle::new()))
            .collect(),
            scroll_axis_lock: None,
            right_drawer: RightDrawer::Closed,
            drawer_tab: DrawerTab::Overview,
            active_popover: ActivePopover::None,
            expanded_info: None,
            active_input: ActiveInput::None,
            search_query: String::new(),
            command_query: String::new(),
            command_index: 0,
            show_archive: false,
            only_attention: false,
            attention_toggled: false,
            interviews_expanded: false,
            dismissed_tidy: HashSet::new(),
            chat_anim_epoch: 1,
            drawer_anim_epoch: 1,
            cmd_anim_epoch: 1,
            move_anim_epoch: 1,
            interviews_anim_epoch: 1,
            attention_anim_epoch: 1,
            last_moved_record: None,
            selected_role: None,
            citation_title: None,
            citation_body: None,
            chat_messages: Vec::new(),
            chat_input: String::new(),
            quick_input: String::new(),
            quick_popover_open: false,
            quick_reply_preview: String::new(),
            queued_prompt: None,
            streaming: false,
            stream_status: String::new(),
            stream_cancel: None,
            toast: None,
            undo_move: None,
        };
        app.refresh_snapshot_async(cx);
        app
    }

    fn start_watcher_loop(rx: Receiver<()>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(800))
                    .await;
                let mut changed = false;
                while rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed {
                    // Debounce burst file saves quietly off-thread
                    cx.background_executor()
                        .timer(Duration::from_millis(250))
                        .await;
                    while rx.try_recv().is_ok() {}
                    let _ = this.update(cx, |app, cx| {
                        app.refresh_snapshot_async(cx);
                    });
                } else {
                    let _ = this.update(cx, |app, cx| {
                        let mut dirty = false;
                        if app
                            .toast
                            .as_ref()
                            .is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(5))
                        {
                            app.toast = None;
                            dirty = true;
                        }
                        if app
                            .undo_move
                            .as_ref()
                            .is_some_and(|u| u.created_at.elapsed() > Duration::from_secs(6))
                        {
                            app.undo_move = None;
                            dirty = true;
                        }
                        if dirty {
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }

    /// Loads workspace snapshot strictly on the background executor (zero UI-thread stalls).
    fn refresh_snapshot_async(&mut self, cx: &mut Context<Self>) {
        if self.refresh_in_flight {
            return;
        }
        self.refresh_in_flight = true;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { client.load_snapshot() })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.refresh_in_flight = false;
                app.loading = false;
                match result {
                    Ok(snap) => {
                        if let Some(sel) = &app.selected_role
                            && let Some(updated) = snap
                                .active_cards
                                .iter()
                                .chain(snap.archived_cards.iter())
                                .find(|c| c.record_path == sel.record_path)
                        {
                            app.selected_role = Some(updated.clone());
                        }
                        app.snapshot = snap;
                    }
                    Err(err) => {
                        app.snapshot.healthy = false;
                        app.snapshot.status_note = err.to_string();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_toast(&mut self, msg: impl Into<String>, cx: &mut Context<Self>) {
        self.toast = Some((msg.into(), Instant::now()));
        cx.notify();
    }

    fn close_overlays(&mut self) {
        self.active_popover = ActivePopover::None;
        self.expanded_info = None;
        self.quick_popover_open = false;
    }

    fn toggle_chat_rail(&mut self, cx: &mut Context<Self>) {
        self.chat_open = !self.chat_open;
        if self.chat_open {
            self.chat_anim_epoch = self.chat_anim_epoch.wrapping_add(1);
            self.active_input = ActiveInput::Chat;
        } else if self.active_input == ActiveInput::Chat {
            self.active_input = ActiveInput::None;
        }
        cx.notify();
    }

    fn open_right_drawer(&mut self, kind: RightDrawer, cx: &mut Context<Self>) {
        self.close_overlays();
        if self.right_drawer == kind {
            self.right_drawer = RightDrawer::Closed;
        } else {
            if self.right_drawer == RightDrawer::Closed {
                self.drawer_anim_epoch = self.drawer_anim_epoch.wrapping_add(1);
            }
            self.right_drawer = kind;
        }
        cx.notify();
    }

    fn open_role_drawer(&mut self, card: RoleCard, tab: DrawerTab, cx: &mut Context<Self>) {
        self.close_overlays();
        if self.right_drawer == RightDrawer::Closed {
            self.drawer_anim_epoch = self.drawer_anim_epoch.wrapping_add(1);
        }
        self.selected_role = Some(card);
        self.drawer_tab = tab;
        self.right_drawer = RightDrawer::Role;
        self.citation_title = None;
        self.citation_body = None;
        if tab == DrawerTab::Brief {
            self.load_selected_brief_async(cx);
        }
        cx.notify();
    }

    fn load_selected_brief_async(&mut self, cx: &mut Context<Self>) {
        let Some(role) = &self.selected_role else {
            return;
        };
        if let Some(brief) = role.brief_path.clone() {
            self.citation_title = Some(brief.clone());
            self.citation_body = Some("Loading brief…".into());
            self.read_citation_async(brief, cx);
        } else {
            self.citation_title = Some("No brief yet".into());
            self.citation_body = Some(
                "No brief is linked yet. Ask Chat to build a company brief and 10-line cheatsheet."
                    .into(),
            );
            cx.notify();
        }
    }

    fn read_citation_async(&mut self, normalized: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let title = normalized.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { client.read_text_preview(&normalized) })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.citation_title = Some(title);
                app.citation_body = Some(match res {
                    Ok(body) => body,
                    Err(err) => format!("Could not read file: {err}"),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn open_citation(&mut self, path: &str, cx: &mut Context<Self>) {
        let normalized = if path.starts_with("briefs/") {
            format!("applications/{path}")
        } else {
            path.to_owned()
        };
        if normalized.ends_with(".pdf") {
            if let Some(root) = self.client.target.local_root() {
                let full = root.join(&normalized);
                if full.is_file() {
                    open_in_system(&full);
                    self.set_toast(format!("Opened {normalized}"), cx);
                    return;
                }
            }
            self.open_right_drawer(RightDrawer::Cvs, cx);
            return;
        }
        if let Some(card) = self
            .snapshot
            .active_cards
            .iter()
            .chain(self.snapshot.archived_cards.iter())
            .find(|c| {
                c.record_path == normalized || c.brief_path.as_deref() == Some(normalized.as_str())
            })
            .cloned()
        {
            self.selected_role = Some(card);
        }
        if self.right_drawer == RightDrawer::Closed {
            self.drawer_anim_epoch = self.drawer_anim_epoch.wrapping_add(1);
        }
        self.right_drawer = RightDrawer::Role;
        self.citation_title = Some(normalized.clone());
        self.citation_body = Some("Loading…".into());
        cx.notify();
        self.read_citation_async(normalized, cx);
    }

    /// Optimistic frame-1 move; atomic `execute_batch` commits off-thread (zero UI freeze).
    fn move_card_optimistic(
        &mut self,
        drag: &DragCard,
        target_column: &'static str,
        cx: &mut Context<Self>,
    ) {
        let target_stage = column_to_stage(target_column);
        if drag.from_stage == target_stage {
            return;
        }
        let prev_active = self.snapshot.active_cards.clone();
        let prev_archived = self.snapshot.archived_cards.clone();

        if target_column == "Archive" {
            if let Some(pos) = self
                .snapshot
                .active_cards
                .iter()
                .position(|c| c.record_path == drag.record_path)
            {
                let mut card = self.snapshot.active_cards.remove(pos);
                card.stage = "closed".into();
                card.column = "Archive";
                self.snapshot.archived_cards.insert(0, card);
                self.snapshot.closed_count = self.snapshot.archived_cards.len();
            }
        } else if let Some(card) = self
            .snapshot
            .active_cards
            .iter_mut()
            .find(|c| c.record_path == drag.record_path)
        {
            card.stage = target_stage.to_owned();
            card.column = match target_column {
                "Technical" | "Final" | "Offer" => "Interviews",
                other => other,
            };
        } else if let Some(pos) = self
            .snapshot
            .archived_cards
            .iter()
            .position(|c| c.record_path == drag.record_path)
        {
            let mut card = self.snapshot.archived_cards.remove(pos);
            card.stage = target_stage.to_owned();
            card.column = match target_column {
                "Technical" | "Final" | "Offer" => "Interviews",
                other => other,
            };
            self.snapshot.active_cards.insert(0, card);
            self.snapshot.closed_count = self.snapshot.archived_cards.len();
        }

        self.move_anim_epoch = self.move_anim_epoch.wrapping_add(1);
        self.last_moved_record = Some((drag.record_path.clone(), self.move_anim_epoch));
        self.set_toast(format!("Moved {} → {}", drag.company, target_column), cx);

        let client = self.client.clone();
        let drag_owned = drag.clone();
        let drag_bg = drag_owned.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client.move_role_stage(
                        &drag_bg.record_path,
                        &drag_bg.from_stage,
                        target_stage,
                        &drag_bg.next_action,
                        drag_bg.has_scheduled_start,
                    )
                })
                .await;
            let _ = this.update(cx, |app, cx| match res {
                Ok(_) => {
                    app.undo_move = Some(UndoMove {
                        record_path: drag_owned.record_path,
                        company: drag_owned.company,
                        previous_stage: drag_owned.from_stage,
                        current_stage: target_stage.to_owned(),
                        next_action: drag_owned.next_action,
                        has_scheduled_start: drag_owned.has_scheduled_start,
                        created_at: Instant::now(),
                    });
                    app.refresh_snapshot_async(cx);
                    cx.notify();
                }
                Err(err) => {
                    app.snapshot.active_cards = prev_active;
                    app.snapshot.archived_cards = prev_archived;
                    app.snapshot.closed_count = app.snapshot.archived_cards.len();
                    app.last_moved_record = None;
                    app.set_toast(format!("Reverted: {err}"), cx);
                    app.refresh_snapshot_async(cx);
                }
            });
        })
        .detach();
    }

    fn perform_undo(&mut self, cx: &mut Context<Self>) {
        let Some(undo) = self.undo_move.take() else {
            return;
        };
        self.set_toast(format!("Undoing move for {}…", undo.company), cx);
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client.move_role_stage(
                        &undo.record_path,
                        &undo.current_stage,
                        &undo.previous_stage,
                        &undo.next_action,
                        undo.has_scheduled_start,
                    )
                })
                .await;
            let _ = this.update(cx, |app, cx| match res {
                Ok(_) => {
                    app.set_toast(format!("Undid move for {}", undo.company), cx);
                    app.refresh_snapshot_async(cx);
                }
                Err(err) => app.set_toast(format!("Undo failed: {err}"), cx),
            });
        })
        .detach();
    }

    fn archive_card_from_tidy(&mut self, card: &RoleCard, cx: &mut Context<Self>) {
        let drag = DragCard {
            record_path: card.record_path.clone(),
            company: card.company.clone(),
            from_stage: card.stage.clone(),
            next_action: card.next_action.clone(),
            has_scheduled_start: card.scheduled_start.is_some(),
        };
        self.move_card_optimistic(&drag, "Archive", cx);
    }

    fn adjust_channel_cap(&mut self, channel: &'static str, delta: i64, cx: &mut Context<Self>) {
        let b = &mut self.snapshot.daily_budget;
        let next_val = |cur: u64| -> u64 { (cur as i64 + delta).max(0) as u64 };
        match channel {
            "portal" => b.portal_limit = next_val(b.portal_limit),
            "direct_email" => b.email_limit = next_val(b.email_limit),
            "recruiter_outreach" => b.outreach_limit = next_val(b.outreach_limit),
            _ => return,
        }
        let total = b.portal_limit + b.email_limit + b.outreach_limit;
        b.limit = total;
        let (p, e, o) = (b.portal_limit, b.email_limit, b.outreach_limit);
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client.update_ledger_config(|cfg| {
                        cfg["max_daily_submissions"] = json!(total);
                        cfg["daily_channel_limits"] = json!({
                            "portal": p,
                            "direct_email": e,
                            "recruiter_outreach": o
                        });
                    })
                })
                .await;
            if let Err(err) = res {
                let _ = this.update(cx, |app, cx| {
                    app.set_toast(format!("Cap save failed: {err}"), cx);
                    app.refresh_snapshot_async(cx);
                });
            }
        })
        .detach();
    }

    fn toggle_permission(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let p = &mut self.snapshot.permissions;
        let new_val = match key {
            "auto_apply_portal" => {
                p.auto_apply_portal = !p.auto_apply_portal;
                p.auto_apply_portal
            }
            "auto_send_first_contact_email" => {
                p.auto_send_first_contact_email = !p.auto_send_first_contact_email;
                p.auto_send_first_contact_email
            }
            "auto_reply_threads" => {
                p.auto_reply_threads = !p.auto_reply_threads;
                p.auto_reply_threads
            }
            "auto_archive_mail" => {
                p.auto_archive_mail = !p.auto_archive_mail;
                p.auto_archive_mail
            }
            _ => return,
        };
        cx.notify();

        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client.update_ledger_config(|cfg| {
                        if !cfg["permissions"].is_object() {
                            cfg["permissions"] = json!({});
                        }
                        cfg["permissions"][key] = json!(new_val);
                    })
                })
                .await;
            if let Err(err) = res {
                let _ = this.update(cx, |app, cx| {
                    app.set_toast(format!("Save failed: {err}"), cx);
                    app.refresh_snapshot_async(cx);
                });
            }
        })
        .detach();
    }

    fn open_command_bar(&mut self, cx: &mut Context<Self>) {
        self.command_query.clear();
        self.command_index = 0;
        self.active_popover = ActivePopover::CommandBar;
        self.active_input = ActiveInput::CommandBar;
        self.cmd_anim_epoch = self.cmd_anim_epoch.wrapping_add(1);
        cx.notify();
    }

    fn tidy_candidates(&self) -> Vec<&RoleCard> {
        let mut quiet: Vec<&RoleCard> = self
            .snapshot
            .active_cards
            .iter()
            .filter(|c| {
                !self.dismissed_tidy.contains(&c.record_path)
                    && c.quiet_days.is_some_and(|d| d >= 5)
            })
            .collect();
        quiet.sort_by_key(|a| std::cmp::Reverse(a.quiet_days));
        quiet
    }

    fn tidy_count(&self) -> usize {
        self.tidy_candidates().len()
            + usize::from(self.snapshot.unresolved_attempts > 0)
            + usize::from(self.snapshot.suggest_new_track)
    }

    fn command_entries(&self) -> Vec<CommandEntry> {
        let q = self.command_query.trim().to_ascii_lowercase();
        let mut out = Vec::new();

        for card in self
            .snapshot
            .active_cards
            .iter()
            .chain(self.snapshot.archived_cards.iter())
        {
            if q.is_empty() && out.len() >= 3 {
                break;
            }
            if q.is_empty() || card.search_blob.contains(&q) {
                out.push(CommandEntry {
                    label: format!("{} — {}", card.company, card.title),
                    meta: card.column.to_owned(),
                    action: CommandAction::OpenRole(Box::new(card.clone())),
                });
                if out.len() >= 5 {
                    break;
                }
            }
        }

        let builtins = [
            ("Toggle Chat rail", "⌘\\", CommandAction::ToggleChat),
            (
                "Tidy quiet & stale roles",
                "Cleanup",
                CommandAction::OpenTidy,
            ),
            ("Open CVs & Tracks", "⌘D", CommandAction::OpenCvs),
            ("Daily Submission Cap", "Split", CommandAction::OpenBudget),
            ("Permissions & Timing", "Rules", CommandAction::OpenSettings),
            (
                "Toggle Urgent & Quiet filter",
                "Filter",
                CommandAction::ToggleAttention,
            ),
            (
                "Toggle Archive column",
                "Board",
                CommandAction::ToggleArchive,
            ),
            ("Refresh workspace", "⌘R", CommandAction::RefreshWorkspace),
            (
                "Import / Replace Master CV PDF",
                "CV",
                CommandAction::ImportMasterCv,
            ),
        ];
        for (label, meta, action) in builtins {
            if q.is_empty() || label.to_ascii_lowercase().contains(&q) {
                out.push(CommandEntry {
                    label: label.to_owned(),
                    meta: meta.to_owned(),
                    action,
                });
            }
        }

        let raw = self.command_query.trim();
        if !raw.is_empty() {
            out.push(CommandEntry {
                label: format!("Ask Chat: \"{raw}\""),
                meta: "⏎".into(),
                action: CommandAction::AskAgent(raw.to_owned()),
            });
        }
        out
    }

    fn execute_command(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        self.active_popover = ActivePopover::None;
        self.active_input = ActiveInput::None;
        match action {
            CommandAction::OpenRole(card) => {
                self.open_role_drawer(*card, DrawerTab::Overview, cx);
            }
            CommandAction::ToggleChat => self.toggle_chat_rail(cx),
            CommandAction::OpenCvs => self.open_right_drawer(RightDrawer::Cvs, cx),
            CommandAction::OpenTidy => self.open_right_drawer(RightDrawer::Tidy, cx),
            CommandAction::OpenBudget => {
                self.active_popover = ActivePopover::Budget;
                cx.notify();
            }
            CommandAction::OpenSettings => {
                self.active_popover = ActivePopover::Settings;
                cx.notify();
            }
            CommandAction::ToggleAttention => {
                self.toggle_attention_only(cx);
            }
            CommandAction::ToggleArchive => {
                self.show_archive = !self.show_archive;
                cx.notify();
            }
            CommandAction::RefreshWorkspace => {
                self.refresh_snapshot_async(cx);
                self.set_toast("Syncing workspace…", cx);
            }
            CommandAction::ImportMasterCv => self.import_cv_dialog(cx),
            CommandAction::AskAgent(prompt) => self.submit_prompt(prompt, false, cx),
        }
    }

    fn import_cv_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some(picked) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_file()
        {
            let client = self.client.clone();
            cx.spawn(async move |this, cx| {
                let res = cx
                    .background_executor()
                    .spawn(async move { client.import_primary_cv(&picked) })
                    .await;
                let _ = this.update(cx, |app, cx| match res {
                    Ok(rel) => {
                        app.set_toast(format!("Imported Master CV: {rel}"), cx);
                        app.refresh_snapshot_async(cx);
                    }
                    Err(e) => app.set_toast(format!("Import failed: {e}"), cx),
                });
            })
            .detach();
        }
    }

    fn submit_prompt(&mut self, prompt: String, from_quick_bar: bool, cx: &mut Context<Self>) {
        let trimmed = prompt.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        if self.streaming {
            self.queued_prompt = Some(trimmed);
            self.set_toast("Queued follow-up message", cx);
            return;
        }
        self.chat_messages.push(ChatMessage {
            is_user: true,
            text: trimmed.clone(),
            citations: vec![],
        });
        self.chat_messages.push(ChatMessage {
            is_user: false,
            text: String::new(),
            citations: vec![],
        });
        self.chat_scroll.scroll_to_bottom();
        self.streaming = true;
        self.stream_status = "Thinking…".into();
        if from_quick_bar {
            self.quick_popover_open = true;
            self.quick_reply_preview.clear();
        } else {
            if !self.chat_open {
                self.chat_anim_epoch = self.chat_anim_epoch.wrapping_add(1);
            }
            self.chat_open = true;
        }

        let (tx, rx) = mpsc::channel::<StreamEvent>();
        let cancel = Arc::new(AtomicBool::new(false));
        self.stream_cancel = Some(cancel.clone());
        self.runtime.stream_turn(
            self.client.target.local_root().map(PathBuf::from),
            trimmed,
            tx,
            cancel,
        );

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                let mut done = false;
                let mut batch = Vec::new();
                while let Ok(ev) = rx.try_recv() {
                    if matches!(ev, StreamEvent::Done) {
                        done = true;
                    }
                    batch.push(ev);
                }
                if !batch.is_empty() {
                    let _ = this.update(cx, |app, cx| {
                        for ev in batch {
                            match ev {
                                StreamEvent::Status(s) => app.stream_status = s,
                                StreamEvent::Delta(chunk) => {
                                    if let Some(last) = app.chat_messages.last_mut() {
                                        last.text.push_str(&chunk);
                                        last.citations = extract_citations(&last.text);
                                        app.quick_reply_preview = last.text.clone();
                                    }
                                    app.chat_scroll.scroll_to_bottom();
                                }
                                StreamEvent::Error(err) => {
                                    if let Some(last) = app.chat_messages.last_mut() {
                                        last.text = format!("Error: {err}");
                                        app.quick_reply_preview = last.text.clone();
                                    }
                                }
                                StreamEvent::Done => {
                                    app.streaming = false;
                                    app.stream_status.clear();
                                    app.stream_cancel = None;
                                    app.refresh_snapshot_async(cx);
                                    if let Some(next_q) = app.queued_prompt.take() {
                                        app.submit_prompt(next_q, false, cx);
                                    }
                                }
                            }
                        }
                        cx.notify();
                    });
                }
                if done {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn stop_stream(&mut self, cx: &mut Context<Self>) {
        if let Some(flag) = &self.stream_cancel {
            flag.store(true, Ordering::Relaxed);
        }
        self.streaming = false;
        self.stream_status.clear();
        cx.notify();
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let cmd = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;

        if cmd && key == "\\" {
            self.toggle_chat_rail(cx);
            return;
        }
        if cmd && key == "k" {
            if self.active_popover == ActivePopover::CommandBar {
                self.active_popover = ActivePopover::None;
                self.active_input = ActiveInput::None;
                cx.notify();
            } else {
                self.open_command_bar(cx);
            }
            return;
        }
        if cmd && key == "r" {
            self.refresh_snapshot_async(cx);
            self.set_toast("Syncing workspace…", cx);
            return;
        }
        if cmd && key == "d" {
            self.open_right_drawer(RightDrawer::Cvs, cx);
            return;
        }
        if cmd && key == "z" && self.undo_move.is_some() {
            self.perform_undo(cx);
            return;
        }
        if key == "escape" {
            self.close_overlays();
            self.right_drawer = RightDrawer::Closed;
            self.active_input = ActiveInput::None;
            cx.notify();
            return;
        }

        if self.active_input == ActiveInput::CommandBar {
            let entries = self.command_entries();
            match key {
                "down" => {
                    if !entries.is_empty() {
                        self.command_index = (self.command_index + 1) % entries.len();
                        cx.notify();
                    }
                    return;
                }
                "up" => {
                    if !entries.is_empty() {
                        self.command_index =
                            (self.command_index + entries.len() - 1) % entries.len();
                        cx.notify();
                    }
                    return;
                }
                "enter" => {
                    if let Some(entry) = entries.into_iter().nth(self.command_index) {
                        self.execute_command(entry.action, cx);
                    }
                    return;
                }
                "backspace" => {
                    self.command_query.pop();
                    self.command_index = 0;
                    cx.notify();
                    return;
                }
                _ => {
                    if !cmd
                        && let Some(ch) = &event.keystroke.key_char
                        && !ch.chars().any(char::is_control)
                    {
                        self.command_query.push_str(ch);
                        self.command_index = 0;
                        cx.notify();
                    }
                    return;
                }
            }
        }

        let target_buf = match self.active_input {
            ActiveInput::Search => Some(&mut self.search_query),
            ActiveInput::QuickAsk => Some(&mut self.quick_input),
            ActiveInput::Chat => Some(&mut self.chat_input),
            ActiveInput::None | ActiveInput::CommandBar => None,
        };
        if let Some(buf) = target_buf {
            match key {
                "backspace" => {
                    buf.pop();
                    cx.notify();
                }
                "enter" => match self.active_input {
                    ActiveInput::QuickAsk => {
                        let q = std::mem::take(&mut self.quick_input);
                        self.submit_prompt(q, true, cx);
                    }
                    ActiveInput::Chat => {
                        let q = std::mem::take(&mut self.chat_input);
                        self.submit_prompt(q, false, cx);
                    }
                    _ => {}
                },
                _ => {
                    if !cmd
                        && let Some(ch) = &event.keystroke.key_char
                        && !ch.chars().any(char::is_control)
                    {
                        buf.push_str(ch);
                        cx.notify();
                    }
                }
            }
        }
    }

    fn filtered_cards(&self) -> Vec<&RoleCard> {
        let needle = self.search_query.trim().to_ascii_lowercase();
        let iter: Box<dyn Iterator<Item = &RoleCard>> = if self.show_archive {
            Box::new(
                self.snapshot
                    .active_cards
                    .iter()
                    .chain(self.snapshot.archived_cards.iter()),
            )
        } else {
            Box::new(self.snapshot.active_cards.iter())
        };
        iter.filter(|c| {
            if self.only_attention && c.urgency == "background" {
                return false;
            }
            needle.is_empty() || c.search_blob.contains(&needle)
        })
        .collect()
    }

    fn health_color(&self) -> gpui::Rgba {
        if !self.snapshot.healthy {
            theme::rose_text()
        } else if !self.runtime.is_ready() || self.snapshot.unfinished_runs > 0 {
            theme::amber_accent()
        } else {
            theme::emerald()
        }
    }

    fn render_topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let urgent = self.snapshot.urgent_count > 0;
        let chat_open = self.chat_open;
        let cvs_open = self.right_drawer == RightDrawer::Cvs;
        let tidy_open = self.right_drawer == RightDrawer::Tidy;
        let tidy_n = self.tidy_count();
        let ws_label = self.client.target.label();

        div()
            .h(px(44.0))
            .w_full()
            .flex_shrink_0()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .pl(px(72.0))
                    // Icon-only Chat sidebar toggle (exact Codex/Zeron vector icon)
                    .child(
                        div()
                            .id("toggle-chat-rail")
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(chat_open, |d| d.bg(theme::surface_active()))
                            .hover(|s| s.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_chat_rail(cx);
                            }))
                            .child(
                                svg()
                                    .path("icons/panel-left.svg")
                                    .size(px(16.0))
                                    .text_color(if chat_open {
                                        theme::text_primary()
                                    } else {
                                        theme::text_secondary()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(ws_label),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("btn-cmd-k")
                            .px_2()
                            .py_1()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::row_hover()))
                            .text_xs()
                            .text_color(theme::text_muted())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_command_bar(cx);
                            }))
                            .child("⌘K"),
                    )
                    // Tidy / Cleanup button
                    .child(
                        div()
                            .id("btn-tidy")
                            .px_2()
                            .py_1()
                            .rounded(px(6.0))
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .when(tidy_open, |d| d.bg(theme::surface_active()))
                            .hover(|s| s.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_right_drawer(RightDrawer::Tidy, cx);
                            }))
                            .child(svg().path("icons/tidy.svg").size(px(14.0)).text_color(
                                if tidy_n > 0 {
                                    theme::lavender()
                                } else {
                                    theme::text_secondary()
                                },
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if tidy_open {
                                        theme::text_primary()
                                    } else {
                                        theme::text_secondary()
                                    })
                                    .child(if tidy_n > 0 {
                                        format!("Tidy · {tidy_n}")
                                    } else {
                                        "Tidy".into()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("btn-cvs")
                            .px_2()
                            .py_1()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .when(cvs_open, |d| d.bg(theme::surface_active()))
                            .hover(|s| s.bg(theme::row_hover()))
                            .text_xs()
                            .text_color(if cvs_open {
                                theme::text_primary()
                            } else {
                                theme::text_secondary()
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_right_drawer(RightDrawer::Cvs, cx);
                            }))
                            .child("CVs"),
                    )
                    .child(
                        div()
                            .id("btn-bell")
                            .relative()
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(self.active_popover == ActivePopover::Bell, |d| {
                                d.bg(theme::surface_active())
                            })
                            .hover(|s| s.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_popover = if this.active_popover == ActivePopover::Bell
                                {
                                    ActivePopover::None
                                } else {
                                    ActivePopover::Bell
                                };
                                cx.notify();
                            }))
                            .child(
                                svg()
                                    .path("icons/bell.svg")
                                    .size(px(16.0))
                                    .text_color(theme::text_secondary()),
                            )
                            .when(urgent, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top(px(5.0))
                                        .right(px(5.0))
                                        .w(px(6.0))
                                        .h(px(6.0))
                                        .rounded(px(99.0))
                                        .bg(theme::amber_accent()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("btn-settings")
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded(px(7.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(self.active_popover == ActivePopover::Settings, |d| {
                                d.bg(theme::surface_active())
                            })
                            .hover(|s| s.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_popover =
                                    if this.active_popover == ActivePopover::Settings {
                                        ActivePopover::None
                                    } else {
                                        ActivePopover::Settings
                                    };
                                cx.notify();
                            }))
                            .child(
                                svg()
                                    .path("icons/settings.svg")
                                    .size(px(16.0))
                                    .text_color(theme::text_secondary()),
                            ),
                    ),
            )
    }

    fn render_chat_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let input_active = self.active_input == ActiveInput::Chat;
        let display_input = if self.chat_input.is_empty() {
            if self.streaming {
                "Queue a follow-up…".to_owned()
            } else {
                "Message your agent…".to_owned()
            }
        } else {
            self.chat_input.clone()
        };

        div()
            .w(px(300.0))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(theme::panel_glass())
            .border_r_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme::hairline())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_primary())
                            .child(self.runtime.label()),
                    )
                    .when(!self.chat_messages.is_empty(), |d| {
                        d.child(
                            div()
                                .id("chat-jump-bottom")
                                .px_1()
                                .rounded(px(4.0))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::row_hover()))
                                .text_xs()
                                .text_color(theme::text_muted())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.chat_scroll.scroll_to_bottom();
                                    cx.notify();
                                }))
                                .child("↓"),
                        )
                    }),
            )
            .child(
                div()
                    .id("chat-scroll")
                    .track_scroll(&self.chat_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .when(self.chat_messages.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("Connected directly to your host agent session. Ask what needs attention today, request a brief, or draft a reply."),
                        )
                    })
                    .children(self.chat_messages.iter().enumerate().map(|(idx, msg)| {
                        let mut block = div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .when(msg.is_user, |d| {
                                        d.px_3()
                                            .py_2()
                                            .rounded(px(8.0))
                                            .bg(theme::surface_subtle())
                                    })
                                    .text_xs()
                                    .text_color(if msg.is_user {
                                        theme::text_primary()
                                    } else {
                                        theme::text_secondary()
                                    })
                                    .child(if msg.text.is_empty() && !msg.is_user {
                                        self.stream_status.clone()
                                    } else {
                                        msg.text.clone()
                                    }),
                            );
                        if !msg.citations.is_empty() {
                            let links = div().flex().flex_wrap().gap_2().pt_1().children(
                                msg.citations.iter().enumerate().map(|(ci, path)| {
                                    let p = path.clone();
                                    let short = path.rsplit('/').next().unwrap_or(path).to_owned();
                                    div()
                                        .id(SharedString::from(format!("cite-{idx}-{ci}")))
                                        .text_xs()
                                        .text_color(theme::lavender())
                                        .cursor_pointer()
                                        .hover(|s| s.text_color(theme::text_primary()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.open_citation(&p, cx);
                                        }))
                                        .child(format!("↗ {short}"))
                                }),
                            );
                            block = block.child(links);
                        }
                        block
                    })),
            )
            .when_some(self.queued_prompt.clone(), |d, queued| {
                d.child(
                    div()
                        .mx_3()
                        .mb_1()
                        .px_2()
                        .py_1()
                        .rounded(px(6.0))
                        .bg(theme::lavender_wash())
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_xs()
                        .text_color(theme::lavender())
                        .child(format!("Queued: {queued}"))
                        .child(
                            div()
                                .id("cancel-queue")
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.queued_prompt = None;
                                    cx.notify();
                                }))
                                .child("✕"),
                        ),
                )
            })
            .child(
                div()
                    .p_3()
                    .border_t_1()
                    .border_color(theme::hairline())
                    .child(
                        div()
                            .id("chat-input-box")
                            .px_3()
                            .py_2()
                            .rounded(px(9.0))
                            .bg(theme::surface_subtle())
                            .border_1()
                            .border_color(if input_active {
                                theme::hairline_strong()
                            } else {
                                theme::hairline()
                            })
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_text()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_input = ActiveInput::Chat;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_xs()
                                    .text_color(if self.chat_input.is_empty() {
                                        theme::text_muted()
                                    } else {
                                        theme::text_primary()
                                    })
                                    .child(display_input),
                            )
                            .when(self.streaming, |d| {
                                d.child(
                                    div()
                                        .id("stop-stream")
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded(px(5.0))
                                        .bg(theme::rose_wash())
                                        .text_xs()
                                        .text_color(theme::rose_text())
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.stop_stream(cx);
                                        }))
                                        .child("Stop"),
                                )
                            }),
                    ),
            )
    }

    fn toggle_attention_only(&mut self, cx: &mut Context<Self>) {
        self.only_attention = !self.only_attention;
        self.attention_toggled = true;
        self.attention_anim_epoch = self.attention_anim_epoch.wrapping_add(1);
        if self.only_attention {
            self.board_scroll_x.set_offset(point(px(0.0), px(0.0)));
        }
        cx.notify();
    }

    /// Dominant-axis gesture lock so trackpad vertical list scrolling and horizontal
    /// Kanban swiping never fight each other.
    fn handle_board_scroll(
        &mut self,
        col_name: Option<&'static str>,
        event: &ScrollWheelEvent,
        cx: &mut Context<Self>,
    ) {
        if matches!(event.touch_phase, TouchPhase::Started) {
            self.scroll_axis_lock = None;
        }
        let delta = event.delta.pixel_delta(px(24.0));
        let dx: f32 = delta.x.into();
        let dy: f32 = delta.y.into();

        if self.scroll_axis_lock.is_none() && (dx.abs() > 0.8 || dy.abs() > 0.8) {
            self.scroll_axis_lock = if dx.abs() > dy.abs() * 1.15 || col_name.is_none() {
                Some(ScrollAxis::Horizontal)
            } else {
                Some(ScrollAxis::Vertical)
            };
        }

        match self.scroll_axis_lock {
            Some(ScrollAxis::Horizontal) => {
                let cur = self.board_scroll_x.offset();
                let max_x: f32 = self.board_scroll_x.max_offset().width.into();
                let cur_x: f32 = cur.x.into();
                let next_x = (cur_x + dx).clamp(-max_x.max(0.0), 0.0);
                self.board_scroll_x.set_offset(point(px(next_x), cur.y));
                cx.stop_propagation();
                cx.notify();
            }
            Some(ScrollAxis::Vertical) => {
                if let Some(name) = col_name
                    && let Some(handle) = self.col_scrolls.get(name)
                {
                    let cur = handle.offset();
                    let max_y: f32 = handle.max_offset().height.into();
                    let cur_y: f32 = cur.y.into();
                    let next_y = (cur_y + dy).clamp(-max_y.max(0.0), 0.0);
                    handle.set_offset(point(cur.x, px(next_y)));
                    cx.stop_propagation();
                    cx.notify();
                }
            }
            None => {}
        }

        if matches!(event.touch_phase, TouchPhase::Ended) {
            self.scroll_axis_lock = None;
        }
    }

    fn render_kanban_column(
        &self,
        col_name: &'static str,
        col_cards: Vec<&RoleCard>,
        col_total: usize,
        expandable_interviews: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let scroll_handle = self.col_scrolls.get(col_name).cloned();
        let hidden_by_attention = self.only_attention && col_total == 0;
        let col_div = div()
            .id(SharedString::from(format!("col-{col_name}")))
            .w(px(268.0))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded(px(10.0))
            .px_1()
            .drag_over::<DragCard>(|style, _, _, _| style.bg(theme::drop_highlight()))
            .on_drop(cx.listener(move |this, drag: &DragCard, _, cx| {
                this.move_card_optimistic(drag, col_name, cx);
            }))
            .child(
                div()
                    .px_2()
                    .pb_2()
                    .mb_1()
                    .border_b_1()
                    .border_color(theme::hairline())
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(if expandable_interviews {
                        div()
                            .id("expand-interviews-col")
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(theme::lavender())
                            .hover(|s| s.text_color(theme::text_primary()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.interviews_expanded = true;
                                this.interviews_anim_epoch =
                                    this.interviews_anim_epoch.wrapping_add(1);
                                cx.notify();
                            }))
                            .child("Interviews  ›")
                            .into_any_element()
                    } else {
                        div()
                            .text_xs()
                            .text_color(theme::text_secondary())
                            .child(col_name)
                            .into_any_element()
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(col_total.to_string()),
                    ),
            )
            .child({
                let mut scroller = div()
                    .id(SharedString::from(format!("col-scroll-{col_name}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _, cx| {
                        this.handle_board_scroll(Some(col_name), ev, cx);
                    }))
                    .flex()
                    .flex_col();
                if let Some(h) = scroll_handle {
                    scroller = scroller.track_scroll(&h);
                }
                scroller.children(col_cards.into_iter().map(|card| {
                    let c = card.clone();
                    let drag_payload = DragCard {
                        record_path: card.record_path.clone(),
                        company: card.company.clone(),
                        from_stage: card.stage.clone(),
                        next_action: card.next_action.clone(),
                        has_scheduled_start: card.scheduled_start.is_some(),
                    };
                    let is_urgent = card.urgency == "urgent_now";
                    let is_quiet = card.quiet_days.is_some();
                    let just_landed = self
                        .last_moved_record
                        .as_ref()
                        .filter(|(p, _)| p == &card.record_path)
                        .map(|(_, epoch)| *epoch);

                    let mut row = div()
                        .id(SharedString::from(card.record_path.clone()))
                        .w_full()
                        .min_w_0()
                        .px_2()
                        .py(px(9.0))
                        .rounded(px(7.0))
                        .border_b_1()
                        .border_color(theme::hairline())
                        .when(is_urgent, |d| d.bg(theme::amber_wash()))
                        .hover(|s| s.bg(theme::row_hover()))
                        .cursor_pointer()
                        .flex()
                        .gap_2()
                        .on_drag(drag_payload, |drag, _, _, cx| {
                            cx.new(|_| DragGhost {
                                company: drag.company.clone(),
                            })
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_role_drawer(c.clone(), DrawerTab::Overview, cx);
                        }))
                        .when(is_urgent, |d| {
                            d.child(
                                div()
                                    .w(px(2.0))
                                    .flex_shrink_0()
                                    .rounded(px(2.0))
                                    .bg(theme::amber_accent()),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_xs()
                                                .text_color(theme::text_primary())
                                                .child(card.company.clone()),
                                        )
                                        .when_some(card.note_line.clone(), |d, note| {
                                            d.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .text_xs()
                                                    .text_color(if is_urgent {
                                                        theme::amber_text()
                                                    } else if is_quiet {
                                                        theme::rose_text()
                                                    } else {
                                                        theme::text_muted()
                                                    })
                                                    .child(note),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .mt(px(2.0))
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(card.title.clone()),
                                ),
                        );
                    if just_landed.is_some() {
                        row = row.bg(theme::lavender_wash());
                    }
                    if let Some(epoch) = just_landed {
                        row.with_animation(
                            SharedString::from(format!("land-{epoch}")),
                            Animation::new(Duration::from_millis(200))
                                .with_easing(ease_out_quint()),
                            |el, t| el.opacity(0.6 + 0.4 * t),
                        )
                        .into_any_element()
                    } else {
                        row.into_any_element()
                    }
                }))
            });

        // Smoothly collapse empty columns to 0px when Attention-only is active so
        // active columns glide cleanly into place, and expand back when toggled off.
        if !self.attention_toggled {
            return div()
                .mr_4()
                .flex_shrink_0()
                .h_full()
                .child(col_div)
                .into_any_element();
        }
        let epoch = self.attention_anim_epoch;
        if hidden_by_attention {
            div()
                .h_full()
                .flex_shrink_0()
                .overflow_hidden()
                .child(col_div)
                .with_animation(
                    SharedString::from(format!("att-hide-{col_name}-{epoch}")),
                    Animation::new(Duration::from_millis(220)).with_easing(ease_out_quint()),
                    |el, t| {
                        let inv = 1.0 - t;
                        el.w(px(268.0 * inv)).mr(px(16.0 * inv)).opacity(inv)
                    },
                )
                .into_any_element()
        } else if !self.only_attention {
            div()
                .h_full()
                .flex_shrink_0()
                .overflow_hidden()
                .child(col_div)
                .with_animation(
                    SharedString::from(format!("att-show-{col_name}-{epoch}")),
                    Animation::new(Duration::from_millis(220)).with_easing(ease_out_quint()),
                    |el, t| el.w(px(268.0 * t)).mr(px(16.0 * t)).opacity(t),
                )
                .into_any_element()
        } else {
            div()
                .mr_4()
                .flex_shrink_0()
                .h_full()
                .child(col_div)
                .into_any_element()
        }
    }

    fn render_board(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cards = self.filtered_cards();
        let search_active = self.active_input == ActiveInput::Search;
        let quick_active = self.active_input == ActiveInput::QuickAsk;
        let base_columns: [&'static str; 3] = ["Saved", "Applied", "Screening"];
        let interview_stages: [(&'static str, &'static str); 3] = [
            ("Technical", "technical"),
            ("Final", "final"),
            ("Offer", "offer"),
        ];
        let interviews_total = cards.iter().filter(|c| c.column == "Interviews").count();
        let archive_cards: Vec<&RoleCard> = cards
            .iter()
            .copied()
            .filter(|c| c.column == "Archive")
            .take(28)
            .collect();
        let archive_total = cards.iter().filter(|c| c.column == "Archive").count();

        div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .px_4()
            .pt_3()
            .pb(px(56.0))
            .gap_3()
            // Subtle search & inline view toggles (no boxed pills)
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .id("search-box")
                            .w(px(280.0))
                            .px_3()
                            .py(px(6.0))
                            .rounded(px(8.0))
                            .bg(theme::surface_subtle())
                            .border_1()
                            .border_color(if search_active {
                                theme::hairline_strong()
                            } else {
                                theme::hairline()
                            })
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_text()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_input = ActiveInput::Search;
                                cx.notify();
                            }))
                            .child(
                                svg()
                                    .path("icons/search.svg")
                                    .size(px(13.0))
                                    .text_color(theme::text_muted()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_xs()
                                    .text_color(if self.search_query.is_empty() {
                                        theme::text_muted()
                                    } else {
                                        theme::text_primary()
                                    })
                                    .child(if self.search_query.is_empty() {
                                        "Filter roles…".to_owned()
                                    } else {
                                        self.search_query.clone()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("toggle-attention-only")
                                    .px_2()
                                    .py_1()
                                    .rounded(px(6.0))
                                    .cursor_pointer()
                                    .when(self.only_attention, |d| d.bg(theme::surface_active()))
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .text_xs()
                                    .text_color(if self.only_attention {
                                        theme::amber_text()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_attention_only(cx);
                                    }))
                                    .child("Attention only"),
                            )
                            .child(
                                div()
                                    .id("toggle-archive-col")
                                    .px_2()
                                    .py_1()
                                    .rounded(px(6.0))
                                    .cursor_pointer()
                                    .when(self.show_archive, |d| d.bg(theme::surface_active()))
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .text_xs()
                                    .text_color(if self.show_archive {
                                        theme::text_primary()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_archive = !this.show_archive;
                                        cx.notify();
                                    }))
                                    .child(format!("Archive ({})", self.snapshot.closed_count)),
                            ),
                    ),
            )
            // Horizontally-scrollable Kanban board with axis-locked scroll & expandable Interviews group
            .child(
                div()
                    .id("kanban-scroll-x")
                    .track_scroll(&self.board_scroll_x)
                    .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                        this.handle_board_scroll(None, ev, cx);
                    }))
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_x_scroll()
                    .flex()
                    .children(base_columns.into_iter().map(|col_name| {
                        let col_cards: Vec<&RoleCard> = cards
                            .iter()
                            .copied()
                            .filter(|c| c.column == col_name)
                            .take(28)
                            .collect();
                        let col_total = cards.iter().filter(|c| c.column == col_name).count();
                        self.render_kanban_column(col_name, col_cards, col_total, false, cx)
                    }))
                    .child(if !self.interviews_expanded {
                        let col_cards: Vec<&RoleCard> = cards
                            .iter()
                            .copied()
                            .filter(|c| c.column == "Interviews")
                            .take(28)
                            .collect();
                        self.render_kanban_column(
                            "Interviews",
                            col_cards,
                            interviews_total,
                            true,
                            cx,
                        )
                        .into_any_element()
                    } else {
                        let epoch = self.interviews_anim_epoch;
                        div()
                            .h_full()
                            .flex_shrink_0()
                            .px_3()
                            .py_2()
                            .rounded(px(12.0))
                            .bg(theme::interview_group_bg())
                            .border_1()
                            .border_color(theme::interview_group_border())
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .px_1()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .id("collapse-interviews-col")
                                            .cursor_pointer()
                                            .text_xs()
                                            .text_color(theme::lavender())
                                            .hover(|s| s.text_color(theme::text_primary()))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.interviews_expanded = false;
                                                cx.notify();
                                            }))
                                            .child(format!("‹  Interviews ({interviews_total})")),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .child("Interview stages"),
                                    ),
                            )
                            .child(div().flex_1().min_h_0().flex().gap_3().children(
                                interview_stages.into_iter().map(|(label, stage)| {
                                    let stage_cards: Vec<&RoleCard> = cards
                                        .iter()
                                        .copied()
                                        .filter(|c| c.stage == stage)
                                        .take(28)
                                        .collect();
                                    let total = cards.iter().filter(|c| c.stage == stage).count();
                                    self.render_kanban_column(label, stage_cards, total, false, cx)
                                }),
                            ))
                            .with_animation(
                                SharedString::from(format!("iv-expand-{epoch}")),
                                Animation::new(Duration::from_millis(180))
                                    .with_easing(ease_out_quint()),
                                |el, t| el.opacity(t),
                            )
                            .into_any_element()
                    })
                    .when(self.show_archive, |d| {
                        d.child(self.render_kanban_column(
                            "Archive",
                            archive_cards,
                            archive_total,
                            false,
                            cx,
                        ))
                    }),
            )
            // Quick-Ask upward reply popover
            .when(self.quick_popover_open, |d| {
                d.child(
                    div()
                        .absolute()
                        .left_4()
                        .bottom(px(54.0))
                        .w(px(400.0))
                        .p_3()
                        .rounded(px(12.0))
                        .bg(theme::popover_glass())
                        .border_1()
                        .border_color(theme::hairline_strong())
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_xs().text_color(theme::text_secondary()).child(
                                    if self.streaming {
                                        self.stream_status.clone()
                                    } else {
                                        "Quick reply".into()
                                    },
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .gap_3()
                                        .child(
                                            div()
                                                .id("expand-to-chat")
                                                .text_xs()
                                                .text_color(theme::lavender())
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.quick_popover_open = false;
                                                    if !this.chat_open {
                                                        this.chat_anim_epoch =
                                                            this.chat_anim_epoch.wrapping_add(1);
                                                    }
                                                    this.chat_open = true;
                                                    this.active_input = ActiveInput::Chat;
                                                    cx.notify();
                                                }))
                                                .child("Open in Chat →"),
                                        )
                                        .child(
                                            div()
                                                .id("close-quick")
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.quick_popover_open = false;
                                                    cx.notify();
                                                }))
                                                .child("✕"),
                                        ),
                                ),
                        )
                        .child(div().text_xs().text_color(theme::text_primary()).child(
                            if self.quick_reply_preview.is_empty() {
                                "Working…".to_owned()
                            } else {
                                self.quick_reply_preview.chars().take(420).collect()
                            },
                        )),
                )
            })
            // Bottom floating bar: Quick Ask + borderless 0 / 25 + subtle emerald status dot
            .child(
                div()
                    .absolute()
                    .left_4()
                    .right_4()
                    .bottom_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .id("quick-ask-bar")
                            .flex_1()
                            .max_w(px(380.0))
                            .px_3()
                            .py(px(7.0))
                            .rounded(px(9.0))
                            .bg(theme::dock_glass())
                            .border_1()
                            .border_color(if quick_active {
                                theme::hairline_strong()
                            } else {
                                theme::hairline()
                            })
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_text()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_input = ActiveInput::QuickAsk;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_xs()
                                    .text_color(if self.quick_input.is_empty() {
                                        theme::text_muted()
                                    } else {
                                        theme::text_primary()
                                    })
                                    .child(if self.quick_input.is_empty() {
                                        "Ask anything…".to_owned()
                                    } else {
                                        self.quick_input.clone()
                                    }),
                            )
                            .child(div().text_xs().text_color(theme::text_muted()).child("⏎")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .when_some(self.toast.clone(), |d, (msg, _)| {
                                let has_undo = self.undo_move.is_some();
                                d.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded(px(6.0))
                                        .bg(theme::dock_glass())
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(theme::text_secondary())
                                        .child(msg)
                                        .when(has_undo, |t| {
                                            t.child(
                                                div()
                                                    .id("undo-btn")
                                                    .text_color(theme::lavender())
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.perform_undo(cx);
                                                    }))
                                                    .child("Undo"),
                                            )
                                        }),
                                )
                            })
                            .child(
                                div()
                                    .id("btn-budget")
                                    .px_2()
                                    .py_1()
                                    .rounded(px(6.0))
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(theme::text_secondary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.active_popover =
                                            if this.active_popover == ActivePopover::Budget {
                                                ActivePopover::None
                                            } else {
                                                ActivePopover::Budget
                                            };
                                        cx.notify();
                                    }))
                                    .child(format!(
                                        "{} / {}",
                                        self.snapshot.daily_budget.used,
                                        self.snapshot.daily_budget.limit
                                    )),
                            )
                            .child(
                                div()
                                    .id("btn-health-dot")
                                    .w(px(22.0))
                                    .h(px(22.0))
                                    .rounded(px(6.0))
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.active_popover =
                                            if this.active_popover == ActivePopover::Health {
                                                ActivePopover::None
                                            } else {
                                                ActivePopover::Health
                                            };
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .w(px(7.0))
                                            .h(px(7.0))
                                            .rounded(px(99.0))
                                            .bg(self.health_color()),
                                    ),
                            ),
                    ),
            )
    }

    fn render_right_drawer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.right_drawer {
            RightDrawer::Closed => None,
            RightDrawer::Role => {
                let role = self.selected_role.as_ref()?;
                let role_company = role.company.clone();
                let role_path = role.record_path.clone();
                let brief_opt = role.brief_path.clone();

                Some(
                    div()
                        .id("role-drawer")
                        .w(px(340.0))
                        .flex_shrink_0()
                        .h_full()
                        .p_4()
                        .bg(theme::panel_glass())
                        .border_l_1()
                        .border_color(theme::hairline())
                        .flex()
                        .flex_col()
                        .gap_3()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(theme::text_primary())
                                                .child(role.company.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child(role.title.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("close-drawer")
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.right_drawer = RightDrawer::Closed;
                                            cx.notify();
                                        }))
                                        .child("✕"),
                                ),
                        )
                        // Clean underline tabs (no box pills)
                        .child(
                            div()
                                .flex()
                                .gap_4()
                                .border_b_1()
                                .border_color(theme::hairline())
                                .children(
                                    [
                                        (DrawerTab::Overview, "Overview"),
                                        (DrawerTab::Brief, "Brief"),
                                        (DrawerTab::Packet, "Packet"),
                                        (DrawerTab::Notes, "Notes"),
                                    ]
                                    .into_iter()
                                    .map(|(tab, label)| {
                                        let on = self.drawer_tab == tab;
                                        div()
                                            .id(SharedString::from(format!("dtab-{label}")))
                                            .pb_2()
                                            .when(on, |d| {
                                                d.border_b_2().border_color(theme::text_primary())
                                            })
                                            .text_xs()
                                            .text_color(if on {
                                                theme::text_primary()
                                            } else {
                                                theme::text_muted()
                                            })
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.drawer_tab = tab;
                                                if tab == DrawerTab::Brief {
                                                    this.load_selected_brief_async(cx);
                                                } else if tab == DrawerTab::Notes
                                                    && let Some(r) = &this.selected_role
                                                {
                                                    let p = r.record_path.clone();
                                                    this.open_citation(&p, cx);
                                                }
                                                cx.notify();
                                            }))
                                            .child(label)
                                    }),
                                ),
                        )
                        // Metadata rows
                        .child(
                            div()
                                .py_2()
                                .border_b_1()
                                .border_color(theme::hairline())
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(kv_row("Stage", role.stage.clone()))
                                .child(kv_row("Checked", role.checked.clone()))
                                .when_some(role.scheduled_start.clone(), |d, s| {
                                    d.child(kv_row("Scheduled", s))
                                })
                                .when_some(role.cv_track.clone(), |d, tr| {
                                    d.child(kv_row("CV Track", tr))
                                })
                                .when_some(role.packet_hash.clone(), |d, h| {
                                    let short: String = h.chars().take(12).collect();
                                    d.child(kv_row("Packet SHA", short))
                                })
                                .when_some(role.source_url.clone(), |d, u| {
                                    let short: String = u.chars().take(32).collect();
                                    d.child(kv_row("Source", short))
                                })
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .text_xs()
                                        .child(div().text_color(theme::text_muted()).child("Record"))
                                        .child(
                                            div()
                                                .id("cite-record")
                                                .text_color(theme::lavender())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.open_citation(&role_path, cx);
                                                }))
                                                .child(
                                                    role.record_path
                                                        .rsplit('/')
                                                        .next()
                                                        .unwrap_or(&role.record_path)
                                                        .to_owned(),
                                                ),
                                        ),
                                )
                                .when_some(brief_opt, |d, bp| {
                                    let short = bp.rsplit('/').next().unwrap_or(&bp).to_owned();
                                    d.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .text_xs()
                                            .child(
                                                div().text_color(theme::text_muted()).child("Brief"),
                                            )
                                            .child(
                                                div()
                                                    .id("cite-brief")
                                                    .text_color(theme::lavender())
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.open_citation(&bp, cx);
                                                    }))
                                                    .child(short),
                                            ),
                                    )
                                }),
                        )
                        // Next action
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .child(if !role.urgency_reason.is_empty() {
                                    format!("{}\n\nNext: {}", role.urgency_reason, role.next_action)
                                } else {
                                    format!("Next: {}", role.next_action)
                                }),
                        )
                        // Citation preview
                        .when_some(self.citation_body.clone(), |d, body| {
                            d.child(
                                div()
                                    .pt_3()
                                    .border_t_1()
                                    .border_color(theme::hairline())
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::lavender())
                                            .child(
                                                self.citation_title
                                                    .clone()
                                                    .unwrap_or_else(|| "Citation".into()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::text_secondary())
                                            .child(body),
                                    ),
                            )
                        })
                        .child(
                            div()
                                .id("ask-role-chat")
                                .mt_auto()
                                .py_2()
                                .rounded(px(8.0))
                                .bg(theme::white_solid())
                                .text_xs()
                                .text_color(theme::ink_dark())
                                .flex()
                                .justify_center()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.chat_open {
                                        this.chat_anim_epoch = this.chat_anim_epoch.wrapping_add(1);
                                    }
                                    this.chat_open = true;
                                    this.active_input = ActiveInput::Chat;
                                    this.chat_input =
                                        format!("Brief me on {role_company} and next steps");
                                    cx.notify();
                                }))
                                .child("Ask in Chat about this role →"),
                        )
                        .into_any_element(),
                )
            }
            RightDrawer::Cvs => Some(
                div()
                    .id("cv-drawer")
                    .w(px(340.0))
                    .flex_shrink_0()
                    .h_full()
                    .p_4()
                    .bg(theme::panel_glass())
                    .border_l_1()
                    .border_color(theme::hairline())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .pb_2()
                            .border_b_1()
                            .border_color(theme::hairline())
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme::text_primary())
                                    .child("CVs & Tracks"),
                            )
                            .child(
                                div()
                                    .id("close-cv-drawer")
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.right_drawer = RightDrawer::Closed;
                                        cx.notify();
                                    }))
                                    .child("✕"),
                            ),
                    )
                    // Borderless Master -> Children Tree
                    .children(self.snapshot.cv_nodes.iter().enumerate().map(|(i, node)| {
                        let full = node.full_path.clone();
                        let full_reveal = node.full_path.clone();
                        let copy_str = node
                            .full_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| node.relative_path.clone());
                        let prefix = if node.is_master { "" } else { "└ " };
                        let sub = match node.matched_roles {
                            Some(n) => format!("{} · {} recent roles", node.relative_path, n),
                            None => node.relative_path.clone(),
                        };

                        div()
                            .pl(if node.is_master { px(0.0) } else { px(14.0) })
                            .py_2()
                            .border_b_1()
                            .border_color(theme::hairline())
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::text_primary())
                                            .child(format!("{prefix}{}", node.name)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(if node.exists {
                                                theme::emerald()
                                            } else {
                                                theme::amber_text()
                                            })
                                            .child(if node.exists { "Ready" } else { "Missing" }),
                                    ),
                            )
                            .child(div().text_xs().text_color(theme::text_muted()).child(sub))
                            .child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .pt_1()
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("cv-open-{i}")))
                                            .text_xs()
                                            .text_color(theme::lavender())
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(p) = &full {
                                                    open_in_system(p);
                                                } else {
                                                    this.set_toast("Remote PDF open unavailable", cx);
                                                }
                                            }))
                                            .child("Open"),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("cv-reveal-{i}")))
                                            .text_xs()
                                            .text_color(theme::text_secondary())
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(p) = &full_reveal {
                                                    reveal_in_file_manager(p);
                                                } else {
                                                    this.set_toast(
                                                        "Reveal requires local workspace",
                                                        cx,
                                                    );
                                                }
                                            }))
                                            .child("Reveal"),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("cv-copy-{i}")))
                                            .text_xs()
                                            .text_color(theme::text_secondary())
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy_str.clone(),
                                                ));
                                                this.set_toast("Copied CV path", cx);
                                            }))
                                            .child("Copy path"),
                                    ),
                            )
                    }))
                    .when(self.snapshot.suggest_new_track, |d| {
                        let sample = self.snapshot.unmatched_track_titles.join(", ");
                        d.child(
                            div()
                                .py_2()
                                .text_xs()
                                .text_color(theme::amber_text())
                                .child(format!(
                                    "Recent roles sit outside current tracks ({sample}). Ask Chat to draft a new track."
                                )),
                        )
                    })
                    .child(
                        div()
                            .id("import-primary-cv")
                            .mt_auto()
                            .py_2()
                            .rounded(px(8.0))
                            .bg(theme::white_solid())
                            .text_xs()
                            .text_color(theme::ink_dark())
                            .flex()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.import_cv_dialog(cx);
                            }))
                            .child("Import / Replace Master CV PDF…"),
                    )
                    .into_any_element(),
            ),
            RightDrawer::Tidy => {
                let quiet_roles: Vec<RoleCard> =
                    self.tidy_candidates().into_iter().cloned().collect();
                Some(
                    div()
                        .id("tidy-drawer")
                        .w(px(360.0))
                        .flex_shrink_0()
                        .h_full()
                        .p_4()
                        .bg(theme::panel_glass())
                        .border_l_1()
                        .border_color(theme::hairline())
                        .flex()
                        .flex_col()
                        .gap_2()
                        .overflow_y_scroll()
                        .child(
                            div()
                                .pb_2()
                                .border_b_1()
                                .border_color(theme::hairline())
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::text_primary())
                                        .child("Tidy & Archive Suggestions"),
                                )
                                .child(
                                    div()
                                        .id("close-tidy-drawer")
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.right_drawer = RightDrawer::Closed;
                                            cx.notify();
                                        }))
                                        .child("✕"),
                                ),
                        )
                        .when(self.snapshot.unresolved_attempts > 0, |d| {
                            let n = self.snapshot.unresolved_attempts;
                            d.child(
                                div()
                                    .py_2()
                                    .border_b_1()
                                    .border_color(theme::hairline())
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::amber_text())
                                            .child(format!("{n} unresolved submission guard(s)")),
                                    )
                                    .child(
                                        div()
                                            .id("tidy-reconcile")
                                            .text_xs()
                                            .text_color(theme::lavender())
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.submit_prompt(
                                                    "Reconcile unknown submission outcomes against Gmail Sent.".into(),
                                                    false,
                                                    cx,
                                                );
                                            }))
                                            .child("Reconcile →"),
                                    ),
                            )
                        })
                        .when(quiet_roles.is_empty(), |d| {
                            d.child(
                                div()
                                    .py_4()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child("Board is clean — no quiet roles waiting to archive or follow up."),
                            )
                        })
                        .children(quiet_roles.into_iter().enumerate().map(|(i, card)| {
                            let days = card.quiet_days.unwrap_or(0);
                            let card_archive = card.clone();
                            let company_follow = card.company.clone();
                            let path_keep = card.record_path.clone();
                            let card_open = card.clone();

                            div()
                                .py_2()
                                .border_b_1()
                                .border_color(theme::hairline())
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("tidy-open-{i}")))
                                                .text_xs()
                                                .text_color(theme::text_primary())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.open_role_drawer(
                                                        card_open.clone(),
                                                        DrawerTab::Overview,
                                                        cx,
                                                    );
                                                }))
                                                .child(card.company.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(if days >= 14 {
                                                    theme::rose_text()
                                                } else {
                                                    theme::amber_text()
                                                })
                                                .child(format!("Quiet {days}d · {}", card.stage)),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(card.title.clone()),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_3()
                                        .pt_1()
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("tidy-arch-{i}")))
                                                .text_xs()
                                                .text_color(theme::rose_text())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.archive_card_from_tidy(&card_archive, cx);
                                                }))
                                                .child("Archive"),
                                        )
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("tidy-fup-{i}")))
                                                .text_xs()
                                                .text_color(theme::lavender())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.submit_prompt(
                                                        format!(
                                                            "Draft a short follow-up email for {company_follow} (check Gmail Sent first)."
                                                        ),
                                                        false,
                                                        cx,
                                                    );
                                                }))
                                                .child("Follow up"),
                                        )
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("tidy-keep-{i}")))
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.dismissed_tidy.insert(path_keep.clone());
                                                    cx.notify();
                                                }))
                                                .child("Keep"),
                                        ),
                                )
                        }))
                        .into_any_element(),
                )
            }
        }
    }

    fn render_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.active_popover {
            ActivePopover::None => None,
            ActivePopover::CommandBar => {
                let entries = self.command_entries();
                let selected = self.command_index.min(entries.len().saturating_sub(1));
                let epoch = self.cmd_anim_epoch;
                Some(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .justify_center()
                        .pt(px(72.0))
                        .child(
                            div()
                                .id("cmd-bar")
                                .w(px(500.0))
                                .max_h(px(400.0))
                                .p_2()
                                .rounded(px(14.0))
                                .bg(theme::popover_glass())
                                .border_1()
                                .border_color(theme::hairline_strong())
                                .shadow_lg()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .border_b_1()
                                        .border_color(theme::hairline())
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(if self.command_query.is_empty() {
                                                    theme::text_muted()
                                                } else {
                                                    theme::text_primary()
                                                })
                                                .child(if self.command_query.is_empty() {
                                                    "Jump to a role, run an action, or ask Chat…"
                                                        .to_owned()
                                                } else {
                                                    self.command_query.clone()
                                                }),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child("Esc"),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("cmd-list")
                                        .flex_1()
                                        .overflow_y_scroll()
                                        .flex()
                                        .flex_col()
                                        .children(entries.into_iter().enumerate().map(
                                            |(i, entry)| {
                                                let active = i == selected;
                                                let action = entry.action;
                                                div()
                                                    .id(SharedString::from(format!("cmd-row-{i}")))
                                                    .px_3()
                                                    .py(px(7.0))
                                                    .rounded(px(7.0))
                                                    .when(active, |d| d.bg(theme::surface_active()))
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.execute_command(action.clone(), cx);
                                                    }))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(if active {
                                                                theme::text_primary()
                                                            } else {
                                                                theme::text_secondary()
                                                            })
                                                            .child(entry.label),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme::text_muted())
                                                            .child(entry.meta),
                                                    )
                                            },
                                        )),
                                )
                                .with_animation(
                                    SharedString::from(format!("cmd-anim-{epoch}")),
                                    Animation::new(Duration::from_millis(120))
                                        .with_easing(ease_out_quint()),
                                    |el, t| el.opacity(t),
                                ),
                        )
                        .into_any_element(),
                )
            }
            ActivePopover::Bell => Some(
                popover_shell(false, 370.0)
                    .child(popover_header("Attention", cx))
                    .when(self.snapshot.attention_items.is_empty(), |d| {
                        d.child(
                            div()
                                .py_3()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("Nothing urgent right now."),
                        )
                    })
                    // Sleek borderless list rows separated by subtle hairlines (zero box-in-box noise)
                    .children(
                        self.snapshot
                            .attention_items
                            .iter()
                            .take(7)
                            .enumerate()
                            .map(|(idx, item)| {
                                let is_urg = item.urgency == "urgent_now";
                                let path_opt = item.record_path.clone();
                                div()
                                    .id(SharedString::from(format!("att-{idx}")))
                                    .px_2()
                                    .py(px(8.0))
                                    .rounded(px(7.0))
                                    .border_b_1()
                                    .border_color(theme::hairline())
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.active_popover = ActivePopover::None;
                                        if let Some(p) = &path_opt {
                                            this.open_citation(p, cx);
                                        }
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .w(px(6.0))
                                                    .h(px(6.0))
                                                    .flex_shrink_0()
                                                    .rounded(px(99.0))
                                                    .bg(if is_urg {
                                                        theme::amber_accent()
                                                    } else {
                                                        theme::rose_text()
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .overflow_hidden()
                                                    .text_xs()
                                                    .text_color(if is_urg {
                                                        theme::amber_text()
                                                    } else {
                                                        theme::text_primary()
                                                    })
                                                    .child(item.company.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .child(item.detail.clone()),
                                    )
                            }),
                    )
                    .when(self.tidy_count() > 0, |d| {
                        d.child(
                            div()
                                .id("bell-open-tidy")
                                .pt_1()
                                .text_xs()
                                .text_color(theme::lavender())
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.open_right_drawer(RightDrawer::Tidy, cx);
                                }))
                                .child("Open Tidy & Archive suggestions →"),
                        )
                    })
                    .into_any_element(),
            ),
            ActivePopover::Budget => {
                let b = &self.snapshot.daily_budget;
                Some(
                    popover_shell(true, 330.0)
                        .child(popover_header("Daily Submission Cap", cx))
                        .child(stepper_row(
                            "Portals",
                            b.portal_used,
                            b.portal_limit,
                            "portal",
                            cx,
                        ))
                        .child(stepper_row(
                            "Direct email",
                            b.email_used,
                            b.email_limit,
                            "direct_email",
                            cx,
                        ))
                        .child(stepper_row(
                            "Outreach",
                            b.outreach_used,
                            b.outreach_limit,
                            "recruiter_outreach",
                            cx,
                        ))
                        .into_any_element(),
                )
            }
            ActivePopover::Health => Some(
                popover_shell(true, 330.0)
                    .child(popover_header("Workspace Status", cx))
                    .child(kv_row("Storage", self.snapshot.status_note.clone()))
                    .child(kv_row(
                        "Mode",
                        if self.client.target.is_remote() {
                            "Remote VPS".into()
                        } else {
                            "Local in-process".into()
                        },
                    ))
                    .child(kv_row("Chat runtime", self.runtime.label()))
                    .child(kv_row(
                        "Tracked roles",
                        format!(
                            "{} ({} upcoming)",
                            self.snapshot.total_records, self.snapshot.upcoming_count
                        ),
                    ))
                    .child(kv_row(
                        "Unknown guards",
                        self.snapshot.unresolved_attempts.to_string(),
                    ))
                    .into_any_element(),
            ),
            ActivePopover::Settings => {
                let p = &self.snapshot.permissions;
                let info = self.expanded_info;
                Some(
                    popover_shell(false, 360.0)
                        .child(popover_header("Permissions & Timing", cx))
                        .child(perm_row(
                            "Auto-apply portals",
                            p.auto_apply_portal,
                            "auto_apply_portal",
                            "Off = prepares packets for approval. On = submits eligible ATS portals inside your daily cap.",
                            info == Some("auto_apply_portal"),
                            cx,
                        ))
                        .child(perm_row(
                            "First-contact direct email",
                            p.auto_send_first_contact_email,
                            "auto_send_first_contact_email",
                            "Off = drafts in Gmail. On = sends initial cold applications after verifying zero prior Sent mail.",
                            info == Some("auto_send_first_contact_email"),
                            cx,
                        ))
                        .child(perm_row(
                            "Auto-reply threads",
                            p.auto_reply_threads,
                            "auto_reply_threads",
                            "Off = drafts replies in your voice. On = replies to routine threads (still re-checks live Sent first).",
                            info == Some("auto_reply_threads"),
                            cx,
                        ))
                        .child(perm_row(
                            "Auto-archive mail",
                            p.auto_archive_mail,
                            "auto_archive_mail",
                            "Archives verified receipts and rejections once recorded in your ledger.",
                            info == Some("auto_archive_mail"),
                            cx,
                        ))
                        .child(
                            div()
                                .pt_2()
                                .border_t_1()
                                .border_color(theme::hairline())
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(kv_row(
                                    "Prep reminder",
                                    format!("{} min prior", p.brief_lead_time_minutes),
                                ))
                                .child(kv_row("Agent", self.runtime.label()))
                                .child(
                                    div()
                                        .pt_1()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child("Interviews always check your live calendar and ask before committing a new time."),
                                ),
                        )
                        .into_any_element(),
                )
            }
        }
    }
}

impl Render for JobHarnessApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.focus(&this.focus_handle);
                }),
            )
            .size_full()
            .bg(theme::window_glass())
            .text_color(theme::text_primary())
            .flex()
            .flex_col()
            .relative()
            .overflow_hidden()
            .child(self.render_topbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .flex()
                    .overflow_hidden()
                    .when(self.chat_open, |d| {
                        let epoch = self.chat_anim_epoch;
                        d.child(
                            div()
                                .h_full()
                                .flex_shrink_0()
                                .overflow_hidden()
                                .child(self.render_chat_rail(cx))
                                .with_animation(
                                    SharedString::from(format!("chat-slide-{epoch}")),
                                    Animation::new(Duration::from_millis(170))
                                        .with_easing(ease_out_quint()),
                                    |el, t| el.w(px(300.0 * t)).opacity(t),
                                ),
                        )
                    })
                    .child(self.render_board(cx))
                    .when_some(self.render_right_drawer(cx), |d, drawer| {
                        let epoch = self.drawer_anim_epoch;
                        let target_w = if self.right_drawer == RightDrawer::Tidy {
                            360.0
                        } else {
                            340.0
                        };
                        d.child(
                            div()
                                .h_full()
                                .flex_shrink_0()
                                .overflow_hidden()
                                .child(drawer)
                                .with_animation(
                                    SharedString::from(format!("drawer-slide-{epoch}")),
                                    Animation::new(Duration::from_millis(170))
                                        .with_easing(ease_out_quint()),
                                    move |el, t| el.w(px(target_w * t)).opacity(t),
                                ),
                        )
                    }),
            )
            .when(self.loading, |d| {
                d.child(
                    div()
                        .absolute()
                        .bottom_3()
                        .right_4()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("Syncing…"),
                )
            })
            .when_some(self.render_popover(cx), |d, pop| d.child(pop))
    }
}

fn kv_row(label: &str, value: String) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .text_xs()
        .child(
            div()
                .text_color(theme::text_muted())
                .child(label.to_owned()),
        )
        .child(div().text_color(theme::text_primary()).child(value))
}

fn switch_pill(on: bool) -> impl IntoElement {
    div()
        .w(px(28.0))
        .h(px(15.0))
        .rounded(px(99.0))
        .bg(if on {
            theme::emerald()
        } else {
            theme::surface_active()
        })
        .flex()
        .items_center()
        .px(px(2.0))
        .justify_end()
        .when(!on, |d| d.justify_start())
        .child(
            div()
                .w(px(11.0))
                .h(px(11.0))
                .rounded(px(99.0))
                .bg(theme::white_solid()),
        )
}

fn popover_shell(bottom: bool, width: f32) -> gpui::Div {
    let base = div()
        .absolute()
        .right_4()
        .w(px(width))
        .p_3()
        .rounded(px(12.0))
        .bg(theme::popover_glass())
        .border_1()
        .border_color(theme::hairline_strong())
        .shadow_lg()
        .flex()
        .flex_col()
        .gap_2();
    if bottom {
        base.bottom(px(54.0))
    } else {
        base.top(px(48.0))
    }
}

fn popover_header(title: &str, cx: &mut Context<JobHarnessApp>) -> impl IntoElement {
    div()
        .pb_2()
        .border_b_1()
        .border_color(theme::hairline())
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_primary())
                .child(title.to_owned()),
        )
        .child(
            div()
                .id(SharedString::from(format!("close-pop-{title}")))
                .text_xs()
                .text_color(theme::text_muted())
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.active_popover = ActivePopover::None;
                    this.expanded_info = None;
                    cx.notify();
                }))
                .child("✕"),
        )
}

fn perm_row(
    label: &str,
    on: bool,
    key: &'static str,
    hint: &'static str,
    expanded: bool,
    cx: &mut Context<JobHarnessApp>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .py_1()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_primary())
                                .child(label.to_owned()),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("info-{key}")))
                                .px(px(4.0))
                                .rounded(px(4.0))
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::text_primary()))
                                .text_xs()
                                .text_color(if expanded {
                                    theme::lavender()
                                } else {
                                    theme::text_muted()
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.expanded_info = if this.expanded_info == Some(key) {
                                        None
                                    } else {
                                        Some(key)
                                    };
                                    cx.notify();
                                }))
                                .child("ⓘ"),
                        ),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("perm-{key}")))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_permission(key, cx);
                        }))
                        .child(switch_pill(on)),
                ),
        )
        .when(expanded, |d| {
            d.child(
                div()
                    .pb_1()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(hint),
            )
        })
}

fn stepper_row(
    label: &str,
    used: u64,
    limit: u64,
    channel: &'static str,
    cx: &mut Context<JobHarnessApp>,
) -> impl IntoElement {
    div()
        .py_1()
        .flex()
        .items_center()
        .justify_between()
        .text_xs()
        .child(
            div()
                .text_color(theme::text_primary())
                .child(format!("{label} ({used} used)")),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id(SharedString::from(format!("dec-{channel}")))
                        .w(px(20.0))
                        .h(px(20.0))
                        .rounded(px(5.0))
                        .bg(theme::surface_subtle())
                        .hover(|s| s.bg(theme::row_hover()))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.adjust_channel_cap(channel, -1, cx);
                        }))
                        .child("−"),
                )
                .child(
                    div()
                        .text_color(theme::text_primary())
                        .child(limit.to_string()),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("inc-{channel}")))
                        .w(px(20.0))
                        .h(px(20.0))
                        .rounded(px(5.0))
                        .bg(theme::surface_subtle())
                        .hover(|s| s.bg(theme::row_hover()))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.adjust_channel_cap(channel, 1, cx);
                        }))
                        .child("+"),
                ),
        )
}
