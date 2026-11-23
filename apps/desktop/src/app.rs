//! Native GPUI Desktop App for Job Harness.
//! Single-screen symmetrical layout:
//! - Left expandable Chat rail (`⌘\` or sidebar button)
//! - Center 4-column draggable Kanban Board with local instant search + bottom quick-ask dock
//! - Right edge slide-over drawer (tabbed Role Inspector + clickable citations, or CVs Master->Children tree)
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
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div, ease_out_quint,
    prelude::FluentBuilder, px,
};
use notify::RecommendedWatcher;
use serde_json::json;
use std::{
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
            .py_2()
            .rounded(px(9.0))
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
    Filters,
}

#[derive(Clone)]
enum CommandAction {
    OpenRole(Box<RoleCard>),
    ToggleChat,
    OpenCvs,
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
    _watcher: Option<RecommendedWatcher>,
    focus_handle: FocusHandle,

    // Layout & rails
    chat_open: bool,
    chat_scroll: ScrollHandle,
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
    chat_anim_epoch: u64,
    drawer_anim_epoch: u64,
    cmd_anim_epoch: u64,
    move_anim_epoch: u64,
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

    // Feedback toast + 5s Undo on drag
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
        let snapshot = client
            .load_snapshot()
            .unwrap_or_else(|err| WorkspaceSnapshot {
                healthy: false,
                status_note: err.to_string(),
                ..Default::default()
            });

        let (tx, rx) = mpsc::channel::<()>();
        let watcher = spawn_change_listener(target, tx);
        Self::start_watcher_loop(rx, cx);

        Self {
            client,
            runtime,
            snapshot,
            _watcher: watcher,
            focus_handle: cx.focus_handle(),
            chat_open: false,
            chat_scroll: ScrollHandle::new(),
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
            chat_anim_epoch: 1,
            drawer_anim_epoch: 1,
            cmd_anim_epoch: 1,
            move_anim_epoch: 1,
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
        }
    }

    fn start_watcher_loop(rx: Receiver<()>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(650))
                    .await;
                let mut changed = false;
                while rx.try_recv().is_ok() {
                    changed = true;
                }
                if changed {
                    let _ = this.update(cx, |app, cx| {
                        app.refresh_snapshot(cx);
                    });
                } else {
                    // Expire old toasts/undo banners quietly
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

    fn refresh_snapshot(&mut self, cx: &mut Context<Self>) {
        match self.client.load_snapshot() {
            Ok(snap) => {
                if let Some(sel) = &self.selected_role
                    && let Some(updated) = snap
                        .active_cards
                        .iter()
                        .chain(snap.archived_cards.iter())
                        .find(|c| c.record_path == sel.record_path)
                {
                    self.selected_role = Some(updated.clone());
                }
                self.snapshot = snap;
            }
            Err(err) => {
                self.snapshot.healthy = false;
                self.snapshot.status_note = err.to_string();
            }
        }
        cx.notify();
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
            self.load_selected_brief();
        }
        cx.notify();
    }

    fn load_selected_brief(&mut self) {
        let Some(role) = &self.selected_role else {
            return;
        };
        if let Some(brief) = &role.brief_path {
            match self.client.read_text_preview(brief) {
                Ok(text) => {
                    self.citation_title = Some(brief.clone());
                    self.citation_body = Some(text);
                }
                Err(e) => {
                    self.citation_title = Some(brief.clone());
                    self.citation_body = Some(format!("Unable to load brief: {e}"));
                }
            }
        } else {
            self.citation_title = Some("No brief yet".into());
            self.citation_body = Some(
                "No Layer 1/Layer 2 brief is linked yet. Ask Chat to generate a brief and 10-line cheatsheet.".into(),
            );
        }
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
            self.right_drawer = RightDrawer::Cvs;
            cx.notify();
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
            self.right_drawer = RightDrawer::Role;
        } else {
            self.right_drawer = RightDrawer::Role;
        }
        match self.client.read_text_preview(&normalized) {
            Ok(body) => {
                self.citation_title = Some(normalized);
                self.citation_body = Some(body);
            }
            Err(err) => {
                self.citation_title = Some(normalized);
                self.citation_body = Some(format!("Could not read citation: {err}"));
            }
        }
        cx.notify();
    }

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
        // Optimistic update on frame 1
        let prev_cards = self.snapshot.active_cards.clone();
        if let Some(card) = self
            .snapshot
            .active_cards
            .iter_mut()
            .find(|c| c.record_path == drag.record_path)
        {
            card.stage = target_stage.to_owned();
            card.column = target_column;
        }
        self.move_anim_epoch = self.move_anim_epoch.wrapping_add(1);
        self.last_moved_record = Some((drag.record_path.clone(), self.move_anim_epoch));
        cx.notify();

        match self.client.move_role_stage(
            &drag.record_path,
            &drag.from_stage,
            target_stage,
            &drag.next_action,
            drag.has_scheduled_start,
        ) {
            Ok(_) => {
                self.undo_move = Some(UndoMove {
                    record_path: drag.record_path.clone(),
                    company: drag.company.clone(),
                    previous_stage: drag.from_stage.clone(),
                    current_stage: target_stage.to_owned(),
                    next_action: drag.next_action.clone(),
                    has_scheduled_start: drag.has_scheduled_start,
                    created_at: Instant::now(),
                });
                self.set_toast(format!("Moved {} → {}", drag.company, target_column), cx);
                self.refresh_snapshot(cx);
            }
            Err(err) => {
                // Rollback cleanly on stale_record / guard denial
                self.snapshot.active_cards = prev_cards;
                self.last_moved_record = None;
                self.set_toast(format!("Reverted: {err}"), cx);
                self.refresh_snapshot(cx);
            }
        }
    }

    fn perform_undo(&mut self, cx: &mut Context<Self>) {
        let Some(undo) = self.undo_move.take() else {
            return;
        };
        match self.client.move_role_stage(
            &undo.record_path,
            &undo.current_stage,
            &undo.previous_stage,
            &undo.next_action,
            undo.has_scheduled_start,
        ) {
            Ok(_) => {
                self.move_anim_epoch = self.move_anim_epoch.wrapping_add(1);
                self.last_moved_record = Some((undo.record_path.clone(), self.move_anim_epoch));
                self.set_toast(format!("Undid move for {}", undo.company), cx);
                self.refresh_snapshot(cx);
            }
            Err(err) => self.set_toast(format!("Undo failed: {err}"), cx),
        }
    }

    fn open_command_bar(&mut self, cx: &mut Context<Self>) {
        self.command_query.clear();
        self.command_index = 0;
        self.active_popover = ActivePopover::CommandBar;
        self.active_input = ActiveInput::CommandBar;
        self.cmd_anim_epoch = self.cmd_anim_epoch.wrapping_add(1);
        cx.notify();
    }

    fn command_entries(&self) -> Vec<CommandEntry> {
        let q = self.command_query.trim().to_ascii_lowercase();
        let mut out = Vec::new();

        // 1. Instant role matches
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

        // 2. Deterministic quick actions
        let builtins = [
            ("Toggle Chat rail", "⌘\\", CommandAction::ToggleChat),
            ("Open CVs & Tracks", "⌘D", CommandAction::OpenCvs),
            ("Daily Submission Cap", "Split", CommandAction::OpenBudget),
            ("Permissions & Timing", "Rules", CommandAction::OpenSettings),
            (
                "Toggle Urgent & Overdue filter",
                "Filter",
                CommandAction::ToggleAttention,
            ),
            (
                "Toggle Archive column",
                "Filter",
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

        // 3. Natural-language fallback -> Chat
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
            CommandAction::ToggleChat => {
                self.chat_open = !self.chat_open;
                if self.chat_open {
                    self.chat_anim_epoch = self.chat_anim_epoch.wrapping_add(1);
                    self.active_input = ActiveInput::Chat;
                }
                cx.notify();
            }
            CommandAction::OpenCvs => {
                if self.right_drawer == RightDrawer::Closed {
                    self.drawer_anim_epoch = self.drawer_anim_epoch.wrapping_add(1);
                }
                self.right_drawer = RightDrawer::Cvs;
                cx.notify();
            }
            CommandAction::OpenBudget => {
                self.active_popover = ActivePopover::Budget;
                cx.notify();
            }
            CommandAction::OpenSettings => {
                self.active_popover = ActivePopover::Settings;
                cx.notify();
            }
            CommandAction::ToggleAttention => {
                self.only_attention = !self.only_attention;
                cx.notify();
            }
            CommandAction::ToggleArchive => {
                self.show_archive = !self.show_archive;
                cx.notify();
            }
            CommandAction::RefreshWorkspace => {
                self.refresh_snapshot(cx);
                self.set_toast("Workspace synced", cx);
            }
            CommandAction::ImportMasterCv => {
                if let Some(picked) = rfd::FileDialog::new()
                    .add_filter("PDF", &["pdf"])
                    .pick_file()
                {
                    match self.client.import_primary_cv(&picked) {
                        Ok(rel) => {
                            self.set_toast(format!("Imported primary CV: {rel}"), cx);
                            self.refresh_snapshot(cx);
                        }
                        Err(e) => self.set_toast(format!("Import failed: {e}"), cx),
                    }
                }
            }
            CommandAction::AskAgent(prompt) => {
                self.submit_prompt(prompt, false, cx);
            }
        }
    }

    fn adjust_channel_cap(&mut self, channel: &str, delta: i64, cx: &mut Context<Self>) {
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
        if let Err(err) = self.client.update_ledger_config(|cfg| {
            cfg["max_daily_submissions"] = json!(total);
            cfg["daily_channel_limits"] = json!({
                "portal": p,
                "direct_email": e,
                "recruiter_outreach": o
            });
        }) {
            self.set_toast(format!("Cap save failed: {err}"), cx);
            self.refresh_snapshot(cx);
            return;
        }
        cx.notify();
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
            "auto_book_calendar" => {
                p.auto_book_calendar = !p.auto_book_calendar;
                p.auto_book_calendar
            }
            "auto_archive_mail" => {
                p.auto_archive_mail = !p.auto_archive_mail;
                p.auto_archive_mail
            }
            _ => return,
        };
        if let Err(err) = self.client.update_ledger_config(|cfg| {
            if !cfg["permissions"].is_object() {
                cfg["permissions"] = json!({});
            }
            cfg["permissions"][key] = json!(new_val);
        }) {
            self.set_toast(format!("Save failed: {err}"), cx);
            self.refresh_snapshot(cx);
            return;
        }
        cx.notify();
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
                                    app.refresh_snapshot(cx);
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
            self.chat_open = !self.chat_open;
            if self.chat_open {
                self.chat_anim_epoch = self.chat_anim_epoch.wrapping_add(1);
                self.active_input = ActiveInput::Chat;
            }
            cx.notify();
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
            self.refresh_snapshot(cx);
            self.set_toast("Workspace synced", cx);
            return;
        }
        if cmd && key == "d" {
            if self.right_drawer == RightDrawer::Closed {
                self.drawer_anim_epoch = self.drawer_anim_epoch.wrapping_add(1);
            }
            self.right_drawer = if self.right_drawer == RightDrawer::Cvs {
                RightDrawer::Closed
            } else {
                RightDrawer::Cvs
            };
            cx.notify();
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
            theme::amber_text()
        } else {
            theme::emerald()
        }
    }

    fn render_topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let urgent = self.snapshot.urgent_count > 0;
        let chat_open = self.chat_open;
        let cvs_open = self.right_drawer == RightDrawer::Cvs;
        let ws_label = self.client.target.label();

        div()
            .h(px(44.0))
            .w_full()
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
                    // Clearance for macOS traffic lights
                    .pl(px(68.0))
                    .child(
                        div()
                            .id("toggle-chat-rail")
                            .px_2()
                            .py_1()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .when(chat_open, |d| d.bg(theme::surface_hover()))
                            .hover(|s| s.bg(theme::surface_hover()))
                            .text_xs()
                            .text_color(if chat_open {
                                theme::text_primary()
                            } else {
                                theme::text_secondary()
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.chat_open = !this.chat_open;
                                if this.chat_open {
                                    this.chat_anim_epoch = this.chat_anim_epoch.wrapping_add(1);
                                    this.active_input = ActiveInput::Chat;
                                }
                                cx.notify();
                            }))
                            .child("◫ Chat"),
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
                            .hover(|s| s.bg(theme::surface_hover()))
                            .text_xs()
                            .text_color(theme::text_muted())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_command_bar(cx);
                            }))
                            .child("⌘K"),
                    )
                    .child(
                        div()
                            .id("btn-cvs")
                            .px_2()
                            .py_1()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .when(cvs_open, |d| d.bg(theme::surface_hover()))
                            .hover(|s| s.bg(theme::surface_hover()))
                            .text_xs()
                            .text_color(if cvs_open {
                                theme::text_primary()
                            } else {
                                theme::text_secondary()
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_overlays();
                                if this.right_drawer == RightDrawer::Closed {
                                    this.drawer_anim_epoch = this.drawer_anim_epoch.wrapping_add(1);
                                }
                                this.right_drawer = if this.right_drawer == RightDrawer::Cvs {
                                    RightDrawer::Closed
                                } else {
                                    RightDrawer::Cvs
                                };
                                cx.notify();
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
                            .hover(|s| s.bg(theme::surface_hover()))
                            .text_xs()
                            .text_color(theme::text_secondary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_popover = if this.active_popover == ActivePopover::Bell
                                {
                                    ActivePopover::None
                                } else {
                                    ActivePopover::Bell
                                };
                                cx.notify();
                            }))
                            .child("○")
                            .when(urgent, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top(px(5.0))
                                        .right(px(5.0))
                                        .w(px(6.0))
                                        .h(px(6.0))
                                        .rounded(px(99.0))
                                        .bg(theme::amber_text()),
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
                            .hover(|s| s.bg(theme::surface_hover()))
                            .text_xs()
                            .text_color(theme::text_secondary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_popover =
                                    if this.active_popover == ActivePopover::Settings {
                                        ActivePopover::None
                                    } else {
                                        ActivePopover::Settings
                                    };
                                cx.notify();
                            }))
                            .child("⚙"),
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
            .flex()
            .flex_col()
            .bg(theme::panel_glass())
            .border_r_1()
            .border_color(theme::hairline())
            .overflow_hidden()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme::hairline())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_primary())
                            .child("Chat"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child(self.runtime.label()),
                            )
                            .when(!self.chat_messages.is_empty(), |d| {
                                d.child(
                                    div()
                                        .id("chat-jump-bottom")
                                        .px_1()
                                        .rounded(px(4.0))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::surface_hover()))
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.chat_scroll.scroll_to_bottom();
                                            cx.notify();
                                        }))
                                        .child("↓"),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .id("chat-scroll")
                    .track_scroll(&self.chat_scroll)
                    .flex_1()
                    .overflow_y_scroll()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(self.chat_messages.is_empty(), |d| {
                        d.child(
                            div()
                                .p_3()
                                .rounded(px(9.0))
                                .bg(theme::surface())
                                .border_1()
                                .border_color(theme::hairline())
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .child("Connected directly to your host agent session. Ask what needs your attention today, request a brief, or review reply drafts."),
                        )
                    })
                    .children(self.chat_messages.iter().enumerate().map(|(idx, msg)| {
                        let mut block = div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .p_2()
                                    .rounded(px(9.0))
                                    .bg(if msg.is_user {
                                        theme::surface_hover()
                                    } else {
                                        theme::surface()
                                    })
                                    .border_1()
                                    .border_color(theme::hairline())
                                    .text_xs()
                                    .text_color(theme::text_primary())
                                    .child(if msg.text.is_empty() && !msg.is_user {
                                        self.stream_status.clone()
                                    } else {
                                        msg.text.clone()
                                    }),
                            );
                        if !msg.citations.is_empty() {
                            let chips = div().flex().flex_wrap().gap_1().children(
                                msg.citations.iter().enumerate().map(|(ci, path)| {
                                    let p = path.clone();
                                    let short = path.rsplit('/').next().unwrap_or(path).to_owned();
                                    div()
                                        .id(SharedString::from(format!("cite-{idx}-{ci}")))
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded(px(99.0))
                                        .bg(theme::lavender_bg())
                                        .border_1()
                                        .border_color(theme::lavender_border())
                                        .text_xs()
                                        .text_color(theme::lavender())
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.open_citation(&p, cx);
                                        }))
                                        .child(short)
                                }),
                            );
                            block = block.child(chips);
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
                        .rounded(px(7.0))
                        .bg(theme::lavender_bg())
                        .border_1()
                        .border_color(theme::lavender_border())
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
                            .rounded(px(10.0))
                            .bg(theme::surface())
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
                                        .bg(theme::rose_bg())
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

    fn render_board(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cards = self.filtered_cards();
        let search_active = self.active_input == ActiveInput::Search;
        let quick_active = self.active_input == ActiveInput::QuickAsk;
        let columns: [&'static str; 4] = ["Saved", "Applied", "Screening", "Interviews"];

        div()
            .relative()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .px_4()
            .pt_3()
            .pb(px(64.0))
            .gap_3()
            // Instant local search row
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .id("search-box")
                            .w(px(270.0))
                            .px_3()
                            .py(px(6.0))
                            .rounded(px(8.0))
                            .bg(theme::surface())
                            .border_1()
                            .border_color(if search_active {
                                theme::hairline_strong()
                            } else {
                                theme::hairline()
                            })
                            .text_xs()
                            .text_color(if self.search_query.is_empty() {
                                theme::text_muted()
                            } else {
                                theme::text_primary()
                            })
                            .cursor_text()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_input = ActiveInput::Search;
                                cx.notify();
                            }))
                            .child(if self.search_query.is_empty() {
                                "Filter company, role, salary, track…  ⌘K".to_owned()
                            } else {
                                self.search_query.clone()
                            }),
                    )
                    .child(
                        div()
                            .id("btn-filters")
                            .px_2()
                            .py_1()
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::surface_hover()))
                            .text_xs()
                            .text_color(theme::text_muted())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_popover =
                                    if this.active_popover == ActivePopover::Filters {
                                        ActivePopover::None
                                    } else {
                                        ActivePopover::Filters
                                    };
                                cx.notify();
                            }))
                            .child(format!(
                                "Filters · Archive ({})",
                                self.snapshot.closed_count
                            )),
                    ),
            )
            // 4-column draggable Kanban
            .child(div().flex_1().flex().gap_4().overflow_hidden().children(
                columns.into_iter().map(|col_name| {
                    let col_cards: Vec<&RoleCard> = cards
                        .iter()
                        .copied()
                        .filter(|c| c.column == col_name)
                        .take(40)
                        .collect();
                    let col_total = cards.iter().filter(|c| c.column == col_name).count();

                    div()
                        .id(SharedString::from(format!("col-{col_name}")))
                        .flex_1()
                        .h_full()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .rounded(px(10.0))
                        .drag_over::<DragCard>(|style, _, _, _| {
                            style
                                .bg(theme::drop_highlight())
                                .border_t_2()
                                .border_color(theme::lavender())
                        })
                        .on_drop(cx.listener(move |this, drag: &DragCard, _, cx| {
                            this.move_card_optimistic(drag, col_name, cx);
                        }))
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
                                        .text_xs()
                                        .text_color(theme::text_secondary())
                                        .child(col_name),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(col_total.to_string()),
                                ),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("col-scroll-{col_name}")))
                                .flex_1()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .children(col_cards.into_iter().map(|card| {
                                    let c = card.clone();
                                    let drag_payload = DragCard {
                                        record_path: card.record_path.clone(),
                                        company: card.company.clone(),
                                        from_stage: card.stage.clone(),
                                        next_action: card.next_action.clone(),
                                        has_scheduled_start: card.scheduled_start.is_some(),
                                    };
                                    let is_urgent = card.urgency == "urgent_now";
                                    let is_overdue = card.urgency == "upcoming"
                                        && card.urgency_reason.contains("quiet for");
                                    let just_landed = self
                                        .last_moved_record
                                        .as_ref()
                                        .filter(|(p, _)| p == &card.record_path)
                                        .map(|(_, epoch)| *epoch);

                                    let mut el = div()
                                        .id(SharedString::from(card.record_path.clone()))
                                        .p(px(10.0))
                                        .rounded(px(9.0))
                                        .bg(if is_urgent {
                                            theme::amber_bg()
                                        } else if is_overdue {
                                            theme::rose_bg()
                                        } else {
                                            theme::surface()
                                        })
                                        .border_1()
                                        .border_color(if just_landed.is_some() {
                                            theme::lavender_border()
                                        } else if is_urgent {
                                            theme::amber_border()
                                        } else if is_overdue {
                                            theme::rose_border()
                                        } else {
                                            theme::hairline()
                                        })
                                        .hover(|s| {
                                            s.bg(theme::surface_hover())
                                                .border_color(theme::hairline_strong())
                                        })
                                        .cursor_pointer()
                                        .on_drag(drag_payload, |drag, _, _, cx| {
                                            cx.new(|_| DragGhost {
                                                company: drag.company.clone(),
                                            })
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.open_role_drawer(
                                                c.clone(),
                                                DrawerTab::Overview,
                                                cx,
                                            );
                                        }))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_primary())
                                                .child(card.company.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child(card.title.clone()),
                                        );
                                    if let Some(note) = &card.note_line {
                                        el = el.child(
                                            div()
                                                .mt_1()
                                                .text_xs()
                                                .text_color(if is_urgent {
                                                    theme::amber_text()
                                                } else if is_overdue {
                                                    theme::rose_text()
                                                } else {
                                                    theme::text_muted()
                                                })
                                                .child(note.clone()),
                                        );
                                    }
                                    if let Some(epoch) = just_landed {
                                        el.with_animation(
                                            SharedString::from(format!("land-{epoch}")),
                                            Animation::new(Duration::from_millis(240))
                                                .with_easing(ease_out_quint()),
                                            |card_el, t| card_el.opacity(0.65 + 0.35 * t),
                                        )
                                        .into_any_element()
                                    } else {
                                        el.into_any_element()
                                    }
                                })),
                        )
                }),
            ))
            // Quick-Ask upward reply popover
            .when(self.quick_popover_open, |d| {
                d.child(
                    div()
                        .absolute()
                        .left_4()
                        .bottom(px(58.0))
                        .w(px(380.0))
                        .p_3()
                        .rounded(px(12.0))
                        .bg(theme::popover_glass())
                        .border_1()
                        .border_color(theme::hairline_strong())
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
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("expand-to-chat")
                                                .text_xs()
                                                .text_color(theme::lavender())
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.quick_popover_open = false;
                                                    this.chat_open = true;
                                                    this.active_input = ActiveInput::Chat;
                                                    cx.notify();
                                                }))
                                                .child("⤢ Open in Chat"),
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
            // Bottom floating dock: Quick Ask + Actionable 0/25 + Subtle Health Dot
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
                            .px_4()
                            .py_2()
                            .rounded(px(99.0))
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
                            .when_some(self.toast.clone(), |d, (msg, _)| {
                                let has_undo = self.undo_move.is_some();
                                d.child(
                                    div()
                                        .px_3()
                                        .py(px(5.0))
                                        .rounded(px(99.0))
                                        .bg(theme::dock_glass())
                                        .border_1()
                                        .border_color(theme::hairline())
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
                                    .hover(|s| s.bg(theme::surface_hover()))
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
                                    .hover(|s| s.bg(theme::surface_hover()))
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
                        .w(px(330.0))
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
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
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
                        // Clean Codex-style tabs
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .children([
                                    (DrawerTab::Overview, "Overview"),
                                    (DrawerTab::Brief, "Brief"),
                                    (DrawerTab::Packet, "Packet"),
                                    (DrawerTab::Notes, "Notes"),
                                ].into_iter().map(|(tab, label)| {
                                    let on = self.drawer_tab == tab;
                                    div()
                                        .id(SharedString::from(format!("dtab-{label}")))
                                        .flex_1()
                                        .py_1()
                                        .rounded(px(6.0))
                                        .bg(if on { theme::surface_hover() } else { theme::surface() })
                                        .text_xs()
                                        .text_color(if on { theme::text_primary() } else { theme::text_muted() })
                                        .flex()
                                        .justify_center()
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.drawer_tab = tab;
                                            if tab == DrawerTab::Brief {
                                                this.load_selected_brief();
                                            } else if tab == DrawerTab::Notes
                                                && let Some(r) = &this.selected_role
                                            {
                                                let p = r.record_path.clone();
                                                this.open_citation(&p, cx);
                                            }
                                            cx.notify();
                                        }))
                                        .child(label)
                                })),
                        )
                        // Key-value metadata + clickable citations
                        .child(
                            div()
                                .py_2()
                                .border_t_1()
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
                                    let short: String = u.chars().take(30).collect();
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
                                            .child(div().text_color(theme::text_muted()).child("Brief"))
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
                        // Next action / urgency detail
                        .child(
                            div()
                                .p_3()
                                .rounded(px(9.0))
                                .bg(theme::surface())
                                .border_1()
                                .border_color(theme::hairline())
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .child(if !role.urgency_reason.is_empty() {
                                    format!("{}\n\nNext: {}", role.urgency_reason, role.next_action)
                                } else {
                                    format!("Next: {}", role.next_action)
                                }),
                        )
                        // Inline citation drilldown preview
                        .when_some(self.citation_body.clone(), |d, body| {
                            d.child(
                                div()
                                    .p_3()
                                    .rounded(px(9.0))
                                    .bg(theme::lavender_bg())
                                    .border_1()
                                    .border_color(theme::lavender_border())
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
                                            .text_color(theme::text_primary())
                                            .child(body),
                                    ),
                            )
                        })
                        // Ask Chat about this role
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
                    .w(px(330.0))
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
                    // Master -> Children Tree
                    .children(self.snapshot.cv_nodes.iter().enumerate().map(|(i, node)| {
                        let full = node.full_path.clone();
                        let full_reveal = node.full_path.clone();
                        let copy_str = node
                            .full_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| node.relative_path.clone());
                        let prefix = if node.is_master { "" } else { "└─ " };
                        let sub = match node.matched_roles {
                            Some(n) => format!("{} · {} recent roles", node.relative_path, n),
                            None => node.relative_path.clone(),
                        };

                        div()
                            .ml(if node.is_master { px(0.0) } else { px(12.0) })
                            .p_3()
                            .rounded(px(9.0))
                            .bg(theme::surface())
                            .border_1()
                            .border_color(theme::hairline())
                            .flex()
                            .flex_col()
                            .gap_2()
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
                                    .gap_2()
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("cv-open-{i}")))
                                            .px_2()
                                            .py(px(3.0))
                                            .rounded(px(6.0))
                                            .bg(theme::surface_hover())
                                            .text_xs()
                                            .text_color(theme::text_primary())
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
                                            .px_2()
                                            .py(px(3.0))
                                            .rounded(px(6.0))
                                            .bg(theme::surface_hover())
                                            .text_xs()
                                            .text_color(theme::text_secondary())
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(p) = &full_reveal {
                                                    reveal_in_file_manager(p);
                                                } else {
                                                    this.set_toast("Reveal requires local workspace", cx);
                                                }
                                            }))
                                            .child("Reveal"),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("cv-copy-{i}")))
                                            .px_2()
                                            .py(px(3.0))
                                            .rounded(px(6.0))
                                            .bg(theme::surface_hover())
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
                                .p_3()
                                .rounded(px(9.0))
                                .bg(theme::amber_bg())
                                .border_1()
                                .border_color(theme::amber_border())
                                .text_xs()
                                .text_color(theme::amber_text())
                                .child(format!(
                                    "Recent roles sit outside your current tracks ({sample}). Ask Chat to draft a new child track."
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
                                if let Some(picked) = rfd::FileDialog::new()
                                    .add_filter("PDF", &["pdf"])
                                    .pick_file()
                                {
                                    match this.client.import_primary_cv(&picked) {
                                        Ok(rel) => {
                                            this.set_toast(format!("Imported primary CV: {rel}"), cx);
                                            this.refresh_snapshot(cx);
                                        }
                                        Err(e) => this.set_toast(format!("Import failed: {e}"), cx),
                                    }
                                }
                            }))
                            .child("Import / Replace Master CV PDF…"),
                    )
                    .into_any_element(),
            ),
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
                                .w(px(460.0))
                                .max_h(px(380.0))
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
                                        .gap(px(2.0))
                                        .children(entries.into_iter().enumerate().map(
                                            |(i, entry)| {
                                                let active = i == selected;
                                                let action = entry.action;
                                                div()
                                                    .id(SharedString::from(format!("cmd-row-{i}")))
                                                    .px_3()
                                                    .py(px(7.0))
                                                    .rounded(px(8.0))
                                                    .when(active, |d| d.bg(theme::surface_hover()))
                                                    .hover(|s| s.bg(theme::surface_hover()))
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
                                    Animation::new(Duration::from_millis(140))
                                        .with_easing(ease_out_quint()),
                                    |el, t| el.opacity(t),
                                ),
                        )
                        .into_any_element(),
                )
            }
            ActivePopover::Bell => Some(
                popover_shell(false)
                    .child(popover_header("Attention", cx))
                    .when(self.snapshot.attention_items.is_empty(), |d| {
                        d.child(
                            div()
                                .p_3()
                                .rounded(px(8.0))
                                .bg(theme::surface())
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .child("Nothing urgent right now."),
                        )
                    })
                    .children(
                        self.snapshot
                            .attention_items
                            .iter()
                            .take(6)
                            .enumerate()
                            .map(|(idx, item)| {
                                let is_urg = item.urgency == "urgent_now";
                                let path_opt = item.record_path.clone();
                                div()
                                    .id(SharedString::from(format!("att-{idx}")))
                                    .p_2()
                                    .rounded(px(8.0))
                                    .bg(if is_urg {
                                        theme::amber_bg()
                                    } else {
                                        theme::surface()
                                    })
                                    .border_1()
                                    .border_color(if is_urg {
                                        theme::amber_border()
                                    } else {
                                        theme::hairline()
                                    })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.active_popover = ActivePopover::None;
                                        if let Some(p) = &path_opt {
                                            this.open_citation(p, cx);
                                        }
                                    }))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(if is_urg {
                                                theme::amber_text()
                                            } else {
                                                theme::text_primary()
                                            })
                                            .child(item.company.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .child(item.detail.clone()),
                                    )
                            }),
                    )
                    .into_any_element(),
            ),
            ActivePopover::Budget => {
                let b = &self.snapshot.daily_budget;
                Some(
                    popover_shell(true)
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
                popover_shell(true)
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
                    popover_shell(false)
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
                            "Off = saves draft in Gmail. On = sends initial cold applications after verifying zero prior Sent mail.",
                            info == Some("auto_send_first_contact_email"),
                            cx,
                        ))
                        .child(perm_row(
                            "Auto-reply threads",
                            p.auto_reply_threads,
                            "auto_reply_threads",
                            "Off = drafts replies in your voice. On = replies to routine recruiter threads (still checks live Sent first).",
                            info == Some("auto_reply_threads"),
                            cx,
                        ))
                        .child(perm_row(
                            "Auto-book calendar",
                            p.auto_book_calendar,
                            "auto_book_calendar",
                            "Always checks your calendar & asks before locking in a new time (offline plans aren't on calendars). On = auto-accepts confirmed invites for slots you already approved.",
                            info == Some("auto_book_calendar"),
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
                                .child(kv_row("Agent", self.runtime.label())),
                        )
                        .into_any_element(),
                )
            }
            ActivePopover::Filters => Some(
                popover_shell(false)
                    .child(popover_header("Filter Board", cx))
                    .child(
                        div()
                            .id("flt-archive")
                            .py_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(theme::text_primary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_archive = !this.show_archive;
                                cx.notify();
                            }))
                            .child(format!("Include Archive ({})", self.snapshot.closed_count))
                            .child(switch_pill(self.show_archive)),
                    )
                    .child(
                        div()
                            .id("flt-attention")
                            .py_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(theme::text_primary())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.only_attention = !this.only_attention;
                                cx.notify();
                            }))
                            .child("Urgent & overdue only")
                            .child(switch_pill(self.only_attention)),
                    )
                    .into_any_element(),
            ),
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
            .child(self.render_topbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(self.chat_open, |d| {
                        let epoch = self.chat_anim_epoch;
                        d.child(
                            div()
                                .h_full()
                                .overflow_hidden()
                                .child(self.render_chat_rail(cx))
                                .with_animation(
                                    SharedString::from(format!("chat-slide-{epoch}")),
                                    Animation::new(Duration::from_millis(180))
                                        .with_easing(ease_out_quint()),
                                    |el, t| el.w(px(300.0 * t)).opacity(t),
                                ),
                        )
                    })
                    .child(self.render_board(cx))
                    .when_some(self.render_right_drawer(cx), |d, drawer| {
                        let epoch = self.drawer_anim_epoch;
                        d.child(
                            div()
                                .h_full()
                                .overflow_hidden()
                                .child(drawer)
                                .with_animation(
                                    SharedString::from(format!("drawer-slide-{epoch}")),
                                    Animation::new(Duration::from_millis(180))
                                        .with_easing(ease_out_quint()),
                                    |el, t| el.w(px(330.0 * t)).opacity(t),
                                ),
                        )
                    }),
            )
            .when_some(self.render_popover(cx), |d, pop| d.child(pop))
    }
}

fn kv_row(label: &str, value: String) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
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
            theme::surface_hover()
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

fn popover_shell(bottom: bool) -> gpui::Div {
    let base = div()
        .absolute()
        .right_4()
        .w(px(290.0))
        .p_3()
        .rounded(px(12.0))
        .bg(theme::popover_glass())
        .border_1()
        .border_color(theme::hairline_strong())
        .flex()
        .flex_col()
        .gap_2();
    if bottom {
        base.bottom(px(58.0))
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
                                .px(px(3.0))
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
                        .bg(theme::surface_hover())
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
                        .bg(theme::surface_hover())
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
