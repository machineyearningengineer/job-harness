//! Native GPUI Desktop App for Job Harness.
//! Zero main-thread I/O: all Engine / SQLite / HTTP / disk calls execute off-thread so
//! every click, toggle, scroll, and drag runs smoothly at 120Hz.
use crate::{
    chat_bridge::{
        AgentOptions, AgentRuntime, CapabilityAccess, DiscoveredMcpServer, HostSession,
        StreamEvent, discover_mcp_servers, evaluate_capability, extract_citations, is_image_path,
    },
    markdown::{self, LinkTarget},
    sankey::{self, SankeyBucket},
    theme,
    transport::{
        RoleCard, RoleOverviewDetails, WorkspaceClient, WorkspaceSnapshot, WorkspaceTarget,
        column_to_stage, open_in_system, open_url, reveal_in_file_manager, spawn_change_listener,
    },
    ui_state::UiState,
};
use gpui::{
    Animation, AnimationExt, AnyElement, App, AppContext, ClipboardItem, Context, ExternalPaths,
    FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseMoveEvent, MouseUpEvent, ParentElement, Render, ScrollHandle, ScrollWheelEvent,
    SharedString, StatefulInteractiveElement, Styled, TouchPhase, Window, div, ease_out_quint, img,
    point, prelude::FluentBuilder, px, svg,
};
use job_harness_core::secrets::{self, SecretFieldSpec, SecretRequest, SecretStatus};
use notify::RecommendedWatcher;
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    rc::Rc,
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
            .rounded(theme::r_sm())
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
enum ColumnMode {
    Standard,
    ExpandInterviews,
    CollapseInterviews,
    ExpandArchive,
    CollapseArchive,
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
    SankeyFilter,
    SecretField,
    QuickAsk,
    Chat,
    CommandBar,
    ModelField,
    InlineConfig,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResizeRail {
    Chat,
    RightDrawer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivePopover {
    None,
    CommandBar,
    Agent,
    Sessions,
    Secret,
    SetupWizard,
    Tour,
    Budget,
    Health,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DrawerTab {
    Overview,
    Brief,
    Notes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RightDrawer {
    Closed,
    Role,
    Cvs,
    Profile,
    Tidy,
    Notifications,
    Settings,
}

#[derive(Clone)]
enum CommandAction {
    OpenRole(Box<RoleCard>),
    ToggleChat,
    OpenCvs,
    OpenProfile,
    OpenTidy,
    OpenBudget,
    OpenSettings,
    ToggleAttention,
    ToggleArchive,
    RefreshWorkspace,
    ImportMasterCv,
    PassSecret,
    CopyTranscript,
    ExportTranscript,
    OpenSetupWizard,
    StartTour,
    AskAgent(String),
}

struct ChatPickerItem {
    label: String,
    meta: String,
    insert_text: String,
    role_attach: Option<(String, String)>,
}

struct CommandEntry {
    label: String,
    meta: String,
    action: CommandAction,
}

#[derive(Clone, Debug)]
struct ToolStep {
    id: String,
    name: String,
    detail: String,
    output: String,
    expanded: bool,
    done: bool,
    ok: bool,
    duration_ms: Option<u64>,
}

#[derive(Clone, Debug)]
enum SyncIndicator {
    Idle,
    Working(String),
    Saved(String, Instant),
    Failed(String, Instant),
}

#[derive(Clone, Debug)]
struct ChatMessage {
    is_user: bool,
    text: String,
    /// Byte length shown so far (paced reveal, always on a char boundary).
    revealed: usize,
    sent_label: String,
    attachments: Vec<PathBuf>,
    citations: Vec<String>,
    steps: Vec<ToolStep>,
    thinking: String,
    started_at: Instant,
    duration_ms: Option<u64>,
    total_tokens: Option<u64>,
    steps_expanded: bool,
    is_error: bool,
    fast_mode: bool,
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
    available_runtimes: Vec<AgentRuntime>,
    mcp_servers: Vec<DiscoveredMcpServer>,
    host_sessions: Vec<HostSession>,
    resumed_session_label: Option<String>,
    show_connect_help: bool,
    show_mcp_servers: bool,
    tools_subview: bool,
    tools_subview_epoch: u64,
    desktop_notifications: bool,
    notified_keys: HashSet<String>,
    setup_step: usize,
    tour_step: usize,
    tour_prev_step: usize,
    tour_anim_epoch: u64,
    copied_flash: Option<(SharedString, u64)>,
    copied_flash_epoch: u64,
    agent_opts: AgentOptions,
    model_draft: String,
    last_seen_activity_at: Option<String>,
    last_saved_ui: Option<UiState>,
    snapshot: WorkspaceSnapshot,
    needs_first_run: bool,
    is_sample_workspace: bool,
    loading: bool,
    refresh_in_flight: bool,
    _watcher: Option<RecommendedWatcher>,
    focus_handle: FocusHandle,

    // Layout & rails
    chat_open: bool,
    chat_fullscreen: bool,
    chat_scroll: ScrollHandle,
    settings_scroll: ScrollHandle,
    board_scroll_x: ScrollHandle,
    col_scrolls: HashMap<&'static str, ScrollHandle>,
    scroll_axis_lock: Option<ScrollAxis>,
    last_scroll_at: Option<Instant>,
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
    archive_toggled: bool,
    archive_expanded: bool,
    archive_split_toggled: bool,
    archive_split_anim_epoch: u64,
    only_attention: bool,
    attention_toggled: bool,
    interviews_expanded: bool,
    interviews_toggled: bool,
    collapsed_cols: HashSet<&'static str>,
    dismissed_tidy: HashSet<String>,

    // Scoped animation epochs (only trigger when that exact surface opens)
    chat_anim_epoch: u64,
    drawer_anim_epoch: u64,
    drawer_tab_epoch: u64,
    brief_section_epoch: u64,
    cmd_anim_epoch: u64,
    move_anim_epoch: u64,
    interviews_anim_epoch: u64,
    attention_anim_epoch: u64,
    collapse_anim_epoch: u64,
    flow_view: bool,
    flow_anim_epoch: u64,
    sankey_zoom: Option<SankeyBucket>,
    sankey_expand_all: bool,
    sankey_all_time: bool,
    sankey_scale: f32,
    sankey_bucket: Option<SankeyBucket>,
    sankey_filter: String,
    sankey_hover: Option<SankeyBucket>,
    reduce_motion: bool,
    expanded_reader: Option<(String, String)>,
    reader_raw_json: bool,
    brief_section: Option<usize>,
    last_moved_record: Option<(String, u64)>,

    // Selected role & citation drilldown
    selected_role: Option<RoleCard>,
    role_details: Option<RoleOverviewDetails>,
    archive_anim_epoch: u64,
    citation_title: Option<String>,
    citation_body: Option<String>,

    // Chat & quick-ask (streaming + queue + cancel + attached role context chip)
    chat_messages: Vec<ChatMessage>,
    chat_input: String,
    chat_attachments: Vec<PathBuf>,
    pasted_snippets: Vec<String>,
    attached_role: Option<(String, String)>,
    quoted_reply: Option<String>,
    input_all_selected: bool,
    text_selection: Option<(markdown::ActiveSelection, Rc<Vec<String>>)>,
    selecting_doc: Option<SharedString>,
    popover_pinned: bool,
    popover_hover_trigger: Option<ActivePopover>,
    popover_hover_panel: bool,
    popover_hover_gen: u64,
    chat_width: f32,
    drawer_width: f32,
    resizing_rail: Option<(ResizeRail, f32, f32)>,
    chat_resized: bool,
    drawer_resized: bool,
    editing_config_key: Option<&'static str>,
    config_draft: String,
    config_error: Option<String>,
    scrub_config: Option<(&'static str, f32, i64, i64, bool)>,
    quick_input: String,
    quick_popover_open: bool,
    quick_reply_preview: String,
    queued_prompts: VecDeque<String>,
    streaming: bool,
    stream_status: String,
    stream_cancel: Option<Arc<AtomicBool>>,
    stream_turn_id: u64,
    chat_follow: bool,
    chat_ruler_hover: Option<usize>,
    chat_picker_idx: usize,

    // Feedback toast + animated bottom-dock sync/save pill + 5s Undo on drag/archive
    sync_state: SyncIndicator,
    sync_epoch: u64,
    toast: Option<(String, Instant)>,
    undo_move: Option<UndoMove>,

    // Ephemeral self-destructing secret handoff (in-memory only while typing, then 0600 temp files)
    active_secret_req: Option<SecretRequest>,
    secret_drafts: Vec<String>,
    secret_field_idx: usize,
    armed_secrets: Vec<SecretRequest>,
    seen_pending_secrets: HashSet<String>,
    known_ready_secrets: HashMap<String, String>,
}

impl Focusable for JobHarnessApp {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl JobHarnessApp {
    pub fn new(target: WorkspaceTarget, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let client = WorkspaceClient::new(target.clone());
        let saved = UiState::load();
        let available_runtimes = AgentRuntime::detect_all();
        let mcp_servers = discover_mcp_servers();
        let saved_runtime = saved
            .agent_kind
            .as_deref()
            .and_then(|k| available_runtimes.iter().find(|r| r.kind() == k).cloned());
        let mut agent_opts = if saved_runtime.is_some() {
            saved.agent.clone()
        } else {
            AgentOptions::default()
        };
        let runtime = saved_runtime.unwrap_or_else(|| {
            available_runtimes
                .first()
                .cloned()
                .unwrap_or(AgentRuntime::Unconfigured)
        });
        if let Some(cfg_fast) = runtime.read_configured_fast_mode() {
            agent_opts.fast_mode = cfg_fast;
        }
        // Start each desktop launch on a clean session so stale multi-hundred-K-token
        // threads never stall simple turns.
        agent_opts.session_name = Some(format!("career-gui-{}", chrono::Utc::now().timestamp()));
        let collapsed_cols: HashSet<&'static str> = ["Saved", "Applied", "Screening"]
            .into_iter()
            .filter(|c| saved.collapsed_cols.iter().any(|s| s == c))
            .collect();
        let (tx, rx) = mpsc::channel::<()>();
        let watcher = spawn_change_listener(target, tx);
        Self::start_watcher_loop(rx, cx);

        let missing_local_ws = client
            .target
            .local_root()
            .is_some_and(|r| !r.join("config/career-ledger.json").is_file());
        let is_sample_ws = client
            .target
            .local_root()
            .zip(crate::demo_seed::sample_workspace_dir().as_deref())
            .is_some_and(|(a, b)| a == b);

        let mut app = Self {
            client,
            runtime,
            available_runtimes,
            mcp_servers,
            host_sessions: Vec::new(),
            resumed_session_label: None,
            show_connect_help: false,
            show_mcp_servers: false,
            tools_subview: false,
            tools_subview_epoch: 1,
            desktop_notifications: saved.desktop_notifications,
            notified_keys: HashSet::new(),
            setup_step: 0,
            tour_step: 0,
            tour_prev_step: 0,
            tour_anim_epoch: 1,
            copied_flash: None,
            copied_flash_epoch: 0,
            agent_opts,
            model_draft: String::new(),
            last_seen_activity_at: saved.last_seen_activity_at.clone(),
            last_saved_ui: Some(saved.clone()),
            snapshot: WorkspaceSnapshot {
                healthy: true,
                status_note: if missing_local_ws {
                    "Choose or create a workspace".into()
                } else {
                    "Loading workspace…".into()
                },
                ..Default::default()
            },
            needs_first_run: missing_local_ws,
            is_sample_workspace: is_sample_ws,
            loading: !missing_local_ws,
            refresh_in_flight: false,
            _watcher: watcher,
            focus_handle: cx.focus_handle(),
            chat_open: saved.chat_open,
            chat_fullscreen: false,
            chat_scroll: ScrollHandle::new(),
            settings_scroll: ScrollHandle::new(),
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
            last_scroll_at: None,
            right_drawer: RightDrawer::Closed,
            drawer_tab: DrawerTab::Overview,
            active_popover: ActivePopover::None,
            expanded_info: None,
            active_input: ActiveInput::None,
            search_query: String::new(),
            command_query: String::new(),
            command_index: 0,
            show_archive: saved.show_archive,
            archive_toggled: false,
            archive_expanded: false,
            archive_split_toggled: false,
            archive_split_anim_epoch: 1,
            only_attention: saved.only_attention,
            attention_toggled: false,
            interviews_expanded: saved.interviews_expanded,
            interviews_toggled: false,
            collapsed_cols,
            dismissed_tidy: HashSet::new(),
            chat_anim_epoch: 1,
            drawer_anim_epoch: 1,
            drawer_tab_epoch: 1,
            brief_section_epoch: 1,
            cmd_anim_epoch: 1,
            move_anim_epoch: 1,
            interviews_anim_epoch: 1,
            attention_anim_epoch: 1,
            collapse_anim_epoch: 1,
            flow_view: false,
            flow_anim_epoch: 1,
            sankey_zoom: None,
            sankey_expand_all: false,
            sankey_all_time: false,
            sankey_scale: 1.0,
            sankey_bucket: None,
            sankey_filter: String::new(),
            sankey_hover: None,
            reduce_motion: saved.reduce_motion,
            expanded_reader: None,
            reader_raw_json: false,
            brief_section: None,
            last_moved_record: None,
            selected_role: None,
            role_details: None,
            archive_anim_epoch: 1,
            citation_title: None,
            citation_body: None,
            chat_messages: Vec::new(),
            chat_input: String::new(),
            chat_attachments: Vec::new(),
            pasted_snippets: Vec::new(),
            attached_role: None,
            quoted_reply: None,
            input_all_selected: false,
            text_selection: None,
            selecting_doc: None,
            popover_pinned: false,
            popover_hover_trigger: None,
            popover_hover_panel: false,
            popover_hover_gen: 0,
            chat_width: saved
                .chat_width
                .map(|w| (w as f32).clamp(260.0, 540.0))
                .unwrap_or(300.0),
            drawer_width: saved
                .drawer_width
                .map(|w| (w as f32).clamp(300.0, 620.0))
                .unwrap_or(360.0),
            resizing_rail: None,
            chat_resized: false,
            drawer_resized: false,
            editing_config_key: None,
            config_draft: String::new(),
            config_error: None,
            scrub_config: None,
            quick_input: String::new(),
            quick_popover_open: false,
            quick_reply_preview: String::new(),
            queued_prompts: VecDeque::new(),
            streaming: false,
            stream_status: String::new(),
            stream_cancel: None,
            stream_turn_id: 0,
            chat_follow: true,
            chat_ruler_hover: None,
            chat_picker_idx: 0,
            sync_state: SyncIndicator::Idle,
            sync_epoch: 1,
            toast: None,
            undo_move: None,
            active_secret_req: None,
            secret_drafts: Vec::new(),
            secret_field_idx: 0,
            armed_secrets: Vec::new(),
            seen_pending_secrets: HashSet::new(),
            known_ready_secrets: HashMap::new(),
        };
        crate::macos_notify::init();
        if !app.needs_first_run {
            app.refresh_snapshot_async(false, cx);
        }
        // Background ephemeral secret vault + macOS notification click watcher:
        // auto-opens Secret Modal on agent requests, updates live TTL countdown,
        // and opens the linked role's Brief when the user clicks a macOS notification.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(1200))
                    .await;
                let swept = cx
                    .background_executor()
                    .spawn(async move { secrets::sweep_expired().unwrap_or_default() })
                    .await;
                let clicked_rec = crate::macos_notify::take_clicked_record();
                let ok = this.update(cx, |app, cx| {
                    app.sync_secret_vault_state(swept, cx);
                    if let Some(rec_path) = clicked_rec
                        && let Some(card) = app
                            .snapshot
                            .active_cards
                            .iter()
                            .chain(app.snapshot.archived_cards.iter())
                            .find(|c| c.record_path == rec_path)
                            .cloned()
                    {
                        app.open_role_drawer(card, DrawerTab::Brief, cx);
                    }
                });
                if ok.is_err() {
                    break;
                }
            }
        })
        .detach();
        if let Ok(scene) = std::env::var("JOB_HARNESS_SHOWCASE_SCENE")
            && !scene.trim().is_empty()
        {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(750))
                    .await;
                let _ = this.update(cx, |app, cx| match scene.trim() {
                    "hero" => {
                        app.flow_view = false;
                        app.submit_prompt("What needs attention today?".into(), false, cx);
                    }
                    "brief" => {
                        app.flow_view = false;
                        app.submit_prompt("What needs attention today?".into(), false, cx);
                        if let Some(card) = app
                            .snapshot
                            .active_cards
                            .iter()
                            .find(|c| c.brief_path.is_some())
                            .cloned()
                        {
                            app.open_role_drawer(card, DrawerTab::Brief, cx);
                        }
                    }
                    "flow" => {
                        app.flow_view = true;
                        app.sankey_hover = Some(SankeyBucket::Interviews);
                        app.submit_prompt("What needs attention today?".into(), false, cx);
                        cx.notify();
                    }
                    "flow-drill" => {
                        app.flow_view = true;
                        app.sankey_bucket = Some(SankeyBucket::Screening);
                        app.submit_prompt("What needs attention today?".into(), false, cx);
                        cx.notify();
                    }
                    "guardrails" => {
                        app.flow_view = false;
                        app.open_right_drawer(RightDrawer::Settings, cx);
                    }
                    "command" => {
                        app.flow_view = false;
                        app.open_command_bar(cx);
                    }
                    _ => {}
                });
            })
            .detach();
        }
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
                        app.refresh_snapshot_async(false, cx);
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
                        if matches!(
                            app.sync_state,
                            SyncIndicator::Saved(_, t) | SyncIndicator::Failed(_, t)
                                if t.elapsed() > Duration::from_millis(2400)
                        ) {
                            app.sync_state = SyncIndicator::Idle;
                            dirty = true;
                        }
                        if dirty {
                            cx.notify();
                        }
                        // Remember window arrangement quietly (skip while dragging a rail)
                        let current = app.ui_state();
                        if app.resizing_rail.is_none()
                            && app.last_saved_ui.as_ref() != Some(&current)
                        {
                            app.last_saved_ui = Some(current.clone());
                            app.mark_sync_working("Saving…", cx);
                            cx.spawn(async move |this, cx| {
                                let res = cx
                                    .background_executor()
                                    .spawn(async move { current.save() })
                                    .await;
                                let _ = this.update(cx, |app, cx| match res {
                                    Ok(()) => app.mark_sync_saved("Saved", cx),
                                    Err(_) => app.mark_sync_failed("Save failed", cx),
                                });
                            })
                            .detach();
                        }
                    });
                }
            }
        })
        .detach();
    }

    fn ui_state(&self) -> UiState {
        let mut collapsed: Vec<String> = self
            .collapsed_cols
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
        collapsed.sort();
        UiState {
            reduce_motion: self.reduce_motion,
            desktop_notifications: self.desktop_notifications,
            show_archive: self.show_archive,
            only_attention: self.only_attention,
            interviews_expanded: self.interviews_expanded,
            collapsed_cols: collapsed,
            chat_open: self.chat_open,
            chat_width: Some(self.chat_width.round() as u32),
            drawer_width: Some(self.drawer_width.round() as u32),
            agent_kind: Some(self.runtime.kind().to_owned()),
            agent: self.agent_opts.clone(),
            last_seen_activity_at: self.last_seen_activity_at.clone(),
        }
    }

    fn mark_sync_working(&mut self, label: impl Into<String>, cx: &mut Context<Self>) {
        self.sync_state = SyncIndicator::Working(label.into());
        self.sync_epoch = self.sync_epoch.wrapping_add(1);
        cx.notify();
    }

    fn mark_sync_saved(&mut self, label: impl Into<String>, cx: &mut Context<Self>) {
        self.sync_state = SyncIndicator::Saved(label.into(), Instant::now());
        self.sync_epoch = self.sync_epoch.wrapping_add(1);
        let epoch = self.sync_epoch;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(2360))
                .await;
            let _ = this.update(cx, |app, cx| {
                if app.sync_epoch == epoch {
                    app.sync_state = SyncIndicator::Idle;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn mark_sync_failed(&mut self, label: impl Into<String>, cx: &mut Context<Self>) {
        self.sync_state = SyncIndicator::Failed(label.into(), Instant::now());
        self.sync_epoch = self.sync_epoch.wrapping_add(1);
        let epoch = self.sync_epoch;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(2600))
                .await;
            let _ = this.update(cx, |app, cx| {
                if app.sync_epoch == epoch {
                    app.sync_state = SyncIndicator::Idle;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn md_link_handler(&self, cx: &mut Context<Self>) -> markdown::LinkHandler {
        let weak = cx.entity().downgrade();
        Rc::new(move |target, _window, cx| match target {
            LinkTarget::Url(url) => open_url(&url),
            LinkTarget::WorkspacePath(path) => {
                let _ = weak.update(cx, |app, cx| {
                    app.open_citation(&path, cx);
                });
            }
        })
    }

    fn active_text_selection(&self) -> Option<markdown::ActiveSelection> {
        self.text_selection.as_ref().map(|(sel, _)| sel.clone())
    }

    fn selected_markdown_text(&self) -> Option<String> {
        let (sel, blocks) = self.text_selection.as_ref()?;
        if sel.is_empty() || blocks.is_empty() {
            return None;
        }
        let ((sb, si), (eb, ei)) = sel.ordered();
        let mut parts = Vec::new();
        for (bi, text) in blocks.iter().enumerate() {
            if bi < sb || bi > eb {
                continue;
            }
            let len = text.len();
            let lo = if bi == sb { si.min(len) } else { 0 };
            let hi = if bi == eb { ei.min(len) } else { len };
            if lo < hi && text.is_char_boundary(lo) && text.is_char_boundary(hi) {
                parts.push(&text[lo..hi]);
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    fn md_select_handler(&self, cx: &mut Context<Self>) -> markdown::SelectHandler {
        let weak = cx.entity().downgrade();
        Rc::new(move |ev, _window, cx| {
            let _ = weak.update(cx, |app, cx| match ev {
                markdown::TextSelectEvent::Start {
                    doc_id,
                    block_ix,
                    index,
                    doc_blocks,
                } => {
                    app.selecting_doc = Some(doc_id.clone());
                    app.text_selection = Some((
                        markdown::ActiveSelection {
                            doc_id,
                            anchor_block: block_ix,
                            anchor_index: index,
                            focus_block: block_ix,
                            focus_index: index,
                        },
                        doc_blocks,
                    ));
                    cx.notify();
                }
                markdown::TextSelectEvent::Move {
                    doc_id,
                    block_ix,
                    index,
                } => {
                    if app.selecting_doc.as_ref() == Some(&doc_id)
                        && let Some((sel, _)) = app.text_selection.as_mut()
                        && (sel.focus_block != block_ix || sel.focus_index != index)
                    {
                        sel.focus_block = block_ix;
                        sel.focus_index = index;
                        cx.notify();
                    }
                }
                markdown::TextSelectEvent::End => {
                    if app.selecting_doc.take().is_some() {
                        if app
                            .text_selection
                            .as_ref()
                            .is_some_and(|(sel, _)| sel.is_empty())
                        {
                            app.text_selection = None;
                        }
                        cx.notify();
                    }
                }
            });
        })
    }

    fn format_chat_transcript_markdown(&self) -> String {
        let date = chrono::Local::now().format("%Y-%m-%d %H:%M");
        let agent = self.runtime.short_name();
        let mut out = format!("# Job Harness Chat ({agent}, {date})\n");
        for msg in &self.chat_messages {
            let who = if msg.is_user { "You" } else { agent };
            out.push_str(&format!("\n## {who}"));
            if !msg.sent_label.is_empty() {
                out.push_str(&format!(" ({})", msg.sent_label));
            }
            out.push_str("\n\n");
            out.push_str(msg.text.trim());
            out.push('\n');
        }
        out
    }

    fn is_copied_flash(&self, tag: &str) -> bool {
        self.copied_flash
            .as_ref()
            .is_some_and(|(t, _)| t.as_ref() == tag)
    }

    fn flash_copied(&mut self, tag: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.copied_flash_epoch = self.copied_flash_epoch.wrapping_add(1);
        let epoch = self.copied_flash_epoch;
        self.copied_flash = Some((tag.into(), epoch));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1400))
                .await;
            let _ = this.update(cx, |app, cx| {
                if app.copied_flash.as_ref().is_some_and(|(_, e)| *e == epoch) {
                    app.copied_flash = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn copy_chat_transcript(&mut self, cx: &mut Context<Self>) {
        if self.chat_messages.is_empty() {
            self.set_toast("No chat messages to copy yet", cx);
            return;
        }
        let md = self.format_chat_transcript_markdown();
        let count = self.chat_messages.len();
        cx.write_to_clipboard(ClipboardItem::new_string(md));
        self.flash_copied("chat-copy-transcript", cx);
        self.set_toast(format!("Copied transcript ({count} messages)"), cx);
    }

    fn export_chat_transcript(&mut self, cx: &mut Context<Self>) {
        if self.chat_messages.is_empty() {
            self.set_toast("No chat messages to export yet", cx);
            return;
        }
        let md = self.format_chat_transcript_markdown();
        let default_name = format!(
            "job-harness-chat-{}.md",
            chrono::Local::now().format("%Y-%m-%d")
        );
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move {
                    rfd::FileDialog::new()
                        .set_file_name(&default_name)
                        .add_filter("Markdown", &["md"])
                        .save_file()
                })
                .await;
            let Some(path) = picked else {
                return;
            };
            let res = cx
                .background_executor()
                .spawn(async move { std::fs::write(&path, md).map(|()| path) })
                .await;
            let _ = this.update(cx, |app, cx| match res {
                Ok(p) => {
                    let name = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("transcript.md");
                    app.set_toast(format!("Exported {name}"), cx);
                }
                Err(err) => app.set_toast(format!("Export failed: {err}"), cx),
            });
        })
        .detach();
    }

    fn set_delegation_preset(&mut self, preset: &'static str, cx: &mut Context<Self>) {
        let (portal, cold_email, reply, archive) = match preset {
            "drafts" => (false, false, false, false),
            "assisted" => (true, false, false, true),
            "autonomous" => (true, true, true, true),
            _ => return,
        };
        let p = &mut self.snapshot.permissions;
        p.auto_apply_portal = portal;
        p.auto_send_first_contact_email = cold_email;
        p.auto_reply_threads = reply;
        p.auto_archive_mail = archive;
        self.mark_sync_working("Saving…", cx);

        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client.update_ledger_config(|cfg| {
                        if !cfg["permissions"].is_object() {
                            cfg["permissions"] = json!({});
                        }
                        cfg["permissions"]["auto_apply_portal"] = json!(portal);
                        cfg["permissions"]["auto_send_first_contact_email"] = json!(cold_email);
                        cfg["permissions"]["auto_reply_threads"] = json!(reply);
                        cfg["permissions"]["auto_archive_mail"] = json!(archive);
                    })
                })
                .await;
            let _ = this.update(cx, |app, cx| match res {
                Ok(()) => app.mark_sync_saved("Saved", cx),
                Err(err) => {
                    app.mark_sync_failed("Save failed", cx);
                    app.set_toast(format!("Save failed: {err}"), cx);
                    app.refresh_snapshot_async(false, cx);
                }
            });
        })
        .detach();
    }

    fn pick_chat_attachments(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move {
                    rfd::FileDialog::new()
                        .add_filter(
                            "Images & Documents",
                            &["png", "jpg", "jpeg", "webp", "gif", "pdf", "md", "txt"],
                        )
                        .pick_files()
                })
                .await;
            if let Some(files) = picked {
                let _ = this.update(cx, |app, cx| {
                    for p in files {
                        if !app.chat_attachments.contains(&p) {
                            app.chat_attachments.push(p);
                        }
                    }
                    app.active_input = ActiveInput::Chat;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn switch_workspace_to(
        &mut self,
        root: PathBuf,
        is_sample: bool,
        open_wizard: bool,
        cx: &mut Context<Self>,
    ) {
        let target = WorkspaceTarget::Local(root.clone());
        let (tx, rx) = mpsc::channel();
        self._watcher = spawn_change_listener(target.clone(), tx);
        Self::start_watcher_loop(rx, cx);
        self.client = WorkspaceClient::new(target);
        self.needs_first_run = false;
        self.is_sample_workspace = is_sample;
        self.selected_role = None;
        self.role_details = None;
        self.right_drawer = RightDrawer::Closed;
        self.sankey_zoom = None;
        self.sankey_bucket = None;
        self.loading = true;
        if open_wizard {
            self.setup_step = 0;
            self.active_popover = ActivePopover::SetupWizard;
            self.cmd_anim_epoch = self.cmd_anim_epoch.wrapping_add(1);
        }
        self.refresh_snapshot_async(true, cx);
    }

    fn launch_sample_workspace(&mut self, cx: &mut Context<Self>) {
        self.mark_sync_working("Preparing sample…", cx);
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { crate::demo_seed::ensure_sample_workspace() })
                .await;
            let _ = this.update(cx, |app, cx| match res {
                Ok(dir) => {
                    app.switch_workspace_to(dir, true, false, cx);
                    app.set_toast("Loaded 10-role sample workspace", cx);
                }
                Err(err) => {
                    app.mark_sync_failed("Sample failed", cx);
                    app.set_toast(format!("Sample workspace error: {err}"), cx);
                }
            });
        })
        .detach();
    }

    fn create_workspace_dialog(&mut self, cx: &mut Context<Self>) {
        let default_dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join("Career/workspace"));
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move {
                    let mut dlg = rfd::FileDialog::new()
                        .set_title("Choose empty folder for your new Job Harness workspace");
                    if let Some(parent) = default_dir.as_deref().and_then(|d| d.parent()) {
                        let _ = std::fs::create_dir_all(parent);
                        dlg = dlg.set_directory(parent);
                    }
                    dlg.pick_folder()
                })
                .await;
            let Some(folder) = picked else {
                return;
            };
            let target_dir = if folder.join("config/career-ledger.json").is_file() {
                folder
            } else if std::fs::read_dir(&folder)
                .ok()
                .and_then(|mut it| it.next())
                .is_some()
            {
                folder.join("job-harness-workspace")
            } else {
                folder
            };
            let init_dir = target_dir.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    if !init_dir.join("config/career-ledger.json").is_file() {
                        job_harness_core::workspace::initialize(&init_dir)
                            .map_err(|e| anyhow::anyhow!("{e}"))?;
                    }
                    crate::transport::save_default_workspace(&init_dir)?;
                    Ok::<PathBuf, anyhow::Error>(init_dir)
                })
                .await;
            let _ = this.update(cx, |app, cx| match res {
                Ok(dir) => {
                    app.switch_workspace_to(dir, false, true, cx);
                    app.set_toast("Workspace ready — finish setup below", cx);
                }
                Err(err) => {
                    app.set_toast(format!("Could not create workspace: {err}"), cx);
                }
            });
        })
        .detach();
    }

    fn open_workspace_dialog(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_executor()
                .spawn(async move {
                    rfd::FileDialog::new()
                        .set_title("Open existing Job Harness workspace")
                        .pick_folder()
                })
                .await;
            let Some(folder) = picked else {
                return;
            };
            if !folder.join("config/career-ledger.json").is_file() {
                let _ = this.update(cx, |app, cx| {
                    app.set_toast(
                        "That folder has no config/career-ledger.json — use Create workspace instead",
                        cx,
                    );
                });
                return;
            }
            let save_dir = folder.clone();
            let _ = cx
                .background_executor()
                .spawn(async move { crate::transport::save_default_workspace(&save_dir) })
                .await;
            let _ = this.update(cx, |app, cx| {
                app.switch_workspace_to(folder, false, false, cx);
                app.set_toast("Opened workspace", cx);
            });
        })
        .detach();
    }

    fn send_desktop_notification(
        &self,
        id: &str,
        title: &str,
        subtitle: &str,
        body: &str,
        record_path: Option<&str>,
        force: bool,
    ) {
        if !force && !self.desktop_notifications {
            return;
        }
        crate::macos_notify::send(id, title, subtitle, &truncate_end(body, 140), record_path);
    }

    /// Loads workspace snapshot strictly on the background executor (zero UI-thread stalls).
    /// Background/watcher reloads stay silent (`show_pill = false`); explicit user actions show the pill.
    fn refresh_snapshot_async(&mut self, show_pill: bool, cx: &mut Context<Self>) {
        if self.refresh_in_flight {
            return;
        }
        self.refresh_in_flight = true;
        if show_pill {
            self.mark_sync_working("Syncing…", cx);
        }
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
                        // Standardized native macOS desktop notification for upcoming interviews within prep lead time
                        let lead_mins = snap.permissions.brief_lead_time_minutes.max(5);
                        let now = chrono::Utc::now();
                        for card in &snap.active_cards {
                            if let Some(start) = card.scheduled_start.as_deref()
                                && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(start)
                            {
                                let mins = dt
                                    .with_timezone(&chrono::Utc)
                                    .signed_duration_since(now)
                                    .num_minutes();
                                if (0..=lead_mins).contains(&mins) {
                                    let key = format!("interview:{}:{}", card.record_path, start);
                                    if app.notified_keys.insert(key.clone()) {
                                        app.send_desktop_notification(
                                            &key,
                                            "Job Harness",
                                            &format!("Interview in {mins} min"),
                                            &format!("{} — {}", card.company, card.title),
                                            Some(&card.record_path),
                                            false,
                                        );
                                    }
                                }
                            }
                        }
                        app.snapshot = snap;
                        if show_pill {
                            app.mark_sync_saved("Synced", cx);
                        }
                    }
                    Err(err) => {
                        app.snapshot.healthy = false;
                        app.snapshot.status_note = err.to_string();
                        app.mark_sync_failed("Sync failed", cx);
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
        self.popover_pinned = false;
        self.popover_hover_trigger = None;
        self.popover_hover_panel = false;
        self.popover_hover_gen = self.popover_hover_gen.wrapping_add(1);
        self.expanded_info = None;
        self.expanded_reader = None;
        self.quick_popover_open = false;
    }

    /// Hover-intent state machine for bottom-dock popovers (`Budget` / `Health`):
    /// - Click (`popover_pinned = true`) always holds priority over hover-out.
    /// - First hover waits 180ms (prevents accidental triggers while crossing the dock).
    /// - Switching between sibling dock triggers while one is already open is 0ms instant.
    /// - Leaving a trigger or panel waits 240ms (safe hover bridge so you can interact inside the popover).
    fn on_dock_trigger_hover(
        &mut self,
        target: ActivePopover,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        self.popover_hover_gen = self.popover_hover_gen.wrapping_add(1);
        let seq = self.popover_hover_gen;
        if hovered {
            self.popover_hover_trigger = Some(target);
            // Click-pinned state owns priority unless user hovers the sibling trigger
            if self.active_popover == target {
                return;
            }
            let sibling_open = matches!(
                self.active_popover,
                ActivePopover::Budget | ActivePopover::Health
            );
            if sibling_open && !self.popover_pinned {
                // Instant 0ms handoff between sibling hover popovers
                self.active_popover = target;
                self.cmd_anim_epoch = self.cmd_anim_epoch.wrapping_add(1);
                cx.notify();
            } else if !self.popover_pinned && self.active_popover == ActivePopover::None {
                // 180ms hover-intent delay on cold entry
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(180))
                        .await;
                    let _ = this.update(cx, |app, cx| {
                        if app.popover_hover_gen == seq
                            && app.popover_hover_trigger == Some(target)
                            && !app.popover_pinned
                            && app.active_popover == ActivePopover::None
                        {
                            app.active_popover = target;
                            app.cmd_anim_epoch = app.cmd_anim_epoch.wrapping_add(1);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        } else {
            if self.popover_hover_trigger == Some(target) {
                self.popover_hover_trigger = None;
            }
            if !self.popover_pinned {
                self.schedule_dock_hover_close(seq, cx);
            }
        }
    }

    fn on_dock_panel_hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.popover_hover_gen = self.popover_hover_gen.wrapping_add(1);
        let seq = self.popover_hover_gen;
        self.popover_hover_panel = hovered;
        if !hovered && !self.popover_pinned {
            self.schedule_dock_hover_close(seq, cx);
        }
    }

    fn schedule_dock_hover_close(&self, seq: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(240))
                .await;
            let _ = this.update(cx, |app, cx| {
                if app.popover_hover_gen == seq
                    && !app.popover_pinned
                    && !app.popover_hover_panel
                    && app.popover_hover_trigger.is_none()
                    && matches!(
                        app.active_popover,
                        ActivePopover::Budget | ActivePopover::Health
                    )
                {
                    app.active_popover = ActivePopover::None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn toggle_dock_popover_pin(&mut self, target: ActivePopover, cx: &mut Context<Self>) {
        self.popover_hover_gen = self.popover_hover_gen.wrapping_add(1);
        if self.active_popover == target && self.popover_pinned {
            self.active_popover = ActivePopover::None;
            self.popover_pinned = false;
        } else {
            if self.active_popover != target {
                self.cmd_anim_epoch = self.cmd_anim_epoch.wrapping_add(1);
            }
            self.active_popover = target;
            self.popover_pinned = true;
        }
        cx.notify();
    }

    fn toggle_chat_rail(&mut self, cx: &mut Context<Self>) {
        self.chat_open = !self.chat_open;
        if !self.chat_open {
            self.chat_fullscreen = false;
        }
        if self.chat_open {
            self.chat_resized = false;
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
                self.drawer_resized = false;
                self.drawer_anim_epoch = self.drawer_anim_epoch.wrapping_add(1);
            }
            if kind == RightDrawer::Notifications
                && let Some(latest) = &self.snapshot.latest_activity_at
            {
                self.last_seen_activity_at = Some(latest.clone());
            }
            self.right_drawer = kind;
        }
        cx.notify();
    }

    fn open_role_drawer(&mut self, card: RoleCard, tab: DrawerTab, cx: &mut Context<Self>) {
        self.close_overlays();
        if self.right_drawer == RightDrawer::Closed {
            self.drawer_resized = false;
            self.drawer_anim_epoch = self.drawer_anim_epoch.wrapping_add(1);
        }
        let rec_path = card.record_path.clone();
        self.selected_role = Some(card);
        self.role_details = None;
        self.drawer_tab = tab;
        self.right_drawer = RightDrawer::Role;
        self.citation_title = None;
        self.citation_body = None;
        self.brief_section = None;
        self.load_role_details_async(rec_path, cx);
        if tab == DrawerTab::Brief {
            self.load_selected_brief_async(cx);
        }
        cx.notify();
    }

    fn load_role_details_async(&mut self, record_path: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let expected_path = record_path.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { client.load_role_details(&record_path) })
                .await;
            let _ = this.update(cx, |app, cx| {
                if app
                    .selected_role
                    .as_ref()
                    .is_some_and(|r| r.record_path == expected_path)
                    && let Ok(details) = res
                {
                    if app.drawer_tab == DrawerTab::Notes {
                        app.citation_title = Some(expected_path);
                        app.citation_body = Some(details.full_text.clone());
                    }
                    app.role_details = Some(details);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn switch_role_tab(&mut self, tab: DrawerTab, cx: &mut Context<Self>) {
        self.drawer_tab = tab;
        self.drawer_tab_epoch = self.drawer_tab_epoch.wrapping_add(1);
        if tab == DrawerTab::Brief {
            self.load_selected_brief_async(cx);
        } else if tab == DrawerTab::Notes
            && let Some(r) = &self.selected_role
        {
            let p = r.record_path.clone();
            self.citation_title = Some(p.clone());
            if let Some(d) = &self.role_details {
                self.citation_body = Some(d.full_text.clone());
            } else {
                self.citation_body = Some("Loading…".into());
                self.read_citation_async(p, cx);
            }
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

    fn open_file_in_reader_async(&mut self, rel_path: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let title = rel_path.clone();
        self.brief_section = None;
        self.expanded_reader = Some((title.clone(), "Loading…".into()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { client.read_text_preview(&rel_path) })
                .await;
            let _ = this.update(cx, |app, cx| {
                if app
                    .expanded_reader
                    .as_ref()
                    .is_some_and(|(t, _)| t == &title)
                {
                    app.expanded_reader = Some((
                        title,
                        match res {
                            Ok(body) => body,
                            Err(err) => format!("Could not read file: {err}"),
                        },
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
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
                app.brief_section = None;
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
        if self.chat_fullscreen {
            self.chat_fullscreen = false;
        }
        if normalized.contains("/briefs/") || normalized.starts_with("briefs/") {
            self.drawer_tab = DrawerTab::Brief;
        } else if normalized.starts_with("applications/") {
            self.drawer_tab = DrawerTab::Notes;
        }
        if self.right_drawer == RightDrawer::Closed {
            self.drawer_resized = false;
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
        self.mark_sync_working("Saving move…", cx);
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
                    app.mark_sync_saved("Saved", cx);
                    app.refresh_snapshot_async(false, cx);
                    cx.notify();
                }
                Err(err) => {
                    app.snapshot.active_cards = prev_active;
                    app.snapshot.archived_cards = prev_archived;
                    app.snapshot.closed_count = app.snapshot.archived_cards.len();
                    app.last_moved_record = None;
                    app.mark_sync_failed("Move failed", cx);
                    app.set_toast(format!("Reverted: {err}"), cx);
                    app.refresh_snapshot_async(false, cx);
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
                    app.mark_sync_saved("Undone", cx);
                    app.set_toast(format!("Undid move for {}", undo.company), cx);
                    app.refresh_snapshot_async(false, cx);
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
        self.mark_sync_working("Saving caps…", cx);

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
            let _ = this.update(cx, |app, cx| match res {
                Ok(()) => app.mark_sync_saved("Saved", cx),
                Err(err) => {
                    app.mark_sync_failed("Cap save failed", cx);
                    app.set_toast(format!("Cap save failed: {err}"), cx);
                    app.refresh_snapshot_async(false, cx);
                }
            });
        })
        .detach();
    }

    fn start_inline_config(&mut self, key: &'static str, initial: String, cx: &mut Context<Self>) {
        self.editing_config_key = Some(key);
        self.config_draft = initial;
        self.config_error = None;
        self.active_input = ActiveInput::InlineConfig;
        self.input_all_selected = true;
        cx.notify();
    }

    fn cancel_inline_config(&mut self, cx: &mut Context<Self>) {
        self.editing_config_key = None;
        self.config_draft.clear();
        self.config_error = None;
        self.input_all_selected = false;
        if self.active_input == ActiveInput::InlineConfig {
            self.active_input = ActiveInput::None;
        }
        cx.notify();
    }

    fn validate_inline_config(key: &'static str, raw: &str) -> Result<(), String> {
        let t = raw.trim();
        match key {
            "prep_reminder_minutes" => match t.parse::<u64>() {
                Ok(n) if (5..=1440).contains(&n) => Ok(()),
                _ => Err("Enter minutes between 5 and 1440".into()),
            },
            "applied_ghost_days" => match t.parse::<u64>() {
                Ok(n) if (1..=365).contains(&n) => Ok(()),
                _ => Err("Enter days between 1 and 365".into()),
            },
            "min_base_salary" | "target_base_salary" => {
                let digits: String = t
                    .chars()
                    .filter(|c| *c != ',' && *c != '£' && *c != '$' && *c != ' ')
                    .collect();
                match digits.parse::<u64>() {
                    Ok(_) => Ok(()),
                    Err(_) => Err("Enter a whole number ≥ 0".into()),
                }
            }
            "sender_email" | "application_email" => {
                if t.is_empty() || (t.contains('@') && t.contains('.') && !t.contains(' ')) {
                    Ok(())
                } else {
                    Err("Enter a valid email address (or leave blank to inherit default)".into())
                }
            }
            "avoid_list" | "notice_period" | "discovery_sources" => Ok(()),
            "timezone" => {
                if !t.is_empty() && !t.contains(' ') {
                    Ok(())
                } else {
                    Err("Use an IANA timezone, e.g. Europe/London".into())
                }
            }
            _ => {
                if t.is_empty() {
                    Err("Value cannot be empty".into())
                } else {
                    Ok(())
                }
            }
        }
    }

    fn commit_inline_config(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.editing_config_key else {
            return;
        };
        let raw = self.config_draft.trim().to_owned();
        if let Err(msg) = Self::validate_inline_config(key, &raw) {
            self.config_error = Some(msg);
            cx.notify();
            return;
        }
        let clean_num = |s: &str| -> u64 {
            s.chars()
                .filter(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .unwrap_or(0)
        };
        let split_csv = |s: &str| -> Vec<String> {
            s.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_owned)
                .collect()
        };

        // Optimistic UI update immediately
        match key {
            "prep_reminder_minutes" => {
                let v = clean_num(&raw) as i64;
                self.snapshot.permissions.brief_lead_time_minutes = v;
            }
            "applied_ghost_days" => {
                self.snapshot.profile_settings.applied_ghost_days = clean_num(&raw);
            }
            "application_email" => {
                self.snapshot.profile_settings.application_email = raw.clone();
            }
            "sender_email" => self.snapshot.profile_settings.sender_email = raw.clone(),
            "timezone" => self.snapshot.profile_settings.timezone = raw.clone(),
            "min_base_salary" => {
                self.snapshot.profile_settings.min_base_salary = clean_num(&raw);
            }
            "target_base_salary" => {
                self.snapshot.profile_settings.target_base_salary = clean_num(&raw);
            }
            "location_preferences" => {
                self.snapshot.profile_settings.location_preferences = split_csv(&raw).join(", ");
            }
            "notice_period" => self.snapshot.profile_settings.notice_period = raw.clone(),
            "target_roles" => {
                self.snapshot.profile_settings.target_roles = split_csv(&raw).join(", ");
            }
            "discovery_sources" => {
                self.snapshot.profile_settings.discovery_sources = split_csv(&raw).join(", ");
            }
            "avoid_list" => {
                self.snapshot.profile_settings.avoid_list = split_csv(&raw).join(", ");
            }
            "tone" => self.snapshot.profile_settings.tone = raw.clone(),
            _ => {}
        }

        self.editing_config_key = None;
        self.config_draft.clear();
        self.config_error = None;
        self.input_all_selected = false;
        self.active_input = ActiveInput::None;
        self.mark_sync_working("Saving…", cx);

        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client.update_ledger_config(|cfg| match key {
                        "prep_reminder_minutes" => {
                            let v = clean_num(&raw);
                            if !cfg["time_and_reminders"].is_object() {
                                cfg["time_and_reminders"] = json!({});
                            }
                            cfg["time_and_reminders"]["pre_interview_reminder_minutes"] = json!(v);
                            cfg["time_and_reminders"]["brief_lead_time_minutes"] = json!(v);
                        }
                        "applied_ghost_days" => {
                            let v = clean_num(&raw);
                            if !cfg["time_and_reminders"].is_object() {
                                cfg["time_and_reminders"] = json!({});
                            }
                            cfg["time_and_reminders"]["applied_ghost_days"] = json!(v);
                        }
                        "application_email" => {
                            if !cfg["communication"].is_object() {
                                cfg["communication"] = json!({});
                            }
                            cfg["communication"]["application_email"] = if raw.is_empty() {
                                serde_json::Value::Null
                            } else {
                                json!(raw)
                            };
                        }
                        "sender_email" => {
                            if !cfg["communication"].is_object() {
                                cfg["communication"] = json!({});
                            }
                            cfg["communication"]["sender_email"] = if raw.is_empty() {
                                serde_json::Value::Null
                            } else {
                                json!(raw)
                            };
                        }
                        "timezone" => {
                            cfg["timezone"] = json!(raw);
                        }
                        "min_base_salary" => {
                            let v = clean_num(&raw);
                            if !cfg["discovery"].is_object() {
                                cfg["discovery"] = json!({});
                            }
                            cfg["discovery"]["min_base_salary"] = json!(v);
                        }
                        "target_base_salary" => {
                            let v = clean_num(&raw);
                            if !cfg["discovery"].is_object() {
                                cfg["discovery"] = json!({});
                            }
                            cfg["discovery"]["target_base_salary"] = json!(v);
                        }
                        "location_preferences" => {
                            if !cfg["discovery"].is_object() {
                                cfg["discovery"] = json!({});
                            }
                            cfg["discovery"]["location_preferences"] = json!(split_csv(&raw));
                        }
                        "notice_period" => {
                            if !cfg["discovery"].is_object() {
                                cfg["discovery"] = json!({});
                            }
                            cfg["discovery"]["notice_period"] = if raw.is_empty() {
                                serde_json::Value::Null
                            } else {
                                json!(raw)
                            };
                        }
                        "target_roles" => {
                            cfg["target_roles"] = json!(split_csv(&raw));
                        }
                        "discovery_sources" => {
                            let items = split_csv(&raw);
                            if !cfg["discovery"].is_object() {
                                cfg["discovery"] = json!({});
                            }
                            if items.is_empty() {
                                if let Some(obj) = cfg["discovery"].as_object_mut() {
                                    obj.remove("sources");
                                }
                            } else {
                                cfg["discovery"]["sources"] = json!(items);
                            }
                        }
                        "avoid_list" => {
                            if !cfg["discovery"].is_object() {
                                cfg["discovery"] = json!({});
                            }
                            cfg["discovery"]["avoid_list"] = json!(split_csv(&raw));
                        }
                        "tone" => {
                            if !cfg["communication"].is_object() {
                                cfg["communication"] = json!({});
                            }
                            cfg["communication"]["tone"] = if raw.is_empty() {
                                serde_json::Value::Null
                            } else {
                                json!(raw)
                            };
                        }
                        _ => {}
                    })
                })
                .await;
            let _ = this.update(cx, |app, cx| match res {
                Ok(()) => app.mark_sync_saved("Saved", cx),
                Err(err) => {
                    app.mark_sync_failed("Save failed", cx);
                    app.set_toast(format!("Save failed: {err}"), cx);
                    app.refresh_snapshot_async(false, cx);
                }
            });
        })
        .detach();
    }

    fn set_communication_enum(
        &mut self,
        field: &'static str,
        value: &'static str,
        toast: &'static str,
        cx: &mut Context<Self>,
    ) {
        match field {
            "cover_letter_mode" => {
                self.snapshot.profile_settings.cover_letter_mode = value.to_owned();
            }
            "post_rejection_reply" => {
                self.snapshot.profile_settings.post_rejection_reply = value.to_owned();
            }
            _ => {}
        }
        self.mark_sync_working("Saving…", cx);
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client.update_ledger_config(|cfg| {
                        if !cfg["communication"].is_object() {
                            cfg["communication"] = json!({});
                        }
                        cfg["communication"][field] = json!(value);
                    })
                })
                .await;
            let _ = this.update(cx, |app, cx| match res {
                Ok(()) => {
                    app.mark_sync_saved("Saved", cx);
                    app.set_toast(toast, cx);
                }
                Err(err) => {
                    app.mark_sync_failed("Save failed", cx);
                    app.set_toast(format!("Save failed: {err}"), cx);
                    app.refresh_snapshot_async(false, cx);
                }
            });
        })
        .detach();
    }

    fn open_or_create_cover_letter_template(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.client.target.local_root().map(|p| p.to_path_buf()) else {
            self.set_toast("Template editing requires a local workspace", cx);
            return;
        };
        let rel = if self
            .snapshot
            .profile_settings
            .cover_letter_template
            .trim()
            .is_empty()
        {
            "context/profile/cover-letter-template.md".to_owned()
        } else {
            self.snapshot
                .profile_settings
                .cover_letter_template
                .trim()
                .to_owned()
        };
        let full = root.join(&rel);
        if !full.is_file() {
            if let Some(parent) = full.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let starter = "# Cover letter template\n\n\
                Keep cover letters to 3 short paragraphs strictly backed by verified facts in `context/`.\n\
                Leave empty or set Cover letters to **Skip** if you only want to submit CVs.\n\n\
                ## Structure\n\
                1. Why this product or engineering problem caught your eye (1 concrete, public detail).\n\
                2. Two verified proof points from your career ledger directly relevant to the role.\n\
                3. Short, direct close with availability.\n";
            if let Err(err) = std::fs::write(&full, starter) {
                self.set_toast(format!("Could not create template: {err}"), cx);
                return;
            }
            self.snapshot.profile_settings.cover_letter_template = rel.clone();
            let client = self.client.clone();
            let rel_save = rel.clone();
            cx.spawn(async move |_, cx| {
                let _ = cx
                    .background_executor()
                    .spawn(async move {
                        client.update_ledger_config(|cfg| {
                            if !cfg["communication"].is_object() {
                                cfg["communication"] = json!({});
                            }
                            cfg["communication"]["cover_letter_template"] = json!(rel_save);
                        })
                    })
                    .await;
            })
            .detach();
        }
        open_in_system(&full);
        self.set_toast(format!("Opened {rel}"), cx);
    }

    fn render_editable_kv(
        &self,
        key: &'static str,
        label: &str,
        display_value: String,
        edit_initial: String,
        placeholder: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_editing = self.editing_config_key == Some(key);
        let err = if is_editing {
            self.config_error.clone()
        } else {
            None
        };
        let all_sel = is_editing && self.input_all_selected;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .text_xs()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(if is_editing {
                                theme::text_secondary()
                            } else {
                                theme::text_muted()
                            })
                            .child(label.to_owned()),
                    )
                    .child(if is_editing {
                        // Seamless borderless inline editor across both Profile and Setup Wizard
                        let editor = div()
                            .id(SharedString::from(format!("cfg-edit-{key}")))
                            .max_w_full()
                            .max_h(px(96.0))
                            .overflow_y_scroll()
                            .pb(px(1.0))
                            .border_b_1()
                            .border_color(if err.is_some() {
                                theme::rose_text()
                            } else {
                                theme::lavender()
                            })
                            .cursor_text()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    window.focus(&this.focus_handle);
                                    this.active_input = ActiveInput::InlineConfig;
                                    cx.stop_propagation();
                                }),
                            )
                            .on_scroll_wheel(|_, _, cx| {
                                cx.stop_propagation();
                            })
                            .child(inline_editable_text(
                                &self.config_draft,
                                placeholder,
                                all_sel,
                                self.reduce_motion,
                                key,
                            ));
                        let wrapped = div().flex_1().min_w_0().flex().justify_end().child(editor);
                        wrapped.into_any_element()
                    } else {
                        let scrub_spec = numeric_scrub_spec(key);
                        let is_scrubbing = self
                            .scrub_config
                            .is_some_and(|(k, _, _, _, dragged)| k == key && dragged);
                        let init_for_down = edit_initial.clone();
                        let init_for_click = edit_initial;
                        // Subtle dashed underline signifies clickable/editable (+ vertical drag scrub on numbers!)
                        div()
                            .id(SharedString::from(format!("cfg-val-{key}")))
                            .min_w_0()
                            .overflow_hidden()
                            .pb(px(1.0))
                            .border_b_1()
                            .border_dashed()
                            .border_color(if is_scrubbing {
                                theme::lavender()
                            } else {
                                theme::lavender_border()
                            })
                            .hover(|s| {
                                s.border_color(theme::lavender())
                                    .text_color(theme::lavender())
                            })
                            .when(scrub_spec.is_some(), |d| d.cursor_row_resize())
                            .when(scrub_spec.is_none(), |d| d.cursor_pointer())
                            .text_color(if is_scrubbing {
                                theme::lavender()
                            } else {
                                theme::text_primary()
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, ev: &gpui::MouseDownEvent, window, cx| {
                                    window.focus(&this.focus_handle);
                                    cx.stop_propagation();
                                    if scrub_spec.is_some()
                                        && let Ok(v) = init_for_down.trim().parse::<i64>()
                                    {
                                        if this.editing_config_key.is_some() {
                                            this.cancel_inline_config(cx);
                                        }
                                        let sy: f32 = ev.position.y.into();
                                        this.scrub_config = Some((key, sy, v, v, false));
                                        cx.notify();
                                    }
                                }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if scrub_spec.is_some() {
                                    return;
                                }
                                if this.editing_config_key.is_some() {
                                    this.cancel_inline_config(cx);
                                }
                                this.start_inline_config(key, init_for_click.clone(), cx);
                            }))
                            .child(display_value)
                            .into_any_element()
                    }),
            )
            .when_some(err, |d, msg| {
                d.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::rose_text())
                        .child(msg),
                )
            })
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
        self.mark_sync_working("Saving…", cx);

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
            let _ = this.update(cx, |app, cx| match res {
                Ok(()) => app.mark_sync_saved("Saved", cx),
                Err(err) => {
                    app.mark_sync_failed("Save failed", cx);
                    app.set_toast(format!("Save failed: {err}"), cx);
                    app.refresh_snapshot_async(false, cx);
                }
            });
        })
        .detach();
    }

    fn toggle_cv_track(&mut self, track_name: String, cx: &mut Context<Self>) {
        let mut now_enabled = true;
        for node in &mut self.snapshot.cv_nodes {
            if !node.is_master && node.name == track_name {
                node.enabled = !node.enabled;
                now_enabled = node.enabled;
                break;
            }
        }
        let disabled: Vec<String> = self
            .snapshot
            .cv_nodes
            .iter()
            .filter(|n| !n.is_master && !n.enabled)
            .map(|n| n.name.clone())
            .collect();
        self.mark_sync_working("Saving…", cx);
        let client = self.client.clone();
        let name_msg = track_name.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    client.update_ledger_config(|cfg| {
                        if disabled.is_empty() {
                            if let Some(obj) = cfg.as_object_mut() {
                                obj.remove("disabled_cv_tracks");
                            }
                        } else {
                            cfg["disabled_cv_tracks"] = json!(disabled);
                        }
                    })
                })
                .await;
            let _ = this.update(cx, |app, cx| match res {
                Ok(()) => {
                    app.mark_sync_saved("Saved", cx);
                    app.set_toast(
                        if now_enabled {
                            format!("Enabled {name_msg} CV track")
                        } else {
                            format!("Paused {name_msg} CV track (PDF kept on disk)")
                        },
                        cx,
                    );
                }
                Err(err) => {
                    app.mark_sync_failed("Save failed", cx);
                    app.set_toast(format!("Save failed: {err}"), cx);
                    app.refresh_snapshot_async(false, cx);
                }
            });
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
        let today = chrono::Utc::now().date_naive();
        let mut items: Vec<(&RoleCard, u64)> = self
            .snapshot
            .active_cards
            .iter()
            .filter(|c| !self.dismissed_tidy.contains(&c.record_path))
            .filter_map(|c| {
                if let Some(d) = c.quiet_days.filter(|d| *d >= 5) {
                    return Some((c, d));
                }
                // Surface stale Saved / Prepared leads older than 14 days
                if matches!(c.stage.as_str(), "saved" | "prepared")
                    && let Ok(checked) = chrono::NaiveDate::parse_from_str(&c.checked, "%Y-%m-%d")
                {
                    let age = (today - checked).num_days();
                    if age >= 14 {
                        return Some((c, age as u64));
                    }
                }
                None
            })
            .collect();
        items.sort_by_key(|(_, age)| std::cmp::Reverse(*age));
        items.into_iter().map(|(c, _)| c).collect()
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
            ("Open Profile & Context", "⌘P", CommandAction::OpenProfile),
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
                "Pass a secret to agent…",
                "1-use",
                CommandAction::PassSecret,
            ),
            (
                "Copy full chat transcript (Markdown)",
                "Chat",
                CommandAction::CopyTranscript,
            ),
            (
                "Export chat transcript (.md)…",
                "Save",
                CommandAction::ExportTranscript,
            ),
            ("Run setup wizard…", "Setup", CommandAction::OpenSetupWizard),
            ("Start quick UI tour", "Guide", CommandAction::StartTour),
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
            CommandAction::OpenProfile => self.open_right_drawer(RightDrawer::Profile, cx),
            CommandAction::OpenTidy => self.open_right_drawer(RightDrawer::Tidy, cx),
            CommandAction::OpenBudget => {
                self.active_popover = ActivePopover::Budget;
                self.popover_pinned = true;
                cx.notify();
            }
            CommandAction::OpenSettings => {
                self.open_right_drawer(RightDrawer::Settings, cx);
            }
            CommandAction::ToggleAttention => {
                self.toggle_attention_only(cx);
            }
            CommandAction::ToggleArchive => {
                self.show_archive = !self.show_archive;
                self.archive_anim_epoch = self.archive_anim_epoch.wrapping_add(1);
                cx.notify();
            }
            CommandAction::RefreshWorkspace => {
                self.refresh_snapshot_async(true, cx);
            }
            CommandAction::ImportMasterCv => self.import_cv_dialog(cx),
            CommandAction::PassSecret => self.open_proactive_secret_modal(cx),
            CommandAction::CopyTranscript => self.copy_chat_transcript(cx),
            CommandAction::ExportTranscript => self.export_chat_transcript(cx),
            CommandAction::OpenSetupWizard => {
                self.setup_step = 0;
                self.active_popover = ActivePopover::SetupWizard;
                self.cmd_anim_epoch = self.cmd_anim_epoch.wrapping_add(1);
                cx.notify();
            }
            CommandAction::StartTour => {
                self.tour_step = 0;
                self.tour_prev_step = 0;
                self.tour_anim_epoch = self.tour_anim_epoch.wrapping_add(1);
                self.active_popover = ActivePopover::Tour;
                cx.notify();
            }
            CommandAction::AskAgent(prompt) => self.submit_prompt(prompt, false, cx),
        }
    }

    fn sync_secret_vault_state(&mut self, all: Vec<SecretRequest>, cx: &mut Context<Self>) {
        let mut new_ready_map: HashMap<String, String> = HashMap::new();
        let mut armed = Vec::new();
        let mut newly_pending: Option<SecretRequest> = None;

        for req in &all {
            match req.status {
                SecretStatus::Pending => {
                    if self.seen_pending_secrets.insert(req.id.clone())
                        && self.active_secret_req.is_none()
                    {
                        newly_pending = Some(req.clone());
                    }
                }
                SecretStatus::Ready => {
                    new_ready_map.insert(req.id.clone(), req.label.clone());
                    armed.push(req.clone());
                }
                SecretStatus::Consumed => {
                    if let Some(lbl) = self.known_ready_secrets.remove(&req.id) {
                        self.set_toast(format!("Secret consumed & shredded — {lbl}"), cx);
                    }
                }
                SecretStatus::Expired => {
                    if let Some(lbl) = self.known_ready_secrets.remove(&req.id) {
                        self.set_toast(format!("Secret expired & destroyed — {lbl}"), cx);
                    }
                }
                SecretStatus::Revoked => {
                    self.known_ready_secrets.remove(&req.id);
                }
            }
        }
        let changed = self.armed_secrets != armed || newly_pending.is_some();
        self.armed_secrets = armed;
        self.known_ready_secrets = new_ready_map;
        if let Some(req) = newly_pending {
            let key = format!("secret:{}", req.id);
            if self.notified_keys.insert(key.clone()) {
                self.send_desktop_notification(
                    &key,
                    "Job Harness",
                    "Secret requested",
                    &format!(
                        "{} — enter once in the app (auto-shreds after use)",
                        req.label
                    ),
                    None,
                    false,
                );
            }
            self.open_secret_modal(req, cx);
            return;
        }
        if changed || !self.armed_secrets.is_empty() {
            cx.notify();
        }
    }

    fn open_secret_modal(&mut self, req: SecretRequest, cx: &mut Context<Self>) {
        self.secret_drafts = vec![String::new(); req.fields.len()];
        self.secret_field_idx = 0;
        self.active_secret_req = Some(req);
        self.active_popover = ActivePopover::Secret;
        self.active_input = ActiveInput::SecretField;
        self.cmd_anim_epoch = self.cmd_anim_epoch.wrapping_add(1);
        cx.notify();
    }

    fn open_proactive_secret_modal(&mut self, cx: &mut Context<Self>) {
        let fields = vec![
            SecretFieldSpec {
                name: "username".into(),
                label: "Username or email".into(),
                placeholder: Some("name@example.com".into()),
                secret: false,
            },
            SecretFieldSpec {
                name: "password".into(),
                label: "Password or token".into(),
                placeholder: Some("Stored 0600 in temp (1-use)".into()),
                secret: true,
            },
        ];
        if let Ok(req) = secrets::create_request(
            "One-time credential".into(),
            "Passed privately via ephemeral 0600 file — never enters chat history or Git.".into(),
            None,
            fields,
            300,
            1,
        ) {
            self.seen_pending_secrets.insert(req.id.clone());
            self.open_secret_modal(req, cx);
        }
    }

    fn close_secret_modal(&mut self, revoke_if_pending: bool, cx: &mut Context<Self>) {
        if let Some(req) = self.active_secret_req.take()
            && revoke_if_pending
            && req.status == SecretStatus::Pending
        {
            let id = req.id;
            std::thread::spawn(move || {
                let _ = secrets::revoke_request(&id);
            });
        }
        for s in &mut self.secret_drafts {
            s.clear();
        }
        self.secret_drafts.clear();
        self.secret_field_idx = 0;
        if self.active_popover == ActivePopover::Secret {
            self.active_popover = ActivePopover::None;
        }
        if self.active_input == ActiveInput::SecretField {
            self.active_input = ActiveInput::None;
        }
        cx.notify();
    }

    fn submit_secret_modal(&mut self, cx: &mut Context<Self>) {
        let Some(req) = self.active_secret_req.clone() else {
            return;
        };
        let mut values = BTreeMap::new();
        for (idx, field) in req.fields.iter().enumerate() {
            let val = self
                .secret_drafts
                .get(idx)
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            if val.is_empty() {
                self.secret_field_idx = idx;
                self.set_toast(format!("Enter {} first", field.label), cx);
                return;
            }
            values.insert(field.name.clone(), val);
        }
        let id = req.id.clone();
        let label = req.label.clone();
        let field_names: Vec<String> = req.fields.iter().map(|f| f.name.clone()).collect();
        match secrets::fulfill_request(&id, &values, Some(300)) {
            Ok(armed) => {
                self.known_ready_secrets
                    .insert(armed.id.clone(), armed.label.clone());
                self.armed_secrets.retain(|r| r.id != armed.id);
                self.armed_secrets.insert(0, armed);
                self.close_secret_modal(false, cx);
                if self.chat_input.trim().is_empty() {
                    let fields_hint = field_names.join(", ");
                    self.chat_input = format!(
                        "One-time secret ready (id: {id}, fields: {fields_hint}). Read once via `jh secret read {id} <field>`."
                    );
                }
                self.set_toast(
                    format!("Armed 1-use secret ({label}) — 5m self-destruct"),
                    cx,
                );
            }
            Err(e) => {
                self.set_toast(format!("Could not arm secret: {e}"), cx);
            }
        }
    }

    fn destroy_secret_now(&mut self, id: String, cx: &mut Context<Self>) {
        self.armed_secrets.retain(|r| r.id != id);
        self.known_ready_secrets.remove(&id);
        std::thread::spawn(move || {
            let _ = secrets::revoke_request(&id);
        });
        self.set_toast("Secret overwritten and destroyed", cx);
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
                        app.mark_sync_saved("Imported", cx);
                        app.set_toast(format!("Imported Master CV: {rel}"), cx);
                        app.refresh_snapshot_async(false, cx);
                    }
                    Err(e) => app.set_toast(format!("Import failed: {e}"), cx),
                });
            })
            .detach();
        }
    }

    fn replace_track_cv_dialog(&mut self, relative_target: String, cx: &mut Context<Self>) {
        if let Some(picked) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_file()
        {
            let client = self.client.clone();
            cx.spawn(async move |this, cx| {
                let res = cx
                    .background_executor()
                    .spawn(async move { client.replace_track_cv(&relative_target, &picked) })
                    .await;
                let _ = this.update(cx, |app, cx| match res {
                    Ok(rel) => {
                        app.mark_sync_saved("Replaced", cx);
                        app.set_toast(format!("Replaced track CV: {rel}"), cx);
                        app.refresh_snapshot_async(false, cx);
                    }
                    Err(e) => app.set_toast(format!("Replace failed: {e}"), cx),
                });
            })
            .detach();
        }
    }

    fn submit_chat_input(&mut self, steer: bool, cx: &mut Context<Self>) {
        let raw = std::mem::take(&mut self.chat_input);
        self.input_all_selected = false;
        let snippets = std::mem::take(&mut self.pasted_snippets);
        let mut trimmed = raw.trim().to_owned();
        for snip in snippets {
            if !trimmed.is_empty() {
                trimmed.push_str("\n\n");
            }
            trimmed.push_str(&snip);
        }
        let mut prompt = match self.attached_role.take() {
            Some((company, record_path)) => {
                if trimmed.is_empty() {
                    format!(
                        "Brief me on {company} (`{record_path}`) and suggest the clearest next step."
                    )
                } else {
                    format!("{trimmed}\n\n[Role context: {company} — `{record_path}`]")
                }
            }
            None => trimmed,
        };
        if let Some(quote) = self.quoted_reply.take()
            && !prompt.is_empty()
        {
            prompt = format!("> {}\n\n{prompt}", truncate_end(&quote, 220));
        }
        let attachments = std::mem::take(&mut self.chat_attachments);
        if !prompt.is_empty() || !attachments.is_empty() {
            if steer && self.streaming {
                self.stop_stream(cx);
            }
            self.submit_prompt_with_attachments(prompt, attachments, false, cx);
        }
    }

    fn submit_prompt(&mut self, prompt: String, from_quick_bar: bool, cx: &mut Context<Self>) {
        self.submit_prompt_with_attachments(prompt, Vec::new(), from_quick_bar, cx);
    }

    fn submit_prompt_with_attachments(
        &mut self,
        prompt: String,
        attachments: Vec<PathBuf>,
        from_quick_bar: bool,
        cx: &mut Context<Self>,
    ) {
        let trimmed = prompt.trim().to_owned();
        if trimmed.is_empty() && attachments.is_empty() {
            return;
        }
        if self.streaming {
            if !trimmed.is_empty() {
                self.queued_prompts.push_back(trimmed);
            }
            if !attachments.is_empty() {
                self.chat_attachments.extend(attachments);
            }
            cx.notify();
            return;
        }
        let now = Instant::now();
        let sent_label = chrono::Local::now().format("%H:%M").to_string();
        let display_user = if trimmed.is_empty() {
            format!("Attached {} item(s)", attachments.len())
        } else {
            trimmed.clone()
        };
        let user_len = display_user.len();
        let turn_fast =
            self.agent_opts.fast_mode && self.runtime.supports_fast_mode(&self.agent_opts);
        self.chat_messages.push(ChatMessage {
            is_user: true,
            text: display_user,
            revealed: user_len,
            sent_label: sent_label.clone(),
            attachments: attachments.clone(),
            citations: vec![],
            steps: vec![],
            thinking: String::new(),
            started_at: now,
            duration_ms: None,
            total_tokens: None,
            steps_expanded: false,
            is_error: false,
            fast_mode: false,
        });
        self.chat_messages.push(ChatMessage {
            is_user: false,
            text: String::new(),
            revealed: 0,
            sent_label,
            attachments: vec![],
            citations: vec![],
            steps: vec![],
            thinking: String::new(),
            started_at: now,
            duration_ms: None,
            total_tokens: None,
            steps_expanded: true,
            is_error: false,
            fast_mode: turn_fast,
        });
        self.chat_follow = true;
        self.chat_scroll.scroll_to_bottom();
        self.streaming = true;
        self.stream_status = "Thinking…".into();
        self.stream_turn_id = self.stream_turn_id.wrapping_add(1);
        let turn_id = self.stream_turn_id;
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
            attachments,
            self.agent_opts.clone(),
            tx,
            cancel,
        );

        // Network text arrives in bursts; reveal it at our own pace so it ramps up and
        // settles (quick when far behind, gentle when close) instead of jumping.
        cx.spawn(async move |this, cx| {
            let mut done = false;
            let mut tick_count: u64 = 0;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                tick_count = tick_count.wrapping_add(1);
                let mut batch = Vec::new();
                while let Ok(ev) = rx.try_recv() {
                    batch.push(ev);
                }
                let keep_going = this
                    .update(cx, |app, cx| {
                        if app.stream_turn_id != turn_id {
                            return false;
                        }
                        // Tick elapsed timer smoothly (~2x/sec) while waiting on tool calls
                        let mut dirty = !batch.is_empty() || tick_count.is_multiple_of(30);
                        for ev in batch {
                            match ev {
                                StreamEvent::Status(s) => app.stream_status = s,
                                StreamEvent::ToolStart { id, name, detail } => {
                                    if let Some(last) = app.chat_messages.last_mut() {
                                        if let Some(existing) =
                                            last.steps.iter_mut().find(|s| s.id == id)
                                        {
                                            existing.detail = detail;
                                        } else {
                                            last.steps.push(ToolStep {
                                                id,
                                                name,
                                                detail,
                                                output: String::new(),
                                                expanded: false,
                                                done: false,
                                                ok: true,
                                                duration_ms: None,
                                            });
                                        }
                                    }
                                }
                                StreamEvent::ToolFinish {
                                    id,
                                    ok,
                                    duration_ms,
                                    output,
                                } => {
                                    if let Some(last) = app.chat_messages.last_mut() {
                                        if let Some(step) =
                                            last.steps.iter_mut().rev().find(|s| s.id == id)
                                        {
                                            step.done = true;
                                            step.ok = ok;
                                            step.duration_ms = duration_ms;
                                            if !output.is_empty() {
                                                step.output = output;
                                            }
                                        } else if let Some(step) =
                                            last.steps.iter_mut().rev().find(|s| !s.done)
                                        {
                                            step.done = true;
                                            step.ok = ok;
                                            step.duration_ms = duration_ms;
                                            if !output.is_empty() {
                                                step.output = output;
                                            }
                                        }
                                    }
                                }
                                StreamEvent::Thinking(chunk) => {
                                    if let Some(last) = app.chat_messages.last_mut() {
                                        last.thinking.push_str(&chunk);
                                    }
                                }
                                StreamEvent::Usage { total_tokens } => {
                                    if let Some(last) = app.chat_messages.last_mut() {
                                        last.total_tokens = Some(total_tokens);
                                    }
                                }
                                StreamEvent::Delta(chunk) => {
                                    if let Some(last) = app.chat_messages.last_mut() {
                                        last.text.push_str(&chunk);
                                        last.citations = extract_citations(&last.text);
                                    }
                                }
                                StreamEvent::Error(err) => {
                                    if let Some(last) = app.chat_messages.last_mut() {
                                        last.text = format!("Error: {err}");
                                        last.revealed = last.text.len();
                                        last.is_error = true;
                                    }
                                }
                                StreamEvent::Done => done = true,
                            }
                        }
                        let instant = app.reduce_motion || !app.streaming;
                        if let Some(last) = app.chat_messages.last_mut()
                            && !last.is_user
                            && last.revealed < last.text.len()
                        {
                            let len = last.text.len();
                            if instant {
                                last.revealed = len;
                            } else {
                                let lag = len - last.revealed;
                                let mut idx =
                                    last.revealed + (lag / 8).clamp(3, lag.max(3)).min(lag);
                                while idx < len && !last.text.is_char_boundary(idx) {
                                    idx += 1;
                                }
                                last.revealed = idx;
                            }
                            app.quick_reply_preview = last.text[..last.revealed].to_owned();
                            dirty = true;
                        }
                        let fully_shown = app
                            .chat_messages
                            .last()
                            .is_none_or(|m| m.revealed >= m.text.len());
                        let finished = done && fully_shown;
                        if finished {
                            let mut notify_turn: Option<(&'static str, String)> = None;
                            if let Some(last) = app.chat_messages.last_mut()
                                && !last.is_user
                            {
                                if last.duration_ms.is_none() {
                                    last.duration_ms =
                                        Some(last.started_at.elapsed().as_millis() as u64);
                                }
                                let dur = last.duration_ms.unwrap_or(0);
                                if last.is_error {
                                    notify_turn = Some((
                                        "Agent run failed",
                                        truncate_end(last.text.trim(), 120),
                                    ));
                                } else if dur >= 15_000 {
                                    let first_line = last
                                        .text
                                        .lines()
                                        .find(|l| !l.trim().is_empty())
                                        .unwrap_or("Completed agent run")
                                        .trim();
                                    notify_turn =
                                        Some(("Agent run finished", truncate_end(first_line, 120)));
                                }
                                last.sent_label = chrono::Local::now().format("%H:%M").to_string();
                                for st in &mut last.steps {
                                    st.done = true;
                                }
                                if !last.text.is_empty() {
                                    last.steps_expanded = false;
                                }
                            }
                            if let Some((sub, body)) = notify_turn {
                                let turn_key = format!("turn-{}", app.stream_turn_id);
                                app.send_desktop_notification(
                                    &turn_key,
                                    "Job Harness",
                                    sub,
                                    &body,
                                    None,
                                    false,
                                );
                            }
                            app.streaming = false;
                            app.stream_status.clear();
                            app.stream_cancel = None;
                            app.refresh_snapshot_async(false, cx);
                            dirty = true;
                        }
                        if dirty && app.chat_follow {
                            app.chat_scroll.scroll_to_bottom();
                        }
                        if dirty {
                            cx.notify();
                        }
                        if finished && let Some(next_q) = app.queued_prompts.pop_front() {
                            app.submit_prompt(next_q, false, cx);
                        }
                        !finished
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn stop_stream(&mut self, cx: &mut Context<Self>) {
        if let Some(flag) = self.stream_cancel.take() {
            flag.store(true, Ordering::Relaxed);
        }
        self.stream_turn_id = self.stream_turn_id.wrapping_add(1);
        for msg in &mut self.chat_messages {
            if !msg.is_user && msg.duration_ms.is_none() {
                msg.duration_ms = Some(msg.started_at.elapsed().as_millis() as u64);
                msg.revealed = msg.text.len();
                for st in &mut msg.steps {
                    st.done = true;
                }
            }
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
            self.refresh_snapshot_async(true, cx);
            return;
        }
        if cmd && key == "d" {
            self.open_right_drawer(RightDrawer::Cvs, cx);
            return;
        }
        if cmd && key == "p" {
            self.open_right_drawer(RightDrawer::Profile, cx);
            return;
        }
        if cmd && key == "," {
            self.open_right_drawer(RightDrawer::Settings, cx);
            return;
        }
        if cmd && key == "z" && self.undo_move.is_some() {
            self.perform_undo(cx);
            return;
        }
        // Native Select All (⌘A), Cut (⌘X), Copy (⌘C), Paste (⌘V with large-paste chips in Chat)
        if cmd && key == "a" && self.active_input != ActiveInput::None {
            self.input_all_selected = true;
            cx.notify();
            return;
        }
        if cmd && key == "x" && self.active_input != ActiveInput::None {
            let cut = match self.active_input {
                ActiveInput::CommandBar => std::mem::take(&mut self.command_query),
                ActiveInput::Search => std::mem::take(&mut self.search_query),
                ActiveInput::SankeyFilter => std::mem::take(&mut self.sankey_filter),
                ActiveInput::SecretField => self
                    .secret_drafts
                    .get_mut(self.secret_field_idx)
                    .map(std::mem::take)
                    .unwrap_or_default(),
                ActiveInput::QuickAsk => std::mem::take(&mut self.quick_input),
                ActiveInput::Chat => std::mem::take(&mut self.chat_input),
                ActiveInput::ModelField => std::mem::take(&mut self.model_draft),
                ActiveInput::InlineConfig => {
                    self.config_error = None;
                    std::mem::take(&mut self.config_draft)
                }
                ActiveInput::None => String::new(),
            };
            self.input_all_selected = false;
            if !cut.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(cut));
                self.set_toast("Cut to clipboard", cx);
            }
            cx.notify();
            return;
        }
        if cmd && key == "v" {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(pasted) = item.text()
            {
                let clean = pasted.replace('\r', "");
                let replace_all = std::mem::take(&mut self.input_all_selected);
                match self.active_input {
                    ActiveInput::CommandBar => {
                        if replace_all {
                            self.command_query.clear();
                        }
                        self.command_query.push_str(clean.trim());
                        self.command_index = 0;
                    }
                    ActiveInput::Search => {
                        if replace_all {
                            self.search_query.clear();
                        }
                        self.search_query.push_str(clean.trim());
                    }
                    ActiveInput::SankeyFilter => {
                        if replace_all {
                            self.sankey_filter.clear();
                        }
                        self.sankey_filter.push_str(clean.trim());
                    }
                    ActiveInput::SecretField => {
                        if let Some(buf) = self.secret_drafts.get_mut(self.secret_field_idx) {
                            if replace_all {
                                buf.clear();
                            }
                            buf.push_str(clean.trim());
                        }
                    }
                    ActiveInput::QuickAsk => {
                        if replace_all {
                            self.quick_input.clear();
                        }
                        self.quick_input.push_str(&clean);
                    }
                    ActiveInput::Chat => {
                        if replace_all {
                            self.chat_input.clear();
                        }
                        // Large multi-line pastes become a compact chip (T3 Code pattern) instead of flooding the composer!
                        if clean.len() > 300 || clean.lines().count() >= 5 {
                            self.pasted_snippets.push(clean);
                            self.set_toast("Attached pasted text block", cx);
                        } else {
                            self.chat_input.push_str(&clean);
                        }
                    }
                    ActiveInput::ModelField => {
                        if replace_all {
                            self.model_draft.clear();
                        }
                        self.model_draft.push_str(clean.trim());
                    }
                    ActiveInput::InlineConfig => {
                        if replace_all {
                            self.config_draft.clear();
                        }
                        self.config_draft.push_str(clean.trim());
                        if let Some(k) = self.editing_config_key {
                            self.config_error =
                                Self::validate_inline_config(k, &self.config_draft).err();
                        }
                    }
                    ActiveInput::None => {}
                }
                cx.notify();
            }
            return;
        }
        if cmd && key == "c" {
            // 1. Prioritize active drag-selection inside Markdown (Chat / Role Inspector / Focused Reader)
            if let Some(selected) = self.selected_markdown_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(selected));
                self.set_toast("Copied selection", cx);
                return;
            }
            // 2. Otherwise copy focused input buffer (or latest assistant reply)
            let to_copy = match self.active_input {
                ActiveInput::CommandBar if !self.command_query.is_empty() => {
                    Some(self.command_query.clone())
                }
                ActiveInput::Chat if !self.chat_input.is_empty() => Some(self.chat_input.clone()),
                ActiveInput::QuickAsk if !self.quick_input.is_empty() => {
                    Some(self.quick_input.clone())
                }
                ActiveInput::Search if !self.search_query.is_empty() => {
                    Some(self.search_query.clone())
                }
                ActiveInput::SankeyFilter if !self.sankey_filter.is_empty() => {
                    Some(self.sankey_filter.clone())
                }
                ActiveInput::ModelField if !self.model_draft.is_empty() => {
                    Some(self.model_draft.clone())
                }
                ActiveInput::InlineConfig if !self.config_draft.is_empty() => {
                    Some(self.config_draft.clone())
                }
                _ => self
                    .chat_messages
                    .iter()
                    .rev()
                    .find(|m| !m.is_user && !m.text.is_empty())
                    .map(|m| m.text.clone()),
            };
            if let Some(txt) = to_copy {
                cx.write_to_clipboard(ClipboardItem::new_string(txt));
                self.set_toast("Copied to clipboard", cx);
            }
            return;
        }
        if key == "escape" {
            if self.editing_config_key.is_some() {
                self.cancel_inline_config(cx);
                return;
            }
            if self.expanded_reader.is_some() {
                self.expanded_reader = None;
                cx.notify();
                return;
            }
            if self.active_popover == ActivePopover::Secret {
                self.close_secret_modal(true, cx);
                return;
            }
            if self.active_popover != ActivePopover::None || self.quick_popover_open {
                self.close_overlays();
                self.active_input = ActiveInput::None;
                cx.notify();
                return;
            }
            if self.sankey_bucket.is_some() {
                self.sankey_bucket = None;
                self.sankey_filter.clear();
                if self.active_input == ActiveInput::SankeyFilter {
                    self.active_input = ActiveInput::None;
                }
                cx.notify();
                return;
            }
            if self.chat_fullscreen {
                self.chat_fullscreen = false;
                cx.notify();
                return;
            }
            self.right_drawer = RightDrawer::Closed;
            self.active_input = ActiveInput::None;
            cx.notify();
            return;
        }
        // Keyboard section stepping inside the Focused Reader Modal (← / →)
        if let Some((_, body)) = &self.expanded_reader
            && !cmd
            && (key == "left" || key == "right")
        {
            let sec_count = parse_brief_sections(body).len();
            if sec_count >= 2 {
                self.brief_section = if key == "left" {
                    match self.brief_section {
                        None | Some(0) => Some(sec_count.saturating_sub(1)),
                        Some(i) => Some(i - 1),
                    }
                } else {
                    match self.brief_section {
                        None => Some(0),
                        Some(i) if i + 1 < sec_count => Some(i + 1),
                        Some(_) => None,
                    }
                };
                self.brief_section_epoch = self.brief_section_epoch.wrapping_add(1);
                cx.notify();
            }
            return;
        }
        if self.active_input == ActiveInput::None && !cmd {
            if key == "/" {
                self.active_input = ActiveInput::Search;
                cx.notify();
                return;
            }
            if self.right_drawer == RightDrawer::Role {
                let target_tab = match key {
                    "1" => Some(DrawerTab::Overview),
                    "2" => Some(DrawerTab::Brief),
                    "3" => Some(DrawerTab::Notes),
                    "[" => Some(match self.drawer_tab {
                        DrawerTab::Overview => DrawerTab::Notes,
                        DrawerTab::Brief => DrawerTab::Overview,
                        DrawerTab::Notes => DrawerTab::Brief,
                    }),
                    "]" => Some(match self.drawer_tab {
                        DrawerTab::Overview => DrawerTab::Brief,
                        DrawerTab::Brief => DrawerTab::Notes,
                        DrawerTab::Notes => DrawerTab::Overview,
                    }),
                    _ => None,
                };
                if let Some(tab) = target_tab {
                    self.switch_role_tab(tab, cx);
                    return;
                }
            }
            if key == "left" {
                self.nudge_board_x(284.0, cx);
                return;
            }
            if key == "right" {
                self.nudge_board_x(-284.0, cx);
                return;
            }
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
                    if cmd || std::mem::take(&mut self.input_all_selected) {
                        self.command_query.clear();
                    } else {
                        self.command_query.pop();
                    }
                    self.command_index = 0;
                    cx.notify();
                    return;
                }
                _ => {
                    if !cmd
                        && let Some(ch) = &event.keystroke.key_char
                        && !ch.chars().any(char::is_control)
                    {
                        if std::mem::take(&mut self.input_all_selected) {
                            self.command_query.clear();
                        }
                        self.command_query.push_str(ch);
                        self.command_index = 0;
                        cx.notify();
                    }
                    return;
                }
            }
        }

        let shift = event.keystroke.modifiers.shift;
        let alt = event.keystroke.modifiers.alt;
        let all_sel = self.input_all_selected;
        let active_in = self.active_input;
        let target_buf = match active_in {
            ActiveInput::Search => Some(&mut self.search_query),
            ActiveInput::SankeyFilter => Some(&mut self.sankey_filter),
            ActiveInput::SecretField => self.secret_drafts.get_mut(self.secret_field_idx),
            ActiveInput::QuickAsk => Some(&mut self.quick_input),
            ActiveInput::Chat => Some(&mut self.chat_input),
            ActiveInput::ModelField => Some(&mut self.model_draft),
            ActiveInput::InlineConfig => Some(&mut self.config_draft),
            ActiveInput::None | ActiveInput::CommandBar => None,
        };
        if let Some(buf) = target_buf {
            match key {
                "up" | "down" if active_in == ActiveInput::Chat => {
                    let items = self.chat_picker_items();
                    if !items.is_empty() {
                        self.chat_picker_idx = if key == "down" {
                            (self.chat_picker_idx + 1) % items.len()
                        } else {
                            (self.chat_picker_idx + items.len() - 1) % items.len()
                        };
                        cx.notify();
                    }
                }
                "tab" if active_in == ActiveInput::SecretField => {
                    let n = self.secret_drafts.len().max(1);
                    self.secret_field_idx = if shift {
                        (self.secret_field_idx + n - 1) % n
                    } else {
                        (self.secret_field_idx + 1) % n
                    };
                    self.input_all_selected = false;
                    cx.notify();
                }
                "tab" if active_in == ActiveInput::Chat => {
                    let items = self.chat_picker_items();
                    if let Some(item) = items.into_iter().nth(self.chat_picker_idx) {
                        self.apply_chat_picker_item(item.insert_text, item.role_attach, cx);
                    }
                }
                "up" | "down" if active_in == ActiveInput::InlineConfig => {
                    if let Some(k) = self.editing_config_key
                        && let Some((step, min_v, _max_v)) = numeric_scrub_spec(k)
                        && let Ok(cur) = self.config_draft.trim().parse::<i64>()
                    {
                        let mult = if shift { 5 } else { 1 };
                        let delta = if key == "up" {
                            step * mult
                        } else {
                            -step * mult
                        };
                        let next = (cur + delta).max(min_v);
                        self.config_draft = next.to_string();
                        self.input_all_selected = false;
                        self.config_error = None;
                        cx.notify();
                    }
                }
                "backspace" => {
                    if cmd || all_sel {
                        buf.clear();
                    } else {
                        buf.pop();
                    }
                    self.input_all_selected = false;
                    if active_in == ActiveInput::InlineConfig
                        && let Some(k) = self.editing_config_key
                    {
                        self.config_error =
                            Self::validate_inline_config(k, &self.config_draft).err();
                    }
                    cx.notify();
                }
                "enter" => {
                    if active_in == ActiveInput::Chat && shift {
                        if all_sel {
                            buf.clear();
                        }
                        buf.push('\n');
                        self.input_all_selected = false;
                        cx.notify();
                    } else {
                        self.input_all_selected = false;
                        match active_in {
                            ActiveInput::QuickAsk => {
                                let q = std::mem::take(&mut self.quick_input);
                                self.submit_prompt(q, true, cx);
                            }
                            ActiveInput::Chat => {
                                let picker = self.chat_picker_items();
                                if !picker.is_empty()
                                    && let Some(item) = picker.into_iter().nth(self.chat_picker_idx)
                                {
                                    self.apply_chat_picker_item(
                                        item.insert_text,
                                        item.role_attach,
                                        cx,
                                    );
                                } else {
                                    // Steer is default on Enter while streaming; ⌘⏎ / ⌥⏎ queues for later!
                                    let steer = !(cmd || alt) && self.streaming;
                                    self.submit_chat_input(steer, cx);
                                }
                            }
                            ActiveInput::ModelField => {
                                self.apply_model_draft(cx);
                            }
                            ActiveInput::InlineConfig => {
                                self.commit_inline_config(cx);
                            }
                            ActiveInput::SecretField => {
                                if self.secret_field_idx + 1 < self.secret_drafts.len() {
                                    self.secret_field_idx += 1;
                                    cx.notify();
                                } else {
                                    self.submit_secret_modal(cx);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {
                    if !cmd
                        && let Some(ch) = &event.keystroke.key_char
                        && !ch.chars().any(char::is_control)
                    {
                        if all_sel {
                            buf.clear();
                        }
                        buf.push_str(ch);
                        self.input_all_selected = false;
                        if active_in == ActiveInput::InlineConfig {
                            self.config_error = None;
                        }
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
        let unseen_activity = self
            .snapshot
            .latest_activity_at
            .as_ref()
            .is_some_and(|latest| {
                self.last_seen_activity_at
                    .as_ref()
                    .is_none_or(|seen| seen < latest)
            });
        let urgent = self.snapshot.urgent_count > 0 || unseen_activity;
        let chat_open = self.chat_open;
        let cvs_open = self.right_drawer == RightDrawer::Cvs;
        let profile_open = self.right_drawer == RightDrawer::Profile;
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
                    .flex_shrink_0()
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
                            .rounded(theme::r_sm())
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
                    .child({
                        let local_root = self.client.target.local_root().map(PathBuf::from);
                        let has_dir = local_root.is_some();
                        div()
                            .id("topbar-open-ws")
                            .px(px(6.0))
                            .py(px(3.0))
                            .rounded(theme::r_sm())
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .when(has_dir, |d| {
                                let p = local_root.unwrap();
                                d.hover(|s| s.bg(theme::row_hover()))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |_, _, _, _| {
                                        open_in_system(&p);
                                    }))
                            })
                            .child(
                                div()
                                    .w(px(19.0))
                                    .h(px(19.0))
                                    .rounded(theme::r_xs())
                                    .bg(gpui::rgba(0x38bdf81c))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        svg()
                                            .path("icons/folder.svg")
                                            .size(px(12.0))
                                            .text_color(gpui::rgba(0x38bdf8dd)),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_secondary())
                                    .child(ws_label),
                            )
                    }),
            )
            // Mathematically balanced Filter Roles input: fluid center with >=40px drag handles on both sides, borderless until focused!
            .when(!self.chat_fullscreen, |bar| {
                let search_active = self.active_input == ActiveInput::Search;
                bar.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(px(40.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .id("search-box")
                                .w_full()
                                .max_w(px(440.0))
                                .px_3()
                                .py(px(5.0))
                                .rounded(theme::r_md())
                                .bg(if search_active {
                                    theme::surface_subtle()
                                } else {
                                    gpui::rgba(0xffffff08)
                                })
                                .border_1()
                                .border_color(if search_active {
                                    theme::hairline_strong()
                                } else {
                                    gpui::rgba(0x00000000)
                                })
                                .hover(|s| {
                                    if search_active {
                                        s
                                    } else {
                                        s.bg(gpui::rgba(0xffffff0e))
                                    }
                                })
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_text()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        window.focus(&this.focus_handle);
                                        this.active_input = ActiveInput::Search;
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.active_input = ActiveInput::Search;
                                    cx.notify();
                                }))
                                .child(svg().path("icons/search.svg").size(px(12.5)).text_color(
                                    if search_active {
                                        theme::text_secondary()
                                    } else {
                                        theme::text_muted()
                                    },
                                ))
                                .child(input_line(
                                    &self.search_query,
                                    "Filter roles…",
                                    search_active,
                                    self.reduce_motion,
                                    "search",
                                    44,
                                ))
                                .when(!self.search_query.is_empty(), |box_d| {
                                    box_d.child(
                                        div()
                                            .id("search-clear-btn")
                                            .flex_shrink_0()
                                            .w(px(18.0))
                                            .h(px(18.0))
                                            .rounded(px(99.0))
                                            .hover(|s| {
                                                s.bg(theme::row_hover())
                                                    .text_color(theme::text_primary())
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .text_size(px(11.0))
                                            .text_color(theme::text_muted())
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation();
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.search_query.clear();
                                                this.active_input = ActiveInput::None;
                                                cx.notify();
                                            }))
                                            .child("✕"),
                                    )
                                }),
                        ),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("btn-cmd-k")
                            .px_2()
                            .py_1()
                            .rounded(theme::r_sm())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::row_hover()))
                            .text_xs()
                            .text_color(theme::text_muted())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_command_bar(cx);
                            }))
                            .child("⌘K"),
                    )
                    // Icon-only Tidy button matching Bell, with crisp corner count badge when tidy_n > 0
                    .child(
                        div()
                            .id("btn-tidy")
                            .relative()
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded(theme::r_sm())
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(tidy_open, |d| d.bg(theme::surface_active()))
                            .hover(|s| s.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_right_drawer(RightDrawer::Tidy, cx);
                            }))
                            .child(svg().path("icons/tidy.svg").size(px(15.0)).text_color(
                                if tidy_open {
                                    theme::text_primary()
                                } else {
                                    theme::text_secondary()
                                },
                            ))
                            .when(tidy_n > 0, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .top(px(1.0))
                                        .right(px(0.0))
                                        .min_w(px(14.0))
                                        .h(px(14.0))
                                        .px(px(3.0))
                                        .rounded(px(99.0))
                                        .bg(theme::lavender())
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(8.5))
                                        .font_weight(FontWeight::BOLD)
                                        .text_color(theme::ink_dark())
                                        .child(if tidy_n > 99 {
                                            "99".to_owned()
                                        } else {
                                            tidy_n.to_string()
                                        }),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("btn-cvs")
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded(theme::r_sm())
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(cvs_open, |d| d.bg(theme::surface_active()))
                            .hover(|s| s.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_right_drawer(RightDrawer::Cvs, cx);
                            }))
                            .child(svg().path("icons/file-text.svg").size(px(15.0)).text_color(
                                if cvs_open {
                                    theme::text_primary()
                                } else {
                                    theme::text_secondary()
                                },
                            )),
                    )
                    .child(
                        div()
                            .id("btn-profile")
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded(theme::r_sm())
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(profile_open, |d| d.bg(theme::surface_active()))
                            .hover(|s| s.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_right_drawer(RightDrawer::Profile, cx);
                            }))
                            .child(svg().path("icons/user.svg").size(px(15.5)).text_color(
                                if profile_open {
                                    theme::text_primary()
                                } else {
                                    theme::text_secondary()
                                },
                            )),
                    )
                    .child(
                        div()
                            .id("btn-bell")
                            .relative()
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded(theme::r_sm())
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(self.right_drawer == RightDrawer::Notifications, |d| {
                                d.bg(theme::surface_active())
                            })
                            .hover(|s| s.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_right_drawer(RightDrawer::Notifications, cx);
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
                            .rounded(theme::r_sm())
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .when(self.right_drawer == RightDrawer::Settings, |d| {
                                d.bg(theme::surface_active())
                            })
                            .hover(|s| s.bg(theme::row_hover()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_right_drawer(RightDrawer::Settings, cx);
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

    fn active_model_badge(&self) -> String {
        if let Some(m) = self.agent_opts.model.as_deref().filter(|s| !s.is_empty()) {
            return truncate_end(m, 18);
        }
        if let Some(def) = self.runtime.default_model() {
            return truncate_end(&def, 18);
        }
        "default".into()
    }

    fn apply_model_draft(&mut self, cx: &mut Context<Self>) {
        let trimmed = self.model_draft.trim().to_owned();
        self.agent_opts.model = (!trimmed.is_empty()).then_some(trimmed);
        self.active_input = ActiveInput::None;
        self.set_toast(format!("Model set to {}", self.active_model_badge()), cx);
    }

    fn resolve_company_for_brief(&self, brief_path: &str) -> Option<(String, String)> {
        if let Some(card) = self.snapshot.active_cards.iter().find(|c| {
            c.brief_path
                .as_deref()
                .is_some_and(|bp| bp == brief_path || bp.ends_with(brief_path))
        }) {
            return Some((
                card.company.clone(),
                title_case_stage(&card.stage).to_lowercase(),
            ));
        }
        let stem = brief_path
            .rsplit('/')
            .next()
            .unwrap_or(brief_path)
            .trim_end_matches(".md");
        let mut parts = stem.split('-');
        let raw_co = parts.next().unwrap_or(stem);
        let stage = parts.next().unwrap_or("interview");
        let mut chars = raw_co.chars();
        let c = chars.next()?;
        let co = format!("{}{}", c.to_uppercase(), chars.as_str());
        Some((co, stage.to_owned()))
    }

    fn suggest_chat_followups(&self, msg: &ChatMessage) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let lower = msg.text.to_ascii_lowercase();

        // 1. If the assistant explicitly suggested a confirmation phrase (`say "..."`, `reply "..."`), offer it verbatim!
        if let Some(phrase) = extract_suggested_reply_phrase(&msg.text) {
            let mut chars = phrase.chars();
            let label = match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => phrase.clone(),
            };
            out.push((truncate_end(&label, 44), phrase));
        } else if lower.contains("haven't changed anything")
            || lower.contains("haven’t changed anything")
            || lower.contains("want me to ")
            || lower.contains("would you like me to ")
            || lower.contains("should i ")
        {
            out.push(("Yes, go ahead".into(), "Yes, go ahead.".into()));
        }

        // 2. If this specific reply cited a prep brief, offer a 1-question-at-a-time mock drill.
        if out.len() < 2
            && let Some(brief) = msg.citations.iter().find(|c| c.contains("briefs/"))
            && let Some((company, stage)) = self.resolve_company_for_brief(brief)
        {
            let stage_noun = match stage.as_str() {
                "screen" | "screening" => "screen",
                "technical" => "technical interview",
                "final" => "final round",
                _ => "interview",
            };
            out.push((
                format!("Practise for the {company} {stage_noun}"),
                format!(
                    "Run a 1-question-at-a-time mock prep drill for {company} using `{brief}`."
                ),
            ));
        }

        // 3. Only suggest drafting a reply when this reply actually discusses scheduling/emailing someone.
        if out.len() < 2
            && (lower.contains("availability")
                || lower.contains("rsvp")
                || lower.contains("draft a reply")
                || lower.contains("waiting for your reply"))
        {
            let reply_co = self
                .snapshot
                .active_cards
                .iter()
                .find(|c| {
                    let co_low = c.company.to_ascii_lowercase();
                    !co_low.is_empty() && lower.contains(&co_low)
                })
                .map(|c| c.company.clone());
            let (label, prompt) = match reply_co {
                Some(co) => (
                    format!("Draft reply to {co}"),
                    format!(
                        "Draft a short reply to {co} in my voice (check Sent mail first; save as draft only)."
                    ),
                ),
                None => (
                    "Draft the reply email".into(),
                    "Draft the short reply email in my voice (check Sent mail first; save as draft only).".into(),
                ),
            };
            out.push((label, prompt));
        }

        // 4. Only suggest checking unconfirmed receipts when this reply itself brought them up!
        if out.len() < 2
            && self.snapshot.unresolved_attempts > 0
            && (lower.contains("outcome_unknown")
                || lower.contains("unconfirmed")
                || lower.contains("unverified")
                || lower.contains("receipt"))
            && !lower.contains("reconciled")
        {
            out.push((
                "Check email receipts for these".into(),
                "Check live Sent & inbox receipts for the unconfirmed submissions we just discussed and update their recorded outcomes.".into(),
            ));
        }

        out.truncate(2);
        out
    }

    fn chat_picker_items(&self) -> Vec<ChatPickerItem> {
        let raw = self.chat_input.as_str();
        // 1. Slash commands when input starts with `/` (before first space)
        if let Some(after_slash) = raw.strip_prefix('/')
            && !after_slash.contains(' ')
            && !after_slash.contains('\n')
        {
            let q = after_slash.to_ascii_lowercase();
            let cmds: [(&str, &str, &str); 6] = [
                (
                    "/digest",
                    "Today's priorities",
                    "What needs attention today? Check recent inbox & calendar changes.",
                ),
                (
                    "/brief",
                    "Interview brief",
                    "Prepare a 2-layer brief for my next interview.",
                ),
                (
                    "/drill",
                    "Mock prep drill",
                    "Run a 1-question-at-a-time mock prep drill for my next interview.",
                ),
                (
                    "/draft",
                    "Draft email reply",
                    "Draft a short reply email in my voice (check Gmail Sent first; save as draft only): ",
                ),
                (
                    "/reconcile",
                    "Check pending guards",
                    "Reconcile unknown submission outcomes against Gmail Sent.",
                ),
                ("/new", "Start fresh chat", "__CLEAR__"),
            ];
            return cmds
                .into_iter()
                .filter(|(k, d, _)| {
                    q.is_empty()
                        || k.to_ascii_lowercase().contains(&q)
                        || d.to_ascii_lowercase().contains(&q)
                })
                .map(|(k, d, p)| ChatPickerItem {
                    label: k.to_owned(),
                    meta: d.to_owned(),
                    insert_text: p.to_owned(),
                    role_attach: None,
                })
                .collect();
        }
        // 2. `@` mentions for active Roles & canonical Profile/Context docs
        let last_tok = raw.rsplit([' ', '\n']).next().unwrap_or("");
        if let Some(after_at) = last_tok.strip_prefix('@') {
            let q = after_at.to_ascii_lowercase();
            let mut items = Vec::new();
            for card in &self.snapshot.active_cards {
                if q.is_empty()
                    || card.company.to_ascii_lowercase().contains(&q)
                    || card.title.to_ascii_lowercase().contains(&q)
                {
                    items.push(ChatPickerItem {
                        label: format!("@{}", card.company),
                        meta: truncate_end(&card.title, 34),
                        insert_text: String::new(),
                        role_attach: Some((card.company.clone(), card.record_path.clone())),
                    });
                    if items.len() >= 5 {
                        break;
                    }
                }
            }
            for doc in &self.snapshot.profile_docs {
                if items.len() >= 7 {
                    break;
                }
                if q.is_empty()
                    || doc.label.to_ascii_lowercase().contains(&q)
                    || doc.relative_path.to_ascii_lowercase().contains(&q)
                {
                    items.push(ChatPickerItem {
                        label: format!("@{}", doc.label),
                        meta: truncate_label(&doc.relative_path, 26),
                        insert_text: format!("`{}` ", doc.relative_path),
                        role_attach: None,
                    });
                }
            }
            return items;
        }
        Vec::new()
    }

    fn apply_chat_picker_item(
        &mut self,
        insert_text: String,
        role_attach: Option<(String, String)>,
        cx: &mut Context<Self>,
    ) {
        self.chat_picker_idx = 0;
        if insert_text == "__CLEAR__" {
            self.chat_input.clear();
            self.chat_messages.clear();
            self.agent_opts.session_name =
                Some(format!("career-gui-{}", chrono::Utc::now().timestamp()));
            self.agent_opts.resume_existing = false;
            self.resumed_session_label = None;
            self.quoted_reply = None;
            self.chat_attachments.clear();
            self.pasted_snippets.clear();
            self.text_selection = None;
            self.chat_ruler_hover = None;
            self.chat_follow = true;
            cx.notify();
            return;
        }
        if self.chat_input.starts_with('/') {
            if insert_text.ends_with(' ') {
                self.chat_input = insert_text;
            } else {
                self.chat_input.clear();
                self.submit_prompt(insert_text, false, cx);
                return;
            }
        } else if let Some(at_pos) = self.chat_input.rfind('@') {
            self.chat_input.truncate(at_pos);
            if let Some(role) = role_attach {
                self.attached_role = Some(role);
            } else {
                self.chat_input.push_str(&insert_text);
            }
        }
        self.active_input = ActiveInput::Chat;
        cx.notify();
    }

    fn render_chat_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let input_active = self.active_input == ActiveInput::Chat;
        let agent_pop_open = self.active_popover == ActivePopover::Agent;
        let model_badge = self.active_model_badge();
        let placeholder = if self.streaming {
            "Send a follow-up…".to_owned()
        } else if let Some((company, _)) = &self.attached_role {
            format!("Ask about {company}…")
        } else {
            "Ask anything (@ roles, / commands)".to_owned()
        };
        let on_link = self.md_link_handler(cx);
        let on_select = self.md_select_handler(cx);
        let active_sel = self.active_text_selection();

        let is_full = self.chat_fullscreen;
        let show_ruler = (is_full || self.chat_width >= 420.0)
            && self.chat_messages.iter().filter(|m| m.is_user).count() >= 2;
        let inner_composer_w = if is_full {
            728.0
        } else {
            (self.chat_width - 24.0).max(200.0)
        };
        div()
            .id("chat-rail-drop")
            .drag_over::<ExternalPaths>(|s, _, _, _| {
                s.bg(theme::lavender_wash())
                    .border_color(theme::lavender_border())
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                let mut added = 0usize;
                for p in paths.paths() {
                    if p.is_file() && !this.chat_attachments.contains(p) {
                        this.chat_attachments.push(p.clone());
                        added += 1;
                    }
                }
                if added > 0 {
                    this.active_input = ActiveInput::Chat;
                    this.set_toast(format!("Attached {added} file(s)"), cx);
                }
            }))
            .relative()
            .when(is_full, |d| d.w_full())
            .when(!is_full, |d| d.w(px(self.chat_width)))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(theme::panel_glass())
            .when(!is_full, |d| d.border_r_1().border_color(theme::hairline()))
            .overflow_hidden()
            // Clean borderless header: clickable Agent & Model pill + subtle Clear (NO permanent ↓)
            .child(
                div()
                    .px_4()
                    .pt_3()
                    .pb_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(
                                div()
                                    .id("chat-agent-picker-btn")
                                    .px_2()
                                    .py_1()
                                    .mx(px(-6.0))
                                    .rounded(theme::r_sm())
                                    .when(agent_pop_open, |d| d.bg(theme::surface_active()))
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.active_popover = if this.active_popover
                                            == ActivePopover::Agent
                                        {
                                            ActivePopover::None
                                        } else {
                                            this.cmd_anim_epoch =
                                                this.cmd_anim_epoch.wrapping_add(1);
                                            this.model_draft =
                                                this.agent_opts.model.clone().unwrap_or_default();
                                            ActivePopover::Agent
                                        };
                                        cx.notify();
                                    }))
                                    .child(
                                        div()
                                            .w(px(6.0))
                                            .h(px(6.0))
                                            .flex_shrink_0()
                                            .rounded(px(99.0))
                                            .bg(if self.streaming {
                                                theme::lavender()
                                            } else if self.runtime.is_ready() {
                                                theme::emerald()
                                            } else {
                                                theme::amber_accent()
                                            }),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_xs()
                                            .text_color(theme::text_primary())
                                            .child(self.runtime.short_name().to_owned()),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .child(format!("{model_badge} ▾")),
                                    ),
                            )
                            .when(self.runtime.supports_fast_mode(&self.agent_opts), |r| {
                                let fast_on = self.agent_opts.fast_mode;
                                r.child(
                                    div()
                                        .id("chat-fast-toggle")
                                        .ml_1()
                                        .px(px(6.0))
                                        .py(px(3.0))
                                        .rounded(theme::r_sm())
                                        .flex_shrink_0()
                                        .bg(if fast_on {
                                            theme::amber_wash()
                                        } else {
                                            gpui::rgba(0x00000000)
                                        })
                                        .border_1()
                                        .border_color(if fast_on {
                                            gpui::rgba(0xfacc1540)
                                        } else {
                                            theme::hairline()
                                        })
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .gap(px(3.0))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let next = !this.agent_opts.fast_mode;
                                            this.agent_opts.fast_mode = next;
                                            this.runtime.sync_fast_mode(next);
                                            this.set_toast(
                                                if next {
                                                    "Fast mode enabled (priority tier)"
                                                } else {
                                                    "Standard speed restored"
                                                },
                                                cx,
                                            );
                                        }))
                                        .child(
                                            svg().path("icons/zap.svg").size(px(11.0)).text_color(
                                                if fast_on {
                                                    theme::amber_text()
                                                } else {
                                                    theme::text_muted()
                                                },
                                            ),
                                        )
                                        .when(is_full || self.chat_width >= 340.0, |b| {
                                            b.child(
                                                div()
                                                    .text_size(px(10.5))
                                                    .font_weight(if fast_on {
                                                        FontWeight::MEDIUM
                                                    } else {
                                                        FontWeight::NORMAL
                                                    })
                                                    .text_color(if fast_on {
                                                        theme::amber_text()
                                                    } else {
                                                        theme::text_muted()
                                                    })
                                                    .child("Fast"),
                                            )
                                        }),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when(self.runtime.supports_sessions() && !self.streaming, |d| {
                                let sess_open = self.active_popover == ActivePopover::Sessions;
                                let resuming = self.resumed_session_label.is_some();
                                d.child(
                                    div()
                                        .id("chat-sessions-btn")
                                        .w(px(26.0))
                                        .h(px(26.0))
                                        .rounded(theme::r_sm())
                                        .when(sess_open || resuming, |b| {
                                            b.bg(theme::surface_active())
                                        })
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if this.active_popover == ActivePopover::Sessions {
                                                this.active_popover = ActivePopover::None;
                                                cx.notify();
                                                return;
                                            }
                                            this.cmd_anim_epoch =
                                                this.cmd_anim_epoch.wrapping_add(1);
                                            this.active_popover = ActivePopover::Sessions;
                                            cx.notify();
                                            let rt = this.runtime.clone();
                                            let ws =
                                                this.client.target.local_root().map(PathBuf::from);
                                            cx.spawn(async move |this, cx| {
                                                let list = cx
                                                    .background_executor()
                                                    .spawn(async move {
                                                        rt.list_recent_sessions(ws.as_deref())
                                                    })
                                                    .await;
                                                let _ = this.update(cx, |app, cx| {
                                                    app.host_sessions = list;
                                                    cx.notify();
                                                });
                                            })
                                            .detach();
                                        }))
                                        .child(
                                            svg()
                                                .path("icons/history.svg")
                                                .size(px(13.5))
                                                .text_color(if sess_open || resuming {
                                                    theme::text_primary()
                                                } else {
                                                    theme::text_muted()
                                                }),
                                        ),
                                )
                            })
                            .when(!self.chat_messages.is_empty(), |d| {
                                let copied = self.is_copied_flash("chat-copy-transcript");
                                d.child(
                                    div()
                                        .id("chat-copy-transcript")
                                        .w(px(26.0))
                                        .h(px(26.0))
                                        .rounded(theme::r_sm())
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .on_click(cx.listener(
                                            |this, ev: &gpui::ClickEvent, _, cx| {
                                                if ev.modifiers().shift {
                                                    this.export_chat_transcript(cx);
                                                } else {
                                                    this.copy_chat_transcript(cx);
                                                }
                                            },
                                        ))
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(|this, _, _, cx| {
                                                this.export_chat_transcript(cx);
                                            }),
                                        )
                                        .child(
                                            svg()
                                                .path(if copied {
                                                    "icons/check.svg"
                                                } else {
                                                    "icons/copy.svg"
                                                })
                                                .size(px(13.0))
                                                .text_color(if copied {
                                                    theme::emerald()
                                                } else {
                                                    theme::text_muted()
                                                }),
                                        ),
                                )
                            })
                            .when(!self.chat_messages.is_empty() && !self.streaming, |d| {
                                d.child(
                                    div()
                                        .id("chat-clear")
                                        .w(px(26.0))
                                        .h(px(26.0))
                                        .rounded(theme::r_sm())
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.chat_messages.clear();
                                            this.agent_opts.session_name = Some(format!(
                                                "career-gui-{}",
                                                chrono::Utc::now().timestamp()
                                            ));
                                            this.agent_opts.resume_existing = false;
                                            this.resumed_session_label = None;
                                            this.quoted_reply = None;
                                            this.chat_attachments.clear();
                                            this.pasted_snippets.clear();
                                            this.text_selection = None;
                                            this.chat_ruler_hover = None;
                                            this.chat_follow = true;
                                            cx.notify();
                                        }))
                                        .child(
                                            svg()
                                                .path("icons/square-pen.svg")
                                                .size(px(13.5))
                                                .text_color(theme::text_muted()),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .id("chat-expand-fullscreen")
                                    .w(px(26.0))
                                    .h(px(26.0))
                                    .rounded(theme::r_sm())
                                    .when(is_full, |d| d.bg(theme::surface_active()))
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.chat_fullscreen = !this.chat_fullscreen;
                                        this.chat_resized = true;
                                        cx.notify();
                                    }))
                                    .child(
                                        svg()
                                            .path(if is_full {
                                                "icons/minimize.svg"
                                            } else {
                                                "icons/expand.svg"
                                            })
                                            .size(px(13.5))
                                            .text_color(if is_full {
                                                theme::text_primary()
                                            } else {
                                                theme::text_muted()
                                            }),
                                    ),
                            ),
                    ),
            )
            // Quiet strip when continuing an existing host runtime session
            .when_some(self.resumed_session_label.clone(), |d, lbl| {
                d.child(
                    div()
                        .mx_4()
                        .mb_1()
                        .px(px(8.0))
                        .py(px(4.0))
                        .rounded(theme::r_sm())
                        .bg(theme::surface_subtle())
                        .border_1()
                        .border_color(theme::hairline())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_size(px(11.0))
                                .text_color(theme::text_secondary())
                                .child(format!("Resuming: {}", truncate_end(&lbl, 34))),
                        )
                        .child(
                            div()
                                .id("chat-exit-resumed-session")
                                .flex_shrink_0()
                                .px(px(5.0))
                                .py(px(1.5))
                                .rounded(theme::r_xs())
                                .hover(|s| s.bg(theme::row_hover()))
                                .cursor_pointer()
                                .text_size(px(10.5))
                                .text_color(theme::text_muted())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.chat_messages.clear();
                                    this.agent_opts.session_name = Some(format!(
                                        "career-gui-{}",
                                        chrono::Utc::now().timestamp()
                                    ));
                                    this.agent_opts.resume_existing = false;
                                    this.resumed_session_label = None;
                                    cx.notify();
                                }))
                                .child("Fresh"),
                        ),
                )
            })
            // Scrollable message stream wrapped in `.relative()` so jump-to-bottom floats cleanly above the composer
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("chat-scroll")
                            .track_scroll(&self.chat_scroll)
                            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                                let dy: f32 = ev.delta.pixel_delta(px(20.0)).y.into();
                                if dy > 1.0 {
                                    this.chat_follow = false;
                                    cx.notify();
                                } else if dy < -1.0 {
                                    let cur_y: f32 = this.chat_scroll.offset().y.into();
                                    let max_y: f32 = this.chat_scroll.max_offset().height.into();
                                    if max_y <= 0.0 || (cur_y + max_y).abs() < 28.0 {
                                        this.chat_follow = true;
                                        cx.notify();
                                    }
                                }
                                cx.stop_propagation();
                            }))
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .pl(px(if show_ruler && !is_full { 30.0 } else { 16.0 }))
                            .pr_4()
                            .pt_3()
                            .pb(px(36.0))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .when(self.chat_messages.is_empty(), |d| {
                                d.child(
                                    div()
                                        .when(is_full, |x| x.w(px(728.0)).mx_auto())
                                        .pt_2()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .pb_1()
                                                .text_size(px(12.0))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme::text_primary())
                                                .child("Suggested"),
                                        )
                                        .children(
                                            [
                                                "What needs attention today?",
                                                "Catch me up on recent inbox & calendar changes",
                                                "Prepare a 2-layer brief for my next interview",
                                            ]
                                            .into_iter()
                                            .enumerate()
                                            .map(
                                                |(i, prompt)| {
                                                    let p = prompt.to_owned();
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "chat-starter-{i}"
                                                        )))
                                                        .px_2()
                                                        .py(px(6.0))
                                                        .mx(px(-6.0))
                                                        .rounded(theme::r_sm())
                                                        .hover(|s| s.bg(theme::row_hover()))
                                                        .cursor_pointer()
                                                        .flex()
                                                        .items_start()
                                                        .gap_2()
                                                        .text_xs()
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.submit_prompt(
                                                                    p.clone(),
                                                                    false,
                                                                    cx,
                                                                );
                                                            },
                                                        ))
                                                        .child(
                                                            div()
                                                                .text_color(theme::lavender())
                                                                .child("→"),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .text_color(theme::text_secondary())
                                                                .child(prompt),
                                                        )
                                                },
                                            ),
                                        ),
                                )
                            })
                            .children(self.chat_messages.iter().enumerate().map(|(idx, msg)| {
                                let is_last = idx + 1 == self.chat_messages.len();
                                let shown = if msg.is_user {
                                    msg.text.as_str()
                                } else {
                                    let end = msg.revealed.min(msg.text.len());
                                    &msg.text[..end]
                                };
                                let actively_writing = !msg.is_user
                                    && is_last
                                    && (self.streaming || msg.revealed < msg.text.len());
                                let elapsed_s = msg
                                    .duration_ms
                                    .map(|ms| ms as f32 / 1000.0)
                                    .unwrap_or_else(|| msg.started_at.elapsed().as_secs_f32());
                                let has_trace = !msg.steps.is_empty() || !msg.thinking.is_empty();
                                let copy_text = msg.text.clone();
                                let reply_text = msg.text.clone();
                                let expand_text = msg.text.clone();
                                let prev_user_prompt = if !msg.is_user && idx > 0 {
                                    self.chat_messages
                                        .get(idx - 1)
                                        .filter(|m| m.is_user)
                                        .map(|m| m.text.clone())
                                } else {
                                    None
                                };
                                let has_model_override = self.agent_opts.model.is_some()
                                    || self.agent_opts.provider.is_some();
                                let group_name = SharedString::from(format!("chat-msg-grp-{idx}"));

                                let mut block = div()
                                    .group(group_name.clone())
                                    .when(is_full, |d| d.w(px(728.0)).mx_auto())
                                    .when(!is_full, |d| d.w_full())
                                    .when(msg.is_user && idx > 0, |d| d.pt_3())
                                    .flex()
                                    .flex_col()
                                    .gap_1();

                                // Live / collapsible inline Activity Trace with tappable steps + real token count
                                if !msg.is_user && (actively_writing || has_trace) {
                                    let step_count = msg.steps.len();
                                    let tok_suffix = msg
                                        .total_tokens
                                        .map(|t| format!("  {}", format_tokens(t)))
                                        .unwrap_or_default();
                                    let header_label = if actively_writing {
                                        let base = if self.stream_status.is_empty() {
                                            "Thinking…".to_owned()
                                        } else {
                                            self.stream_status.clone()
                                        };
                                        format!("{base}  {:.0}s", elapsed_s.max(0.0))
                                    } else if step_count > 0 {
                                        let noun = if step_count == 1 { "step" } else { "steps" };
                                        format!("{step_count} {noun}  {elapsed_s:.1}s{tok_suffix}")
                                    } else {
                                        format!("Thought  {elapsed_s:.1}s{tok_suffix}")
                                    };

                                    let dot = div().w(px(6.0)).h(px(6.0)).rounded(px(99.0)).bg(
                                        if actively_writing {
                                            theme::lavender()
                                        } else {
                                            theme::emerald()
                                        },
                                    );
                                    let header_row = div()
                                        .id(SharedString::from(format!("trace-tog-{idx}")))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Some(m) = this.chat_messages.get_mut(idx) {
                                                m.steps_expanded = !m.steps_expanded;
                                                cx.notify();
                                            }
                                        }))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .child(dot)
                                                .when(msg.fast_mode, |r| {
                                                    r.child(
                                                            div()
                                                                .px(px(5.0))
                                                                .py(px(1.0))
                                                                .rounded(theme::r_xs())
                                                                .bg(theme::amber_wash())
                                                                .border_1()
                                                                .border_color(gpui::rgba(
                                                                    0xfacc1538,
                                                                ))
                                                                .flex_shrink_0()
                                                                .flex()
                                                                .items_center()
                                                                .gap(px(3.0))
                                                                .child(
                                                                    svg()
                                                                        .path("icons/zap.svg")
                                                                        .size(px(10.0))
                                                                        .text_color(
                                                                            theme::amber_text(),
                                                                        ),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_size(px(10.0))
                                                                        .font_weight(
                                                                            FontWeight::MEDIUM,
                                                                        )
                                                                        .text_color(
                                                                            theme::amber_text(),
                                                                        )
                                                                        .child("Fast"),
                                                                ),
                                                        )
                                                })
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .overflow_hidden()
                                                        .text_xs()
                                                        .text_color(if actively_writing {
                                                            theme::text_secondary()
                                                        } else {
                                                            theme::text_muted()
                                                        })
                                                        .child(header_label),
                                                ),
                                        )
                                        .when(has_trace, |r| {
                                            r.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme::text_muted())
                                                    .child(if msg.steps_expanded {
                                                        "▾"
                                                    } else {
                                                        "▸"
                                                    }),
                                            )
                                        });

                                    let header_el = if actively_writing && !self.reduce_motion {
                                        header_row
                                            .with_animation(
                                                SharedString::from(format!("think-pulse-{idx}")),
                                                Animation::new(Duration::from_millis(900))
                                                    .repeat()
                                                    .with_easing(ease_out_quint()),
                                                |el, t| {
                                                    let wave =
                                                        0.6 + 0.4 * (1.0 - (2.0 * t - 1.0).abs());
                                                    el.opacity(wave)
                                                },
                                            )
                                            .into_any_element()
                                    } else {
                                        header_row.into_any_element()
                                    };

                                    let mut trace_box =
                                        div().flex().flex_col().gap_1().child(header_el);

                                    if msg.steps_expanded && has_trace {
                                        let mut inner = div()
                                            .pl_3()
                                            .ml(px(2.0))
                                            .border_l_1()
                                            .border_color(theme::hairline())
                                            .flex()
                                            .flex_col()
                                            .gap_1();
                                        if !msg.thinking.trim().is_empty() {
                                            inner = inner.child(
                                                div()
                                                    .flex()
                                                    .items_start()
                                                    .gap_2()
                                                    .py(px(2.0))
                                                    .child(
                                                        div().pt(px(2.0)).flex_shrink_0().child(
                                                            svg()
                                                                .path("icons/brain.svg")
                                                                .size(px(12.5))
                                                                .text_color(theme::text_muted()),
                                                        ),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .text_xs()
                                                            .line_height(px(17.0))
                                                            .text_color(theme::text_muted())
                                                            .child(truncate_end(
                                                                msg.thinking.trim(),
                                                                220,
                                                            )),
                                                    ),
                                            );
                                        }
                                        for (si, step) in msg.steps.iter().enumerate() {
                                            let step_icon =
                                                tool_step_icon(&step.name, &step.detail);
                                            let icon_col = if !step.done {
                                                theme::lavender()
                                            } else if step.ok {
                                                theme::text_muted()
                                            } else {
                                                theme::rose_text()
                                            };
                                            let dur_tag = step
                                                .duration_ms
                                                .map(|ms| format!("{:.1}s", ms as f32 / 1000.0));
                                            let detail_cap = if is_full { 60 } else { 24 };
                                            let line_txt = if step.detail.is_empty() {
                                                step.name.clone()
                                            } else {
                                                format!(
                                                    "{}  {}",
                                                    step.name,
                                                    truncate_end(&step.detail, detail_cap)
                                                )
                                            };
                                            let can_expand =
                                                !step.output.is_empty() || !step.detail.is_empty();
                                            let mut step_wrap =
                                                div().flex().flex_col().gap_1().child(
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "step-row-{idx}-{si}"
                                                        )))
                                                        .px_1()
                                                        .mx(px(-4.0))
                                                        .py(px(3.0))
                                                        .rounded(theme::r_xs())
                                                        .when(can_expand, |d| {
                                                            d.hover(|s| s.bg(theme::row_hover()))
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        if let Some(m) = this
                                                                            .chat_messages
                                                                            .get_mut(idx)
                                                                            && let Some(st) =
                                                                                m.steps.get_mut(si)
                                                                        {
                                                                            st.expanded =
                                                                                !st.expanded;
                                                                            cx.notify();
                                                                        }
                                                                    },
                                                                ))
                                                        })
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .gap_2()
                                                        .text_xs()
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .overflow_hidden()
                                                                .flex()
                                                                .items_center()
                                                                .gap_2()
                                                                .child(
                                                                    svg()
                                                                        .path(step_icon)
                                                                        .size(px(12.5))
                                                                        .flex_shrink_0()
                                                                        .text_color(icon_col),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .flex_1()
                                                                        .min_w_0()
                                                                        .overflow_hidden()
                                                                        .text_color(if !step.done {
                                                                            theme::text_primary()
                                                                        } else {
                                                                            theme::text_muted()
                                                                        })
                                                                        .child(line_txt),
                                                                ),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex_shrink_0()
                                                                .flex()
                                                                .items_center()
                                                                .gap_1()
                                                                .when_some(dur_tag, |r, d| {
                                                                    r.child(
                                                                        div()
                                                                            .text_color(
                                                                                theme::text_muted(),
                                                                            )
                                                                            .child(d),
                                                                    )
                                                                })
                                                                .when(can_expand, |r| {
                                                                    r.child(
                                                                        div()
                                                                            .text_color(
                                                                                theme::text_muted(),
                                                                            )
                                                                            .child(
                                                                                if step.expanded {
                                                                                    "▾"
                                                                                } else {
                                                                                    "›"
                                                                                },
                                                                            ),
                                                                    )
                                                                }),
                                                        ),
                                                );
                                            if step.expanded && can_expand {
                                                let raw_out = if !step.output.is_empty() {
                                                    step.output.as_str()
                                                } else {
                                                    step.detail.as_str()
                                                };
                                                let trimmed_out = raw_out.trim();
                                                let is_json_out = trimmed_out.starts_with('{')
                                                    || trimmed_out.starts_with('[');
                                                if is_json_out {
                                                    let step_code_id = SharedString::from(format!(
                                                        "step-json-{idx}-{si}"
                                                    ));
                                                    let step_text = truncate_end(trimmed_out, 600);
                                                    let step_sel =
                                                        active_sel.as_ref().and_then(|s| {
                                                            s.range_for_block(
                                                                step_code_id.as_ref(),
                                                                0,
                                                                step_text.len(),
                                                            )
                                                        });
                                                    let blocks_rc =
                                                        Rc::new(std::cell::RefCell::new(vec![
                                                            step_text.clone(),
                                                        ]));
                                                    step_wrap = step_wrap.child(
                                                        markdown::render_syntax_code(
                                                            step_code_id.clone(),
                                                            step_code_id,
                                                            0,
                                                            blocks_rc,
                                                            &step_text,
                                                            "json",
                                                            false,
                                                            step_sel,
                                                            on_select.clone(),
                                                        ),
                                                    );
                                                } else {
                                                    step_wrap = step_wrap.child(
                                                        div()
                                                            .w_full()
                                                            .min_w_0()
                                                            .overflow_hidden()
                                                            .p_2()
                                                            .rounded(theme::r_sm())
                                                            .bg(theme::window_glass())
                                                            .border_1()
                                                            .border_color(theme::hairline())
                                                            .flex()
                                                            .flex_col()
                                                            .gap(px(2.0))
                                                            .children(
                                                                trimmed_out.lines().take(8).map(
                                                                    |ln| {
                                                                        let col = if ln
                                                                            .starts_with('✓')
                                                                        {
                                                                            theme::emerald()
                                                                        } else if ln
                                                                            .starts_with('✕')
                                                                            || ln.starts_with(
                                                                                "Error:",
                                                                            )
                                                                        {
                                                                            theme::rose_text()
                                                                        } else {
                                                                            theme::text_secondary()
                                                                        };
                                                                        div()
                                                                            .text_size(px(11.0))
                                                                            .line_height(px(16.0))
                                                                            .text_color(col)
                                                                            .child(truncate_end(
                                                                                ln, 64,
                                                                            ))
                                                                    },
                                                                ),
                                                            ),
                                                    );
                                                }
                                            }
                                            inner = inner.child(step_wrap);
                                        }
                                        trace_box = trace_box.child(inner);
                                    }
                                    block = block.child(trace_box);
                                }

                                // Message content body: subtle right-aligned user prompt vs left-aligned assistant prose (T3 Code rhythm!)
                                if msg.is_user {
                                    let copy_u = copy_text.clone();
                                    let reply_u = reply_text.clone();
                                    let user_max_w = if is_full {
                                        560.0
                                    } else {
                                        (self.chat_width - 40.0).max(160.0)
                                    };
                                    let mut bubble = div()
                                        .relative()
                                        .flex_shrink_0()
                                        .max_w(px(user_max_w))
                                        .min_w_0()
                                        .overflow_hidden()
                                        .px_3()
                                        .py_2()
                                        .rounded(theme::r_lg())
                                        .bg(theme::surface_subtle())
                                        .border_1()
                                        .border_color(theme::hairline())
                                        .flex()
                                        .flex_col()
                                        .gap_2();
                                    if !msg.attachments.is_empty() {
                                        bubble = bubble.child(
                                            div().flex().flex_wrap().gap_1().children(
                                                msg.attachments.iter().map(|p| {
                                                    if is_image_path(p) {
                                                        img(p.clone())
                                                            .size(px(44.0))
                                                            .rounded(theme::r_sm())
                                                            .into_any_element()
                                                    } else {
                                                        let name = p
                                                            .file_name()
                                                            .and_then(|s| s.to_str())
                                                            .unwrap_or("file");
                                                        div()
                                                            .px_2()
                                                            .py(px(2.0))
                                                            .rounded(theme::r_xs())
                                                            .bg(theme::surface_active())
                                                            .text_xs()
                                                            .text_color(theme::text_secondary())
                                                            .child(truncate_label(name, 20))
                                                            .into_any_element()
                                                    }
                                                }),
                                            ),
                                        );
                                    }
                                    bubble = bubble.child(markdown::render_markdown(
                                        &format!("chat-user-{idx}"),
                                        &msg.text,
                                        false,
                                        active_sel.as_ref(),
                                        on_link.clone(),
                                        on_select.clone(),
                                    ));
                                    let hover_tools = div()
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .opacity(0.0)
                                        .group_hover(group_name.clone(), |s| s.opacity(1.0))
                                        .child(
                                            div()
                                                .text_size(px(10.5))
                                                .text_color(theme::text_muted())
                                                .child(msg.sent_label.clone()),
                                        )
                                        .child({
                                            let u_tag = format!("umsg-copy-{idx}");
                                            let copied = self.is_copied_flash(&u_tag);
                                            div()
                                                .id(SharedString::from(u_tag.clone()))
                                                .p(px(3.0))
                                                .rounded(theme::r_xs())
                                                .cursor_pointer()
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(copy_u.clone()),
                                                    );
                                                    this.flash_copied(u_tag.clone(), cx);
                                                    this.set_toast("Copied message", cx);
                                                }))
                                                .child(
                                                    svg()
                                                        .path(if copied {
                                                            "icons/check.svg"
                                                        } else {
                                                            "icons/copy.svg"
                                                        })
                                                        .size(px(12.0))
                                                        .text_color(if copied {
                                                            theme::emerald()
                                                        } else {
                                                            theme::text_muted()
                                                        }),
                                                )
                                        })
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("umsg-reply-{idx}")))
                                                .p(px(3.0))
                                                .rounded(theme::r_xs())
                                                .cursor_pointer()
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.quoted_reply = Some(reply_u.clone());
                                                    this.active_input = ActiveInput::Chat;
                                                    cx.notify();
                                                }))
                                                .child(
                                                    svg()
                                                        .path("icons/reply.svg")
                                                        .size(px(12.0))
                                                        .text_color(theme::text_muted()),
                                                ),
                                        );
                                    // Stack hover toolbar cleanly below the bubble so no horizontal sibling ever steals bubble width!
                                    block = block.child(
                                        div()
                                            .w_full()
                                            .flex()
                                            .flex_col()
                                            .items_end()
                                            .gap(px(2.0))
                                            .child(bubble)
                                            .child(hover_tools),
                                    );
                                } else if !shown.is_empty() {
                                    let display_str = if actively_writing {
                                        format!("{shown} ▋")
                                    } else {
                                        shown.to_owned()
                                    };
                                    block = block.child(markdown::render_markdown(
                                        &format!("chat-md-{idx}"),
                                        &display_str,
                                        false,
                                        active_sel.as_ref(),
                                        on_link.clone(),
                                        on_select.clone(),
                                    ));
                                }

                                // Deduplicated Sources tie-in strip at the end of assistant messages
                                if !msg.citations.is_empty() && !actively_writing {
                                    let links = div()
                                        .pt_1()
                                        .flex()
                                        .flex_wrap()
                                        .items_center()
                                        .gap_1()
                                        .children(msg.citations.iter().enumerate().map(
                                            |(ci, path)| {
                                                let p = path.clone();
                                                let short = truncate_label(
                                                    path.rsplit('/').next().unwrap_or(path),
                                                    24,
                                                );
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "cite-{idx}-{ci}"
                                                    )))
                                                    .px_2()
                                                    .py(px(2.0))
                                                    .rounded(theme::r_sm())
                                                    .bg(theme::surface_subtle())
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .text_xs()
                                                    .text_color(theme::lavender())
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.open_citation(&p, cx);
                                                    }))
                                                    .child(format!("↗ {short}"))
                                            },
                                        ));
                                    block = block.child(links);
                                }

                                // T3 Code hover-reveal message footer on assistant turns
                                if !msg.is_user && !msg.text.is_empty() && !actively_writing {
                                    let mut icon_bar = div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child({
                                            let m_tag = format!("msg-copy-{idx}");
                                            let copied = self.is_copied_flash(&m_tag);
                                            div()
                                                .id(SharedString::from(m_tag.clone()))
                                                .p(px(4.0))
                                                .rounded(theme::r_xs())
                                                .cursor_pointer()
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(
                                                            copy_text.clone(),
                                                        ),
                                                    );
                                                    this.flash_copied(m_tag.clone(), cx);
                                                    this.set_toast("Copied message", cx);
                                                }))
                                                .child(
                                                    svg()
                                                        .path(if copied {
                                                            "icons/check.svg"
                                                        } else {
                                                            "icons/copy.svg"
                                                        })
                                                        .size(px(13.0))
                                                        .text_color(if copied {
                                                            theme::emerald()
                                                        } else {
                                                            theme::text_muted()
                                                        }),
                                                )
                                        })
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("msg-reply-{idx}")))
                                                .p(px(4.0))
                                                .rounded(theme::r_xs())
                                                .cursor_pointer()
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.quoted_reply = Some(reply_text.clone());
                                                    this.active_input = ActiveInput::Chat;
                                                    cx.notify();
                                                }))
                                                .child(
                                                    svg()
                                                        .path("icons/reply.svg")
                                                        .size(px(13.0))
                                                        .text_color(theme::text_muted()),
                                                ),
                                        );
                                    if !msg.is_user {
                                        icon_bar = icon_bar.child(
                                            div()
                                                .id(SharedString::from(format!("msg-expand-{idx}")))
                                                .p(px(4.0))
                                                .rounded(theme::r_xs())
                                                .cursor_pointer()
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.expanded_reader = Some((
                                                        format!(
                                                            "{} reply",
                                                            this.runtime.short_name()
                                                        ),
                                                        expand_text.clone(),
                                                    ));
                                                    cx.notify();
                                                }))
                                                .child(
                                                    svg()
                                                        .path("icons/expand.svg")
                                                        .size(px(13.0))
                                                        .text_color(theme::text_muted()),
                                                ),
                                        );
                                    }
                                    let footer = div()
                                        .mt(px(-2.0))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(div().flex().items_center().gap_2().when(
                                            msg.is_error
                                                && has_model_override
                                                && prev_user_prompt.is_some(),
                                            |r| {
                                                let retry_p =
                                                    prev_user_prompt.clone().unwrap_or_default();
                                                r.child(
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "msg-reset-{idx}"
                                                        )))
                                                        .cursor_pointer()
                                                        .text_xs()
                                                        .text_color(theme::lavender())
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.agent_opts.model = None;
                                                                this.agent_opts.provider = None;
                                                                this.model_draft.clear();
                                                                this.submit_prompt(
                                                                    retry_p.clone(),
                                                                    false,
                                                                    cx,
                                                                );
                                                            },
                                                        ))
                                                        .child("Reset model & retry"),
                                                )
                                            },
                                        ))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .opacity(0.0)
                                                .group_hover(group_name.clone(), |s| s.opacity(1.0))
                                                .child(
                                                    div()
                                                        .text_size(px(11.0))
                                                        .text_color(theme::text_muted())
                                                        .child(msg.sent_label.clone()),
                                                )
                                                .child(icon_bar),
                                        );
                                    block = block.child(footer);
                                }

                                // Emma-style contextual follow-up suggestions (ONLY on newest completed assistant turn)
                                if !msg.is_user
                                    && is_last
                                    && !actively_writing
                                    && !msg.text.is_empty()
                                    && !msg.is_error
                                {
                                    let followups = self.suggest_chat_followups(msg);
                                    if !followups.is_empty() {
                                        block = block.child(
                                            div().pt_1().flex().flex_col().gap_1().children(
                                                followups.into_iter().enumerate().map(
                                                    |(fi, (label, prompt))| {
                                                        div()
                                                            .id(SharedString::from(format!(
                                                                "fup-{idx}-{fi}"
                                                            )))
                                                            .px_2()
                                                            .py(px(4.0))
                                                            .mx(px(-6.0))
                                                            .rounded(theme::r_sm())
                                                            .hover(|s| s.bg(theme::row_hover()))
                                                            .cursor_pointer()
                                                            .text_xs()
                                                            .text_color(theme::text_muted())
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.submit_prompt(
                                                                        prompt.clone(),
                                                                        false,
                                                                        cx,
                                                                    );
                                                                },
                                                            ))
                                                            .child(format!("↳  {label}"))
                                                    },
                                                ),
                                            ),
                                        );
                                    }
                                }

                                block
                            })),
                    )
                    // T3 Code left-edge Prompt Ruler / Minimap with Gaussian neighbor magnification & tick-anchored preview!
                    .when(show_ruler, |d| {
                        let turns: Vec<(usize, String, String)> = self
                            .chat_messages
                            .iter()
                            .enumerate()
                            .filter(|(_, m)| m.is_user)
                            .map(|(mi, m)| {
                                let reply = self
                                    .chat_messages
                                    .get(mi + 1)
                                    .filter(|r| !r.is_user)
                                    .map(|r| r.text.clone())
                                    .unwrap_or_default();
                                (mi, m.text.clone(), reply)
                            })
                            .collect();
                        let hovered_ti = self
                            .chat_ruler_hover
                            .and_then(|hi| turns.iter().position(|(mi, _, _)| *mi == hi));
                        let total_msgs = self.chat_messages.len();
                        let ruler_left = if is_full { 14.0 } else { 6.0 };
                        let preview_w = if is_full {
                            280.0
                        } else {
                            (self.chat_width - 48.0).clamp(200.0, 270.0)
                        };
                        let reduce_motion = self.reduce_motion;
                        d.child(
                            div()
                                .id("chat-prompt-ruler")
                                .absolute()
                                .left(px(ruler_left))
                                .top_4()
                                .bottom_4()
                                .w(px(24.0))
                                .flex()
                                .flex_col()
                                .justify_center()
                                .gap(px(4.0))
                                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                    if !*hovered && this.chat_ruler_hover.is_some() {
                                        this.chat_ruler_hover = None;
                                        cx.notify();
                                    }
                                }))
                                .children(turns.into_iter().enumerate().map(
                                    |(ti, (msg_idx, prompt_txt, reply_txt))| {
                                        let dist = hovered_ti.map(|h| ti.abs_diff(h));
                                        let is_hov = dist == Some(0);
                                        // Smooth macOS-Dock / T3 Code Gaussian wave magnification!
                                        let tick_w = match dist {
                                            Some(0) => 20.0,
                                            Some(1) => 13.0,
                                            Some(2) => 9.5,
                                            _ => 6.5,
                                        };
                                        let tick_col = match dist {
                                            Some(0) => theme::text_primary(),
                                            Some(1) => theme::text_secondary(),
                                            Some(2) => theme::text_muted(),
                                            _ => theme::hairline_strong(),
                                        };
                                        let clean_reply = reply_txt
                                            .replace("**", "")
                                            .replace("__", "")
                                            .replace('`', "")
                                            .replace('\n', " ");
                                        let mut tick_row = div()
                                            .id(SharedString::from(format!("ruler-tick-{ti}")))
                                            .relative()
                                            .w(px(24.0))
                                            .py(px(3.5))
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .on_hover(cx.listener(move |this, h: &bool, _, cx| {
                                                if *h && this.chat_ruler_hover != Some(msg_idx) {
                                                    this.chat_ruler_hover = Some(msg_idx);
                                                    cx.notify();
                                                }
                                            }))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.chat_follow = msg_idx + 2 >= total_msgs;
                                                this.chat_scroll.scroll_to_item(msg_idx);
                                                this.chat_ruler_hover = None;
                                                cx.notify();
                                            }))
                                            .child(
                                                div()
                                                    .h(px(if is_hov { 2.0 } else { 1.5 }))
                                                    .w(px(tick_w))
                                                    .rounded(px(1.0))
                                                    .bg(tick_col),
                                            );
                                        if is_hov {
                                            let card = div()
                                                .absolute()
                                                .left(px(28.0))
                                                .top(px(-26.0))
                                                .w(px(preview_w))
                                                .p_3()
                                                .rounded(theme::r_md())
                                                .bg(theme::popover_glass())
                                                .border_1()
                                                .border_color(theme::hairline_strong())
                                                .shadow_lg()
                                                .overflow_hidden()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(theme::text_primary())
                                                        .child(truncate_end(
                                                            prompt_txt
                                                                .lines()
                                                                .next()
                                                                .unwrap_or(&prompt_txt),
                                                            44,
                                                        )),
                                                )
                                                .when(!clean_reply.trim().is_empty(), |c| {
                                                    c.child(
                                                        div()
                                                            .text_size(px(11.0))
                                                            .line_height(px(15.5))
                                                            .text_color(theme::text_muted())
                                                            .child(truncate_end(
                                                                clean_reply.trim(),
                                                                96,
                                                            )),
                                                    )
                                                });
                                            tick_row = tick_row.child(if reduce_motion {
                                                card.into_any_element()
                                            } else {
                                                card.with_animation(
                                                    SharedString::from(format!(
                                                        "ruler-pop-{msg_idx}"
                                                    )),
                                                    Animation::new(Duration::from_millis(120))
                                                        .with_easing(ease_out_quint()),
                                                    |el, t| el.opacity(t).ml(px(-6.0 * (1.0 - t))),
                                                )
                                                .into_any_element()
                                            });
                                        }
                                        tick_row
                                    },
                                )),
                        )
                    })
                    // Emma-style floating jump-to-bottom button anchored cleanly inside the scroll pane
                    .when(!self.chat_follow && !self.chat_messages.is_empty(), |d| {
                        let is_busy = self.streaming;
                        d.child(
                            div()
                                .absolute()
                                .bottom(px(10.0))
                                .left_0()
                                .right_0()
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .id("chat-jump-bottom")
                                        .h(px(28.0))
                                        .when(!is_busy, |b| b.w(px(28.0)))
                                        .when(is_busy, |b| b.px_3())
                                        .rounded(px(99.0))
                                        .bg(theme::popover_glass())
                                        .border_1()
                                        .border_color(theme::hairline_strong())
                                        .shadow_md()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .text_xs()
                                        .text_color(if is_busy {
                                            theme::lavender()
                                        } else {
                                            theme::text_secondary()
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.chat_follow = true;
                                            this.chat_scroll.scroll_to_bottom();
                                            cx.notify();
                                        }))
                                        .child(if is_busy { "•••  ↓" } else { "↓" }),
                                ),
                        )
                    }),
            )
            // T3 Code multi-item Queued tray above the composer (Enter queues · ⌘⏎/⌥⏎ steers immediately!)
            .when(!self.queued_prompts.is_empty(), |d| {
                let q_count = self.queued_prompts.len();
                d.child(
                    div().w_full().px_3().mb_1().flex().justify_center().child(
                        div()
                            .w(px(inner_composer_w))
                            .rounded(theme::r_md())
                            .bg(theme::window_glass())
                            .border_1()
                            .border_color(theme::hairline())
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(
                                div()
                                    .px_3()
                                    .py(px(4.0))
                                    .bg(theme::surface_subtle())
                                    .border_b_1()
                                    .border_color(theme::hairline())
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .text_size(px(10.5))
                                    .child(
                                        div()
                                            .text_color(theme::text_secondary())
                                            .child(format!("Queued ({q_count})")),
                                    )
                                    .child(div().text_color(theme::text_muted()).child("⏎ Steer")),
                            )
                            .children(self.queued_prompts.iter().enumerate().map(|(qi, item)| {
                                let steer_q = item.clone();
                                let edit_q = item.clone();
                                div()
                                    .px_3()
                                    .py(px(5.0))
                                    .when(qi > 0, |r| {
                                        r.border_t_1().border_color(theme::hairline())
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .text_xs()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_color(theme::text_secondary())
                                            .child(truncate_end(
                                                item,
                                                ((inner_composer_w - 135.0) / 7.0).clamp(12.0, 64.0)
                                                    as usize,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .id(SharedString::from(format!("steer-q-{qi}")))
                                                    .px(px(5.0))
                                                    .py(px(1.0))
                                                    .rounded(theme::r_xs())
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .cursor_pointer()
                                                    .text_size(px(11.0))
                                                    .text_color(theme::text_primary())
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        if qi < this.queued_prompts.len() {
                                                            this.queued_prompts.remove(qi);
                                                        }
                                                        this.stop_stream(cx);
                                                        let atts = std::mem::take(
                                                            &mut this.chat_attachments,
                                                        );
                                                        this.submit_prompt_with_attachments(
                                                            steer_q.clone(),
                                                            atts,
                                                            false,
                                                            cx,
                                                        );
                                                    }))
                                                    .child("↪ Steer"),
                                            )
                                            .child(
                                                div()
                                                    .id(SharedString::from(format!("edit-q-{qi}")))
                                                    .cursor_pointer()
                                                    .text_size(px(11.0))
                                                    .text_color(theme::text_muted())
                                                    .hover(|s| s.text_color(theme::text_primary()))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        if qi < this.queued_prompts.len() {
                                                            this.queued_prompts.remove(qi);
                                                        }
                                                        this.chat_input = edit_q.clone();
                                                        this.active_input = ActiveInput::Chat;
                                                        cx.notify();
                                                    }))
                                                    .child("↑"),
                                            )
                                            .child(
                                                div()
                                                    .id(SharedString::from(format!("rm-q-{qi}")))
                                                    .cursor_pointer()
                                                    .text_size(px(11.0))
                                                    .text_color(theme::text_muted())
                                                    .hover(|s| s.text_color(theme::text_primary()))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        if qi < this.queued_prompts.len() {
                                                            this.queued_prompts.remove(qi);
                                                            cx.notify();
                                                        }
                                                    }))
                                                    .child("✕"),
                                            ),
                                    )
                            })),
                    ),
                )
            })
            // Interactive `@` role/doc & `/` slash-command picker right above the composer (T3 Code pattern!)
            .when_some(
                {
                    let items = self.chat_picker_items();
                    (input_active && !items.is_empty()).then_some(items)
                },
                |d, items| {
                    let sel = self.chat_picker_idx.min(items.len().saturating_sub(1));
                    d.child(
                        div().w_full().px_3().mb_1().flex().justify_center().child(
                            div()
                                .w(px(inner_composer_w))
                                .p_1()
                                .rounded(theme::r_md())
                                .bg(theme::popover_glass())
                                .border_1()
                                .border_color(theme::hairline_strong())
                                .shadow_lg()
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .gap(px(1.0))
                                .children(items.into_iter().enumerate().map(|(pi, item)| {
                                    let active = pi == sel;
                                    let insert_txt = item.insert_text;
                                    let role_att = item.role_attach;
                                    let meta_cap = ((inner_composer_w - 132.0) / 6.8)
                                        .clamp(10.0, 52.0)
                                        as usize;
                                    div()
                                        .id(SharedString::from(format!("cpick-{pi}")))
                                        .w_full()
                                        .px_2()
                                        .py(px(6.0))
                                        .rounded(theme::r_sm())
                                        .when(active, |r| r.bg(theme::surface_active()))
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .overflow_hidden()
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.apply_chat_picker_item(
                                                insert_txt.clone(),
                                                role_att.clone(),
                                                cx,
                                            );
                                        }))
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_xs()
                                                .font_weight(if active {
                                                    FontWeight::MEDIUM
                                                } else {
                                                    FontWeight::NORMAL
                                                })
                                                .text_color(if active {
                                                    theme::text_primary()
                                                } else {
                                                    theme::text_secondary()
                                                })
                                                .child(item.label),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .flex()
                                                .justify_end()
                                                .text_size(px(11.0))
                                                .text_color(if active {
                                                    theme::text_secondary()
                                                } else {
                                                    theme::text_muted()
                                                })
                                                .child(truncate_end(&item.meta, meta_cap)),
                                        )
                                })),
                        ),
                    )
                },
            )
            // T3 Code-grade unified composer card (mathematically locked to rail width or centered 728px in full-screen!)
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .px_3()
                    .pb_3()
                    .pt_1()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .id("chat-input-box")
                            .w(px(inner_composer_w))
                            .p(px(8.0))
                            .rounded(theme::r_lg())
                            .bg(if input_active {
                                theme::surface_active()
                            } else {
                                theme::surface_subtle()
                            })
                            .border_1()
                            .border_color(if input_active {
                                theme::hairline_strong()
                            } else {
                                gpui::rgba(0x00000000)
                            })
                            .hover(|s| {
                                if input_active {
                                    s
                                } else {
                                    s.bg(gpui::rgba(0xffffff0e))
                                }
                            })
                            .flex()
                            .flex_col()
                            .gap_2()
                            .cursor_text()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_input = ActiveInput::Chat;
                                cx.notify();
                            }))
                            // Clean inline reply quote chip (NO curved left border!)
                            .when_some(self.quoted_reply.clone(), |d, q| {
                                d.child(
                                    div()
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(theme::r_sm())
                                        .bg(theme::window_glass())
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_size(px(11.5))
                                                .text_color(theme::text_muted())
                                                .child(truncate_end(&format!("↩  {q}"), 34)),
                                        )
                                        .child(
                                            div()
                                                .id("clear-quote-chip")
                                                .flex_shrink_0()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .hover(|s| s.text_color(theme::text_primary()))
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.quoted_reply = None;
                                                    cx.notify();
                                                }))
                                                .child("✕"),
                                        ),
                                )
                            })
                            // Attached role chip
                            .when_some(self.attached_role.clone(), |d, (company, path)| {
                                let short =
                                    truncate_label(path.rsplit('/').next().unwrap_or(&path), 20);
                                d.child(
                                    div()
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(theme::r_sm())
                                        .bg(theme::lavender_wash())
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_size(px(11.5))
                                                .text_color(theme::lavender())
                                                .child(format!("{company} — {short}")),
                                        )
                                        .child(
                                            div()
                                                .id("clear-role-chip")
                                                .flex_shrink_0()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .hover(|s| s.text_color(theme::text_primary()))
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.attached_role = None;
                                                    cx.notify();
                                                }))
                                                .child("✕"),
                                        ),
                                )
                            })
                            // Inline image thumbnails & file chips inside composer (T3 Code style!)
                            .when(!self.chat_attachments.is_empty(), |d| {
                                d.child(div().flex().flex_wrap().gap_2().children(
                                    self.chat_attachments.iter().enumerate().map(|(ai, p)| {
                                        if is_image_path(p) {
                                            div()
                                                .relative()
                                                .child(
                                                    img(p.clone())
                                                        .size(px(46.0))
                                                        .rounded(theme::r_sm()),
                                                )
                                                .child(
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "rm-att-{ai}"
                                                        )))
                                                        .absolute()
                                                        .top(px(2.0))
                                                        .right(px(2.0))
                                                        .w(px(16.0))
                                                        .h(px(16.0))
                                                        .rounded(px(99.0))
                                                        .bg(gpui::rgba(0x000000cc))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .cursor_pointer()
                                                        .text_size(px(10.0))
                                                        .text_color(theme::text_primary())
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                if ai < this.chat_attachments.len()
                                                                {
                                                                    this.chat_attachments
                                                                        .remove(ai);
                                                                    cx.notify();
                                                                }
                                                            },
                                                        ))
                                                        .child("✕"),
                                                )
                                                .into_any_element()
                                        } else {
                                            let fname = p
                                                .file_name()
                                                .and_then(|s| s.to_str())
                                                .unwrap_or("file");
                                            div()
                                                .px_2()
                                                .py(px(3.0))
                                                .rounded(theme::r_sm())
                                                .bg(theme::window_glass())
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .text_size(px(11.0))
                                                .text_color(theme::text_secondary())
                                                .child(truncate_label(fname, 18))
                                                .child(
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "rm-att-{ai}"
                                                        )))
                                                        .cursor_pointer()
                                                        .text_color(theme::text_muted())
                                                        .hover(|s| {
                                                            s.text_color(theme::text_primary())
                                                        })
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                if ai < this.chat_attachments.len()
                                                                {
                                                                    this.chat_attachments
                                                                        .remove(ai);
                                                                    cx.notify();
                                                                }
                                                            },
                                                        ))
                                                        .child("✕"),
                                                )
                                                .into_any_element()
                                        }
                                    }),
                                ))
                            })
                            // Compact large-paste chips (T3 Code pattern: keeps composer clean!)
                            .when(!self.pasted_snippets.is_empty(), |d| {
                                d.child(div().flex().flex_wrap().gap_1().children(
                                    self.pasted_snippets.iter().enumerate().map(|(pi, txt)| {
                                        let lines = txt.lines().count().max(1);
                                        let paste_label = if lines > 1 {
                                            format!("Pasted ({lines} lines)")
                                        } else {
                                            format!("Pasted ({} chars)", txt.chars().count())
                                        };
                                        div()
                                            .px_2()
                                            .py(px(3.0))
                                            .rounded(theme::r_sm())
                                            .bg(theme::window_glass())
                                            .border_1()
                                            .border_color(theme::hairline())
                                            .overflow_hidden()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .text_size(px(11.0))
                                            .text_color(theme::text_secondary())
                                            .child(paste_label)
                                            .child(
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "rm-paste-{pi}"
                                                    )))
                                                    .cursor_pointer()
                                                    .text_color(theme::text_muted())
                                                    .hover(|s| s.text_color(theme::text_primary()))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        if pi < this.pasted_snippets.len() {
                                                            this.pasted_snippets.remove(pi);
                                                            cx.notify();
                                                        }
                                                    }))
                                                    .child("✕"),
                                            )
                                    }),
                                ))
                            })
                            // Multi-line auto-growing input region (1 line → 136px max, then smooth internal scroll)
                            .child(
                                div()
                                    .id("chat-input-scroll")
                                    .w_full()
                                    .min_h(px(22.0))
                                    .max_h(px(136.0))
                                    .overflow_y_scroll()
                                    .on_scroll_wheel({
                                        let capture = input_active
                                            && (self.chat_input.len() > 80
                                                || self.chat_input.contains('\n'));
                                        move |_, _, cx| {
                                            if capture {
                                                cx.stop_propagation();
                                            }
                                        }
                                    })
                                    .px_1()
                                    .py(px(2.0))
                                    .child(chat_multiline_input(
                                        &self.chat_input,
                                        &placeholder,
                                        input_active,
                                        input_active && self.input_all_selected,
                                        self.reduce_motion,
                                    )),
                            )
                            // Bottom action bar: paperclip attach on left, mutually-exclusive hints OR draft Steer/Queue + Stop/Send on right
                            .child({
                                let has_draft = !self.chat_input.trim().is_empty()
                                    || !self.chat_attachments.is_empty()
                                    || !self.pasted_snippets.is_empty();
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .id("chat-attach-btn")
                                                    .w(px(24.0))
                                                    .h(px(24.0))
                                                    .flex_shrink_0()
                                                    .rounded(theme::r_sm())
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.pick_chat_attachments(cx);
                                                    }))
                                                    .child(
                                                        svg()
                                                            .path("icons/paperclip.svg")
                                                            .size(px(14.0))
                                                            .text_color(theme::text_muted()),
                                                    ),
                                            )
                                            .when(
                                                self.streaming
                                                    && !has_draft
                                                    && inner_composer_w >= 265.0,
                                                |r| {
                                                    r.child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .gap_2()
                                                            .overflow_hidden()
                                                            .text_size(px(10.5))
                                                            .text_color(theme::text_muted())
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .items_center()
                                                                    .gap(px(4.0))
                                                                    .child(
                                                                        div()
                                                                            .px(px(4.0))
                                                                            .py(px(1.0))
                                                                            .rounded(theme::r_xs())
                                                                            .bg(
                                                                                theme::window_glass(
                                                                                ),
                                                                            )
                                                                            .border_1()
                                                                            .border_color(
                                                                                theme::hairline(),
                                                                            )
                                                                            .child("⏎"),
                                                                    )
                                                                    .child("Steer"),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .items_center()
                                                                    .gap(px(4.0))
                                                                    .child(
                                                                        div()
                                                                            .px(px(4.0))
                                                                            .py(px(1.0))
                                                                            .rounded(theme::r_xs())
                                                                            .bg(
                                                                                theme::window_glass(
                                                                                ),
                                                                            )
                                                                            .border_1()
                                                                            .border_color(
                                                                                theme::hairline(),
                                                                            )
                                                                            .child("⌘⏎"),
                                                                    )
                                                                    .child("Queue"),
                                                            ),
                                                    )
                                                },
                                            ),
                                    )
                                    .child(if self.streaming {
                                        div()
                                            .flex_shrink_0()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .when(has_draft, |r| {
                                                r.child(
                                                    div()
                                                        .id("chat-queue-btn")
                                                        .px_2()
                                                        .py(px(3.0))
                                                        .rounded(theme::r_sm())
                                                        .bg(theme::window_glass())
                                                        .border_1()
                                                        .border_color(theme::hairline())
                                                        .hover(|s| s.bg(theme::row_hover()))
                                                        .cursor_pointer()
                                                        .text_size(px(11.0))
                                                        .text_color(theme::text_secondary())
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.submit_chat_input(false, cx);
                                                        }))
                                                        .child(if inner_composer_w >= 300.0 {
                                                            "Queue ⌘⏎"
                                                        } else {
                                                            "Queue"
                                                        }),
                                                )
                                                .child(
                                                    div()
                                                        .id("chat-steer-btn")
                                                        .px_2()
                                                        .py(px(3.0))
                                                        .rounded(theme::r_sm())
                                                        .bg(theme::white_solid())
                                                        .hover(|s| s.opacity(0.9))
                                                        .cursor_pointer()
                                                        .text_size(px(11.0))
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(theme::ink_dark())
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.submit_chat_input(true, cx);
                                                        }))
                                                        .child(if inner_composer_w >= 300.0 {
                                                            "Steer ⏎"
                                                        } else {
                                                            "Steer"
                                                        }),
                                                )
                                            })
                                            .child(
                                                div()
                                                    .id("stop-stream")
                                                    .w(px(26.0))
                                                    .h(px(26.0))
                                                    .flex_shrink_0()
                                                    .rounded(px(99.0))
                                                    .bg(theme::surface_active())
                                                    .border_1()
                                                    .border_color(theme::hairline_strong())
                                                    .hover(|s| s.bg(theme::rose_wash()))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.stop_stream(cx);
                                                    }))
                                                    .child(
                                                        svg()
                                                            .path("icons/stop.svg")
                                                            .size(px(10.0))
                                                            .text_color(theme::text_primary()),
                                                    ),
                                            )
                                            .into_any_element()
                                    } else {
                                        let ready = has_draft || self.attached_role.is_some();
                                        div()
                                            .id("chat-send-btn")
                                            .w(px(26.0))
                                            .h(px(26.0))
                                            .flex_shrink_0()
                                            .rounded(px(99.0))
                                            .bg(if ready {
                                                theme::lavender()
                                            } else {
                                                theme::surface_active()
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(ready, |b| b.cursor_pointer())
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.submit_chat_input(false, cx);
                                            }))
                                            .child(
                                                svg()
                                                    .path("icons/arrow-up.svg")
                                                    .size(px(13.0))
                                                    .text_color(if ready {
                                                        gpui::rgba(0x0b0b0fff)
                                                    } else {
                                                        theme::text_muted()
                                                    }),
                                            )
                                            .into_any_element()
                                    })
                            }),
                    ),
            )
    }

    fn render_agent_controls(
        &self,
        scope: &'static str,
        show_settings_link: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let presets = self.runtime.model_presets();
        let cur_model = self.agent_opts.model.clone();
        let cur_provider = self.agent_opts.provider.clone();
        let cur_reasoning = self.agent_opts.reasoning.clone();
        let reasoning_opts: [(&str, Option<&str>); 5] = [
            ("Auto", None),
            ("Low", Some("low")),
            ("Med", Some("medium")),
            ("High", Some("high")),
            ("Max", Some("max")),
        ];

        div()
            .flex()
            .flex_col()
            .gap_4()
            // 1. Scalable Runtime switcher: 1-row segmented bar up to 4 agents; clean wrapping grid for 5+
            .child({
                let many_runtimes = self.available_runtimes.len() > 4;
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("Agent"),
                    )
                    .child(
                        div()
                            .w_full()
                            .p(px(2.0))
                            .rounded(theme::r_sm())
                            .bg(theme::surface_subtle())
                            .flex()
                            .when(many_runtimes, |d| d.flex_wrap().gap(px(4.0)))
                            .when(!many_runtimes, |d| d.items_center().gap(px(2.0)))
                            .children(self.available_runtimes.iter().enumerate().map(|(i, rt)| {
                                let on = rt.kind() == self.runtime.kind()
                                    && rt.short_name() == self.runtime.short_name();
                                let rt_clone = rt.clone();
                                div()
                                    .id(SharedString::from(format!("{scope}-rt-{i}")))
                                    .when(!many_runtimes, |d| d.flex_1())
                                    .when(many_runtimes, |d| d.px_2())
                                    .py(px(4.0))
                                    .rounded(theme::r_sm())
                                    .bg(if on {
                                        theme::surface_active()
                                    } else {
                                        gpui::rgba(0x00000000)
                                    })
                                    .hover(|s| if on { s } else { s.bg(theme::row_hover()) })
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_xs()
                                    .text_color(if on {
                                        theme::text_primary()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.runtime = rt_clone.clone();
                                        this.agent_opts = AgentOptions {
                                            session_name: Some(format!(
                                                "career-gui-{}",
                                                chrono::Utc::now().timestamp()
                                            )),
                                            ..AgentOptions::default()
                                        };
                                        this.resumed_session_label = None;
                                        this.host_sessions.clear();
                                        this.model_draft.clear();
                                        cx.notify();
                                    }))
                                    .child(rt.short_name().to_owned())
                            })),
                    )
            })
            // 2. Clean 2-column scrollable Model rows: readable prefix on left, muted provider badge on right
            .when(!presets.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("Model"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("{scope}-model-scroll")))
                                .max_h(px(184.0))
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .children(presets.into_iter().enumerate().map(|(i, preset)| {
                                    let is_selected = match (&cur_model, &preset.model) {
                                        (None, _) if i == 0 => true,
                                        (Some(a), Some(b)) => {
                                            a == b && cur_provider == preset.provider
                                        }
                                        _ => false,
                                    };
                                    let m = preset.model.clone();
                                    let p = preset.provider.clone();
                                    let prov_tag = preset
                                        .provider_label
                                        .as_deref()
                                        .map(|s| truncate_end(s, 14));
                                    div()
                                        .id(SharedString::from(format!("{scope}-preset-{i}")))
                                        .px_3()
                                        .py(px(7.0))
                                        .rounded(theme::r_sm())
                                        .bg(if is_selected {
                                            theme::surface_active()
                                        } else {
                                            gpui::rgba(0x00000000)
                                        })
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .text_xs()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if i == 0 {
                                                this.agent_opts.model = None;
                                                this.agent_opts.provider = None;
                                                this.model_draft.clear();
                                            } else {
                                                this.agent_opts.model = m.clone();
                                                this.agent_opts.provider = p.clone();
                                                this.model_draft = m.clone().unwrap_or_default();
                                            }
                                            cx.notify();
                                        }))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_color(if is_selected {
                                                    theme::text_primary()
                                                } else {
                                                    theme::text_secondary()
                                                })
                                                .child(truncate_end(&preset.label, 30)),
                                        )
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .when_some(prov_tag, |r, tag| {
                                                    r.child(
                                                        div()
                                                            .text_size(px(11.0))
                                                            .text_color(theme::text_muted())
                                                            .child(tag),
                                                    )
                                                })
                                                .when(is_selected, |r| {
                                                    r.child(
                                                        svg()
                                                            .path("icons/check.svg")
                                                            .size(px(12.0))
                                                            .text_color(theme::emerald()),
                                                    )
                                                }),
                                        )
                                })),
                        ),
                )
            })
            // 3. Single-row 5-segment Reasoning control (Auto · Low · Med · High · Max)
            .when(self.runtime.supports_reasoning(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("Reasoning"),
                        )
                        .child(
                            div()
                                .w_full()
                                .p(px(2.0))
                                .rounded(theme::r_sm())
                                .bg(theme::surface_subtle())
                                .flex()
                                .items_center()
                                .gap(px(2.0))
                                .children(reasoning_opts.into_iter().map(|(label, val)| {
                                    let on = cur_reasoning.as_deref() == val;
                                    div()
                                        .id(SharedString::from(format!("{scope}-reas-{label}")))
                                        .flex_1()
                                        .py(px(4.0))
                                        .rounded(theme::r_sm())
                                        .bg(if on {
                                            theme::surface_active()
                                        } else {
                                            gpui::rgba(0x00000000)
                                        })
                                        .hover(|s| if on { s } else { s.bg(theme::row_hover()) })
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_xs()
                                        .font_weight(if on {
                                            FontWeight::MEDIUM
                                        } else {
                                            FontWeight::NORMAL
                                        })
                                        .text_color(if on {
                                            theme::text_primary()
                                        } else {
                                            theme::text_muted()
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.agent_opts.reasoning = val.map(str::to_owned);
                                            cx.notify();
                                        }))
                                        .child(label)
                                })),
                        ),
                )
            })
            // 3b. Priority / Fast mode toggle — shown ONLY when the active model & provider supports it
            .when(self.runtime.supports_fast_mode(&self.agent_opts), |d| {
                let fast_on = self.agent_opts.fast_mode;
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("Speed"),
                        )
                        .child(
                            div()
                                .w_full()
                                .p(px(2.0))
                                .rounded(theme::r_sm())
                                .bg(theme::surface_subtle())
                                .flex()
                                .items_center()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("{scope}-speed-std")))
                                        .flex_1()
                                        .py(px(4.0))
                                        .rounded(theme::r_sm())
                                        .bg(if !fast_on {
                                            theme::surface_active()
                                        } else {
                                            gpui::rgba(0x00000000)
                                        })
                                        .hover(|s| {
                                            if !fast_on {
                                                s
                                            } else {
                                                s.bg(theme::row_hover())
                                            }
                                        })
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_xs()
                                        .font_weight(if !fast_on {
                                            FontWeight::MEDIUM
                                        } else {
                                            FontWeight::NORMAL
                                        })
                                        .text_color(if !fast_on {
                                            theme::text_primary()
                                        } else {
                                            theme::text_muted()
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.agent_opts.fast_mode = false;
                                            this.runtime.sync_fast_mode(false);
                                            this.set_toast("Standard speed restored", cx);
                                        }))
                                        .child("Standard"),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("{scope}-speed-fast")))
                                        .flex_1()
                                        .py(px(4.0))
                                        .rounded(theme::r_sm())
                                        .bg(if fast_on {
                                            theme::amber_wash()
                                        } else {
                                            gpui::rgba(0x00000000)
                                        })
                                        .hover(
                                            |s| if fast_on { s } else { s.bg(theme::row_hover()) },
                                        )
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .gap(px(4.0))
                                        .text_xs()
                                        .font_weight(if fast_on {
                                            FontWeight::MEDIUM
                                        } else {
                                            FontWeight::NORMAL
                                        })
                                        .text_color(if fast_on {
                                            theme::amber_text()
                                        } else {
                                            theme::text_muted()
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.agent_opts.fast_mode = true;
                                            this.runtime.sync_fast_mode(true);
                                            this.set_toast("Fast mode enabled (priority tier)", cx);
                                        }))
                                        .child(
                                            svg().path("icons/zap.svg").size(px(11.0)).text_color(
                                                if fast_on {
                                                    theme::amber_text()
                                                } else {
                                                    theme::text_muted()
                                                },
                                            ),
                                        )
                                        .child("Fast"),
                                ),
                        ),
                )
            })
            // 4. Quick jump to Settings (shown ONLY inside the Chat agent popover, never inside Settings itself)
            .when(show_settings_link, |d| {
                d.child(
                    div()
                        .pt_2()
                        .border_t_1()
                        .border_color(theme::hairline())
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .id("pop-open-settings")
                                .text_xs()
                                .text_color(theme::text_muted())
                                .hover(|s| s.text_color(theme::text_primary()))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.active_popover = ActivePopover::None;
                                    this.open_right_drawer(RightDrawer::Settings, cx);
                                    this.settings_scroll
                                        .set_offset(gpui::point(px(0.0), px(-360.0)));
                                    cx.spawn(async move |this, cx| {
                                        cx.background_executor()
                                            .timer(Duration::from_millis(40))
                                            .await;
                                        let _ = this.update(cx, |app, cx| {
                                            app.settings_scroll
                                                .set_offset(gpui::point(px(0.0), px(-360.0)));
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }))
                                .child("Delegation & tools →"),
                        ),
                )
            })
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

    fn nudge_board_x(&mut self, delta_px: f32, cx: &mut Context<Self>) {
        let cur = self.board_scroll_x.offset();
        let max_x: f32 = self.board_scroll_x.max_offset().width.into();
        let cur_x: f32 = cur.x.into();
        let next_x = (cur_x + delta_px).clamp(-max_x.max(0.0), 0.0);
        self.board_scroll_x.set_offset(point(px(next_x), cur.y));
        cx.notify();
    }

    /// Time-decaying dominant-axis gesture lock + Shift+Wheel horizontal scroll.
    /// Combined with GPUI's `.restrict_scroll_to_axis()` so sideways trackpad swipes
    /// NEVER hijack vertical column lists (including during macOS inertial momentum).
    fn handle_board_scroll(
        &mut self,
        col_name: Option<&'static str>,
        event: &ScrollWheelEvent,
        cx: &mut Context<Self>,
    ) {
        if self.active_popover != ActivePopover::None || self.expanded_reader.is_some() {
            return;
        }
        let now = Instant::now();
        let gesture_expired = self
            .last_scroll_at
            .is_none_or(|t| now.duration_since(t) > Duration::from_millis(180));
        if matches!(event.touch_phase, TouchPhase::Started) || gesture_expired {
            self.scroll_axis_lock = None;
        }
        self.last_scroll_at = Some(now);

        let delta = event.delta.pixel_delta(px(24.0));
        let mut dx: f32 = delta.x.into();
        let mut dy: f32 = delta.y.into();

        // Standard mouse-wheel horizontal scroll: Shift + Wheel
        if event.modifiers.shift && dx.abs() < 0.1 && dy.abs() > 0.1 {
            dx = dy;
            dy = 0.0;
            self.scroll_axis_lock = Some(ScrollAxis::Horizontal);
        }

        if self.scroll_axis_lock.is_none() && (dx.abs() > 0.6 || dy.abs() > 0.6) {
            self.scroll_axis_lock = if dx.abs() > dy.abs() * 1.1 || col_name.is_none() {
                Some(ScrollAxis::Horizontal)
            } else {
                Some(ScrollAxis::Vertical)
            };
        }

        match self.scroll_axis_lock {
            Some(ScrollAxis::Horizontal) => {
                self.nudge_board_x(dx, cx);
                cx.stop_propagation();
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
    }

    fn render_kanban_column(
        &self,
        col_name: &'static str,
        col_cards: Vec<&RoleCard>,
        col_total: usize,
        mode: ColumnMode,
        right_gap: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expandable_interviews = mode == ColumnMode::ExpandInterviews;
        let collapse_interviews_inline = mode == ColumnMode::CollapseInterviews;
        let expandable_archive = mode == ColumnMode::ExpandArchive;
        let collapse_archive_inline = mode == ColumnMode::CollapseArchive;
        let scroll_handle = self.col_scrolls.get(col_name).cloned();
        let hidden =
            (self.only_attention && col_total == 0) || self.collapsed_cols.contains(col_name);

        let col_div = div()
            .id(SharedString::from(format!("col-{col_name}")))
            .w(px(268.0))
            .flex_shrink_0()
            .h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded(theme::r_md())
            .pt_2()
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
                                this.interviews_toggled = true;
                                this.interviews_anim_epoch =
                                    this.interviews_anim_epoch.wrapping_add(1);
                                cx.notify();
                            }))
                            .child("Interviews  ›")
                            .into_any_element()
                    } else if collapse_interviews_inline {
                        div()
                            .id("collapse-interviews-col")
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(theme::lavender())
                            .hover(|s| s.text_color(theme::text_primary()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.interviews_expanded = false;
                                this.interviews_toggled = true;
                                this.interviews_anim_epoch =
                                    this.interviews_anim_epoch.wrapping_add(1);
                                cx.notify();
                            }))
                            .child(format!("‹  {col_name}"))
                            .into_any_element()
                    } else if expandable_archive {
                        div()
                            .id("expand-archive-col")
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(theme::text_secondary())
                            .hover(|s| s.text_color(theme::text_primary()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.archive_expanded = true;
                                this.archive_split_toggled = true;
                                this.archive_split_anim_epoch =
                                    this.archive_split_anim_epoch.wrapping_add(1);
                                cx.notify();
                            }))
                            .child("Archive  ›")
                            .into_any_element()
                    } else if collapse_archive_inline {
                        let is_rej = col_name == "Rejected";
                        div()
                            .id("collapse-archive-col")
                            .flex()
                            .items_center()
                            .gap_1()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(if is_rej {
                                theme::rose_text()
                            } else {
                                theme::text_secondary()
                            })
                            .hover(|s| s.text_color(theme::text_primary()))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.archive_expanded = false;
                                this.archive_split_toggled = true;
                                this.archive_split_anim_epoch =
                                    this.archive_split_anim_epoch.wrapping_add(1);
                                cx.notify();
                            }))
                            .child(format!("‹  {col_name}"))
                            .into_any_element()
                    } else {
                        div()
                            .text_xs()
                            .text_color(if col_name == "Rejected" {
                                theme::rose_text()
                            } else {
                                theme::text_secondary()
                            })
                            .child(col_name)
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child(col_total.to_string()),
                            )
                            .when(
                                !expandable_interviews
                                    && !collapse_interviews_inline
                                    && !expandable_archive
                                    && !collapse_archive_inline
                                    && !matches!(
                                        col_name,
                                        "Final" | "Offer" | "Withdrawn" | "Closed / expired"
                                    ),
                                |d| {
                                    d.child(
                                        div()
                                            .id(SharedString::from(format!(
                                                "collapse-col-{col_name}"
                                            )))
                                            .px(px(3.0))
                                            .rounded(theme::r_xs())
                                            .cursor_pointer()
                                            .hover(|s| {
                                                s.bg(theme::row_hover())
                                                    .text_color(theme::text_primary())
                                            })
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.collapsed_cols.insert(col_name);
                                                this.attention_toggled = true;
                                                this.collapse_anim_epoch =
                                                    this.collapse_anim_epoch.wrapping_add(1);
                                                cx.notify();
                                            }))
                                            .child("−"),
                                    )
                                },
                            ),
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
                scroller.style().restrict_scroll_to_axis = Some(true);
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
                        .rounded(theme::r_sm())
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
                                        .when_some(
                                            card.note_line
                                                .clone()
                                                .filter(|n| !n.eq_ignore_ascii_case(col_name)),
                                            |d, note| {
                                                d.child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .text_xs()
                                                        .text_color(if is_urgent {
                                                            theme::amber_text()
                                                        } else if let Some(d) = card.quiet_days {
                                                            if d >= 14 {
                                                                theme::rose_text()
                                                            } else {
                                                                theme::amber_text()
                                                            }
                                                        } else if is_quiet {
                                                            theme::rose_text()
                                                        } else {
                                                            theme::text_muted()
                                                        })
                                                        .child(note),
                                                )
                                            },
                                        ),
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
                    if let Some(epoch) = just_landed
                        && !self.reduce_motion
                    {
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

        // Single standardized DRY clip-rect column slide (no text reflow, zero squashed rails)
        let epoch = self
            .attention_anim_epoch
            .wrapping_add(self.collapse_anim_epoch);
        clip_column_slide(
            col_div,
            col_name,
            268.0,
            right_gap,
            hidden,
            self.attention_toggled && !self.reduce_motion,
            epoch,
        )
    }

    fn render_board(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cards = self.filtered_cards();
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
            .pt_2()
            .pb(px(56.0))
            .gap_2()
            .when(self.is_sample_workspace && !self.needs_first_run, |root_d| {
                root_d.child(
                    div()
                        .w_full()
                        .px_3()
                        .py(px(6.0))
                        .rounded(theme::r_md())
                        .bg(theme::surface_subtle())
                        .border_1()
                        .border_color(theme::hairline())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .child("Exploring sample workspace (10 fictional roles — click any card to inspect CV & brief)"),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .id("sample-bar-create")
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(theme::r_sm())
                                        .bg(theme::surface_active())
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .cursor_pointer()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::text_primary())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.create_workspace_dialog(cx);
                                        }))
                                        .child("Start my workspace →"),
                                )
                                .child(
                                    div()
                                        .id("sample-bar-open")
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(theme::r_sm())
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.open_workspace_dialog(cx);
                                        }))
                                        .child("Open folder"),
                                ),
                        ),
                )
            })
            .when(
                !self.loading
                    && !self.needs_first_run
                    && !self.snapshot.healthy
                    && self.snapshot.total_records == 0
                    && !self.snapshot.status_note.is_empty(),
                |root_d| {
                    let note = self.snapshot.status_note.clone();
                    root_d.child(
                        div()
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded(theme::r_md())
                            .bg(theme::rose_wash())
                            .border_1()
                            .border_color(theme::rose_text())
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme::rose_text())
                                            .child("Workspace sync failed"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(theme::text_secondary())
                                            .child(note),
                                    ),
                            )
                            .child(
                                div()
                                    .id("board-retry-sync")
                                    .flex_shrink_0()
                                    .px_2()
                                    .py(px(4.0))
                                    .rounded(theme::r_sm())
                                    .bg(theme::surface_active())
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(theme::text_primary())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_snapshot_async(true, cx);
                                    }))
                                    .child("Retry"),
                            ),
                    )
                },
            )
            // Single unified Board / Flow toolbar (fixed 28px height — zero wasted vertical space or layout jump!)
            .child(
                div()
                    .w_full()
                    .h(px(28.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    // Left side: Sankey breadcrumbs / Overview|Expand-all (in Flow) OR restored-column pills (in Board)
                    .child(if self.flow_view {
                        if let Some(zr) = self.sankey_zoom {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.0))
                                .text_xs()
                                .child(
                                    div()
                                        .id("sk-crumb-root")
                                        .px(px(6.0))
                                        .py(px(3.0))
                                        .rounded(theme::r_sm())
                                        .hover(|s| {
                                            s.bg(theme::row_hover())
                                                .text_color(theme::text_primary())
                                        })
                                        .cursor_pointer()
                                        .text_color(theme::text_muted())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sankey_zoom = None;
                                            this.sankey_bucket = None;
                                            this.sankey_filter.clear();
                                            this.sankey_hover = None;
                                            this.flow_anim_epoch =
                                                this.flow_anim_epoch.wrapping_add(1);
                                            cx.notify();
                                        }))
                                        .child("← All roles"),
                                )
                                .child(div().text_color(theme::text_muted()).child("›"))
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(zr.color())
                                        .child(zr.label()),
                                )
                                .into_any_element()
                        } else {
                            let expanded = self.sankey_expand_all;
                            let all_time = self.sankey_all_time;
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .p(px(2.0))
                                        .rounded(theme::r_sm())
                                        .bg(theme::surface_subtle())
                                        .flex()
                                        .items_center()
                                        .gap(px(2.0))
                                        .child(
                                            div()
                                                .id("sk-mode-current")
                                                .px_2()
                                                .py(px(2.0))
                                                .rounded(theme::r_xs())
                                                .bg(if !all_time {
                                                    theme::surface_active()
                                                } else {
                                                    gpui::rgba(0x00000000)
                                                })
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .text_xs()
                                                .text_color(if !all_time {
                                                    theme::text_primary()
                                                } else {
                                                    theme::text_muted()
                                                })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    if this.sankey_all_time {
                                                        this.sankey_all_time = false;
                                                        this.sankey_bucket = None;
                                                        this.flow_anim_epoch =
                                                            this.flow_anim_epoch.wrapping_add(1);
                                                        cx.notify();
                                                    }
                                                }))
                                                .child("Current"),
                                        )
                                        .child(
                                            div()
                                                .id("sk-mode-alltime")
                                                .px_2()
                                                .py(px(2.0))
                                                .rounded(theme::r_xs())
                                                .bg(if all_time {
                                                    theme::surface_active()
                                                } else {
                                                    gpui::rgba(0x00000000)
                                                })
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .text_xs()
                                                .text_color(if all_time {
                                                    theme::text_primary()
                                                } else {
                                                    theme::text_muted()
                                                })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    if !this.sankey_all_time {
                                                        this.sankey_all_time = true;
                                                        this.sankey_bucket = None;
                                                        this.flow_anim_epoch =
                                                            this.flow_anim_epoch.wrapping_add(1);
                                                        cx.notify();
                                                    }
                                                }))
                                                .child("All-time"),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("sk-toggle-expand")
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(theme::r_sm())
                                        .bg(if expanded {
                                            theme::surface_active()
                                        } else {
                                            theme::surface_subtle()
                                        })
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .gap(px(5.0))
                                        .text_xs()
                                        .text_color(if expanded {
                                            theme::text_primary()
                                        } else {
                                            theme::text_secondary()
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.sankey_expand_all = !this.sankey_expand_all;
                                            this.sankey_zoom = None;
                                            this.sankey_bucket = None;
                                            this.sankey_filter.clear();
                                            this.sankey_hover = None;
                                            this.flow_anim_epoch =
                                                this.flow_anim_epoch.wrapping_add(1);
                                            cx.notify();
                                        }))
                                        .child(
                                            svg()
                                                .path(if expanded {
                                                    "icons/minimize.svg"
                                                } else {
                                                    "icons/expand.svg"
                                                })
                                                .size(px(11.5))
                                                .text_color(if expanded {
                                                    theme::text_primary()
                                                } else {
                                                    theme::text_muted()
                                                }),
                                        )
                                        .child(if expanded { "Collapse" } else { "Expand all" }),
                                )
                                .into_any_element()
                        }
                    } else {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(["Saved", "Applied", "Screening"].into_iter().filter_map(
                                |name| {
                                    if !self.collapsed_cols.contains(name) {
                                        return None;
                                    }
                                    let count = cards.iter().filter(|c| c.column == name).count();
                                    Some(
                                        div()
                                            .id(SharedString::from(format!("restore-col-{name}")))
                                            .px_2()
                                            .py_1()
                                            .rounded(theme::r_sm())
                                            .bg(theme::surface_subtle())
                                            .hover(|s| s.bg(theme::row_hover()))
                                            .cursor_pointer()
                                            .text_xs()
                                            .text_color(theme::text_secondary())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.collapsed_cols.remove(name);
                                                this.attention_toggled = true;
                                                this.collapse_anim_epoch =
                                                    this.collapse_anim_epoch.wrapping_add(1);
                                                cx.notify();
                                            }))
                                            .child(format!("+ {name} ({count})")),
                                    )
                                },
                            ))
                            .into_any_element()
                    })
                    // Right side: inline Zoom (in Flow) + Board|Flow toggle + Board filters
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(self.flow_view, |bar| {
                                let scale_pct =
                                    (self.sankey_scale.clamp(0.75, 1.50) * 100.0).round() as i32;
                                bar.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(2.0))
                                        .pr_2()
                                        .border_r_1()
                                        .border_color(theme::hairline())
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(
                                            div()
                                                .id("sk-zoom-out")
                                                .w(px(22.0))
                                                .h(px(22.0))
                                                .rounded(theme::r_xs())
                                                .hover(|s| {
                                                    s.bg(theme::row_hover())
                                                        .text_color(theme::text_primary())
                                                })
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.sankey_scale =
                                                        (this.sankey_scale - 0.15).max(0.75);
                                                    cx.notify();
                                                }))
                                                .child("−"),
                                        )
                                        .child(
                                            div()
                                                .id("sk-zoom-reset")
                                                .px(px(5.0))
                                                .py(px(2.0))
                                                .rounded(theme::r_xs())
                                                .hover(|s| {
                                                    s.bg(theme::row_hover())
                                                        .text_color(theme::text_primary())
                                                })
                                                .cursor_pointer()
                                                .text_size(px(11.0))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.sankey_scale = 1.0;
                                                    cx.notify();
                                                }))
                                                .child(format!("{scale_pct}%")),
                                        )
                                        .child(
                                            div()
                                                .id("sk-zoom-in")
                                                .w(px(22.0))
                                                .h(px(22.0))
                                                .rounded(theme::r_xs())
                                                .hover(|s| {
                                                    s.bg(theme::row_hover())
                                                        .text_color(theme::text_primary())
                                                })
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.sankey_scale =
                                                        (this.sankey_scale + 0.15).min(1.50);
                                                    cx.notify();
                                                }))
                                                .child("+"),
                                        ),
                                )
                            })
                            .child(
                                // Sleek borderless icon-pair view switcher (Board | Sankey Flow)
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.0))
                                    .child(
                                        div()
                                            .id("view-mode-board")
                                            .w(px(26.0))
                                            .h(px(26.0))
                                            .rounded(theme::r_sm())
                                            .when(!self.flow_view, |d| {
                                                d.bg(theme::surface_subtle())
                                            })
                                            .hover(|s| s.bg(theme::row_hover()))
                                            .cursor_pointer()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if this.flow_view {
                                                    this.flow_view = false;
                                                    if this.active_input
                                                        == ActiveInput::SankeyFilter
                                                    {
                                                        this.active_input = ActiveInput::None;
                                                    }
                                                    this.flow_anim_epoch =
                                                        this.flow_anim_epoch.wrapping_add(1);
                                                    cx.notify();
                                                }
                                            }))
                                            .child(
                                                svg()
                                                    .path("icons/columns.svg")
                                                    .size(px(14.0))
                                                    .text_color(if !self.flow_view {
                                                        theme::text_primary()
                                                    } else {
                                                        theme::text_muted()
                                                    }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("view-mode-flow")
                                            .w(px(26.0))
                                            .h(px(26.0))
                                            .rounded(theme::r_sm())
                                            .when(self.flow_view, |d| d.bg(theme::surface_subtle()))
                                            .hover(|s| s.bg(theme::row_hover()))
                                            .cursor_pointer()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                if !this.flow_view {
                                                    this.flow_view = true;
                                                    this.sankey_hover = None;
                                                    this.flow_anim_epoch =
                                                        this.flow_anim_epoch.wrapping_add(1);
                                                    cx.notify();
                                                }
                                            }))
                                            .child(
                                                svg()
                                                    .path("icons/flow.svg")
                                                    .size(px(14.0))
                                                    .text_color(if self.flow_view {
                                                        theme::text_primary()
                                                    } else {
                                                        theme::text_muted()
                                                    }),
                                            ),
                                    ),
                            )
                            .when(!self.flow_view, |bar| {
                                bar.child(
                                    div()
                                        .id("toggle-attention-only")
                                        .px_2()
                                        .py_1()
                                        .rounded(theme::r_sm())
                                        .cursor_pointer()
                                        .when(self.only_attention, |d| {
                                            d.bg(theme::surface_active())
                                        })
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
                                        .rounded(theme::r_sm())
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
                                            this.archive_toggled = true;
                                            this.archive_anim_epoch =
                                                this.archive_anim_epoch.wrapping_add(1);
                                            cx.notify();
                                        }))
                                        .child(format!("Archive ({})", self.snapshot.closed_count)),
                                )
                            })
                            // Non-intrusive horizontal nudge buttons for mouse-only users (only in Board view)
                            .when(!self.flow_view, |bar| {
                                bar.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(2.0))
                                        .pl_1()
                                        .child(
                                            div()
                                                .id("board-nudge-left")
                                                .w(px(22.0))
                                                .h(px(22.0))
                                                .rounded(theme::r_xs())
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(|s| {
                                                    s.bg(theme::row_hover())
                                                        .text_color(theme::text_primary())
                                                })
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.nudge_board_x(284.0, cx);
                                                }))
                                                .child("‹"),
                                        )
                                        .child(
                                            div()
                                                .id("board-nudge-right")
                                                .w(px(22.0))
                                                .h(px(22.0))
                                                .rounded(theme::r_xs())
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .hover(|s| {
                                                    s.bg(theme::row_hover())
                                                        .text_color(theme::text_primary())
                                                })
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.nudge_board_x(-284.0, cx);
                                                }))
                                                .child("›"),
                                        ),
                                )
                            }),
                    ),
            )
            // Animated swap between Kanban board and interactive Sankey Flow funnel
            .child(if self.flow_view {
                let q = self.search_query.trim().to_ascii_lowercase();
                let all_for_sankey: Vec<RoleCard> = self
                    .snapshot
                    .active_cards
                    .iter()
                    .chain(self.snapshot.archived_cards.iter())
                    .filter(|c| q.is_empty() || c.search_blob.contains(&q))
                    .cloned()
                    .collect();
                sankey::render_sankey_view(
                    &all_for_sankey,
                    self.sankey_all_time,
                    self.sankey_zoom,
                    self.sankey_expand_all,
                    self.sankey_scale,
                    self.sankey_bucket,
                    self.sankey_hover,
                    &self.sankey_filter,
                    self.active_input == ActiveInput::SankeyFilter,
                    self.flow_anim_epoch,
                    self.reduce_motion,
                    |hov, this: &mut Self, _, cx| {
                        if this.sankey_hover != hov {
                            this.sankey_hover = hov;
                            cx.notify();
                        }
                    },
                    |zm, this: &mut Self, _, cx| {
                        this.sankey_zoom = zm;
                        this.sankey_bucket = None;
                        this.sankey_filter.clear();
                        if this.active_input == ActiveInput::SankeyFilter {
                            this.active_input = ActiveInput::None;
                        }
                        this.sankey_hover = None;
                        this.flow_anim_epoch = this.flow_anim_epoch.wrapping_add(1);
                        cx.notify();
                    },
                    |bucket, this: &mut Self, _, cx| {
                        this.sankey_bucket = bucket;
                        this.sankey_filter.clear();
                        if bucket.is_some() {
                            this.active_input = ActiveInput::SankeyFilter;
                        } else if this.active_input == ActiveInput::SankeyFilter {
                            this.active_input = ActiveInput::None;
                        }
                        cx.notify();
                    },
                    |this: &mut Self, _, cx| {
                        this.active_input = ActiveInput::SankeyFilter;
                        cx.notify();
                    },
                    |card, this: &mut Self, _, cx| {
                        this.open_role_drawer(card, DrawerTab::Overview, cx);
                    },
                    cx,
                )
            } else {
                let q = self.search_query.trim().to_ascii_lowercase();
                if !q.is_empty() {
                    // Smart search board: hide 0-count columns, auto-include Archive matches, and sort most-populated/most-relevant columns first!
                    let search_buckets: [(&'static str, Vec<&RoleCard>); 5] = [
                        (
                            "Interviews",
                            cards
                                .iter()
                                .copied()
                                .filter(|c| c.column == "Interviews")
                                .collect(),
                        ),
                        (
                            "Screening",
                            cards
                                .iter()
                                .copied()
                                .filter(|c| c.column == "Screening")
                                .collect(),
                        ),
                        (
                            "Applied",
                            cards
                                .iter()
                                .copied()
                                .filter(|c| c.column == "Applied")
                                .collect(),
                        ),
                        (
                            "Saved",
                            cards
                                .iter()
                                .copied()
                                .filter(|c| c.column == "Saved")
                                .collect(),
                        ),
                        (
                            "Archive",
                            self.snapshot
                                .archived_cards
                                .iter()
                                .filter(|c| c.search_blob.contains(&q))
                                .collect(),
                        ),
                    ];
                    let mut non_empty: Vec<(&'static str, Vec<&RoleCard>)> = search_buckets
                        .into_iter()
                        .filter(|(_, list)| !list.is_empty())
                        .collect();
                    non_empty.sort_by_key(|(_, list)| std::cmp::Reverse(list.len()));

                    let mut search_scroller = div()
                        .id("kanban-search-scroll-x")
                        .track_scroll(&self.board_scroll_x)
                        .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                            this.handle_board_scroll(None, ev, cx);
                        }))
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .overflow_x_scroll()
                        .flex();
                    search_scroller.style().restrict_scroll_to_axis = Some(true);

                    if non_empty.is_empty() {
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .child(div().text_sm().text_color(theme::text_secondary()).child(
                                format!("No roles matching \"{}\"", self.search_query.trim()),
                            ))
                            .child(
                                div()
                                    .id("search-empty-clear")
                                    .px_3()
                                    .py(px(5.0))
                                    .rounded(theme::r_sm())
                                    .bg(theme::surface_subtle())
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(theme::lavender())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.search_query.clear();
                                        this.active_input = ActiveInput::None;
                                        cx.notify();
                                    }))
                                    .child("Clear filter"),
                            )
                            .into_any_element()
                    } else {
                        search_scroller
                            .children(non_empty.into_iter().map(|(col_name, list)| {
                                let total = list.len();
                                let mode = match col_name {
                                    "Interviews" => ColumnMode::ExpandInterviews,
                                    "Archive" => ColumnMode::ExpandArchive,
                                    _ => ColumnMode::Standard,
                                };
                                self.render_kanban_column(
                                    col_name,
                                    list.into_iter().take(40).collect(),
                                    total,
                                    mode,
                                    16.0,
                                    cx,
                                )
                            }))
                            .into_any_element()
                    }
                } else {
                    let mut board_scroller = div()
                        .id("kanban-scroll-x")
                        .track_scroll(&self.board_scroll_x)
                        .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                            this.handle_board_scroll(None, ev, cx);
                        }))
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .overflow_x_scroll()
                        .flex();
                    board_scroller.style().restrict_scroll_to_axis = Some(true);
                    let board_el = board_scroller
                        .children(base_columns.into_iter().map(|col_name| {
                            let col_cards: Vec<&RoleCard> = cards
                                .iter()
                                .copied()
                                .filter(|c| c.column == col_name)
                                .take(28)
                                .collect();
                            let col_total = cards.iter().filter(|c| c.column == col_name).count();
                            self.render_kanban_column(
                                col_name,
                                col_cards,
                                col_total,
                                ColumnMode::Standard,
                                16.0,
                                cx,
                            )
                        }))
                        // Standardized ZERO-SHIFT Interviews expand/collapse:
                        // Primary column (`Interviews ›` / `‹ Technical`) stays at the exact same (x, y) pixel,
                        // while `Final` & `Offer` glide in/out using the exact same `clip_column_slide` primitive!
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
                                ColumnMode::ExpandInterviews,
                                16.0,
                                cx,
                            )
                        } else {
                            let tech_cards: Vec<&RoleCard> = cards
                                .iter()
                                .copied()
                                .filter(|c| c.stage == "technical")
                                .take(28)
                                .collect();
                            let tech_total =
                                cards.iter().filter(|c| c.stage == "technical").count();
                            self.render_kanban_column(
                                "Technical",
                                tech_cards,
                                tech_total,
                                ColumnMode::CollapseInterviews,
                                16.0,
                                cx,
                            )
                        })
                        .children(interview_stages[1..].iter().map(|&(label, stage)| {
                            let stage_cards: Vec<&RoleCard> = cards
                                .iter()
                                .copied()
                                .filter(|c| c.stage == stage)
                                .take(28)
                                .collect();
                            let total = cards.iter().filter(|c| c.stage == stage).count();
                            let inner = self.render_kanban_column(
                                label,
                                stage_cards,
                                total,
                                ColumnMode::Standard,
                                0.0,
                                cx,
                            );
                            clip_column_slide(
                                inner,
                                label,
                                268.0,
                                16.0,
                                !self.interviews_expanded,
                                self.interviews_toggled && !self.reduce_motion,
                                self.interviews_anim_epoch,
                            )
                        }))
                        .child({
                            let inner = if !self.archive_expanded {
                                self.render_kanban_column(
                                    "Archive",
                                    archive_cards.clone(),
                                    archive_total,
                                    ColumnMode::ExpandArchive,
                                    0.0,
                                    cx,
                                )
                            } else {
                                let rej_all: Vec<&RoleCard> = self
                                    .snapshot
                                    .archived_cards
                                    .iter()
                                    .filter(|c| {
                                        (q.is_empty() || c.search_blob.contains(&q))
                                            && c.outcome
                                                .as_deref()
                                                .is_some_and(|o| o.contains("reject"))
                                    })
                                    .collect();
                                let rej_total = rej_all.len();
                                self.render_kanban_column(
                                    "Rejected",
                                    rej_all.into_iter().take(28).collect(),
                                    rej_total,
                                    ColumnMode::CollapseArchive,
                                    16.0,
                                    cx,
                                )
                            };
                            clip_column_slide(
                                inner,
                                "Archive",
                                268.0,
                                if self.archive_expanded { 16.0 } else { 0.0 },
                                !self.show_archive,
                                self.archive_toggled && !self.reduce_motion,
                                self.archive_anim_epoch,
                            )
                        })
                        .children(["Withdrawn", "Closed / expired"].into_iter().map(|label| {
                            let matching: Vec<&RoleCard> = self
                                .snapshot
                                .archived_cards
                                .iter()
                                .filter(|c| {
                                    if !q.is_empty() && !c.search_blob.contains(&q) {
                                        return false;
                                    }
                                    let out = c.outcome.as_deref().unwrap_or("");
                                    if label == "Withdrawn" {
                                        out.contains("withdraw")
                                    } else {
                                        !out.contains("reject") && !out.contains("withdraw")
                                    }
                                })
                                .collect();
                            let total = matching.len();
                            let inner = self.render_kanban_column(
                                label,
                                matching.into_iter().take(28).collect(),
                                total,
                                ColumnMode::Standard,
                                0.0,
                                cx,
                            );
                            clip_column_slide(
                                inner,
                                label,
                                268.0,
                                16.0,
                                !(self.show_archive && self.archive_expanded),
                                self.archive_split_toggled && !self.reduce_motion,
                                self.archive_split_anim_epoch,
                            )
                        }));
                    if !self.reduce_motion && self.flow_anim_epoch > 0 {
                        let fe = self.flow_anim_epoch;
                        board_el
                            .with_animation(
                                SharedString::from(format!("board-enter-{fe}")),
                                Animation::new(Duration::from_millis(180))
                                    .with_easing(ease_out_quint()),
                                |el, t| el.opacity(0.25 + 0.75 * t).mt(px(4.0 * (1.0 - t))),
                            )
                            .into_any_element()
                    } else {
                        board_el.into_any_element()
                    }
                }
            })
            // Quick-Ask upward reply popover
            .when(self.quick_popover_open, |d| {
                d.child(
                    div()
                        .absolute()
                        .left_4()
                        .bottom(px(54.0))
                        .w(px(400.0))
                        .p_3()
                        .rounded(theme::r_lg())
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
                                                    if !this.quick_input.is_empty()
                                                        && this.chat_input.is_empty()
                                                    {
                                                        this.chat_input =
                                                            std::mem::take(&mut this.quick_input);
                                                    }
                                                    this.chat_open = true;
                                                    this.active_input = ActiveInput::Chat;
                                                    this.chat_scroll.scroll_to_bottom();
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
            // Bottom floating bar: Quick Ask (hidden when Chat rail is open so only 1 input exists)
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
                    .child(if self.chat_open {
                        div().w(px(0.0)).into_any_element()
                    } else if quick_active || !self.quick_input.is_empty() {
                        let has_text = !self.quick_input.is_empty();
                        let bar = div()
                            .id("quick-ask-bar")
                            .w(px(300.0))
                            .h(px(34.0))
                            .pl(px(10.0))
                            .pr_3()
                            .rounded(px(99.0))
                            .bg(theme::popover_glass())
                            .border_1()
                            .border_color(if quick_active {
                                theme::lavender_border()
                            } else {
                                theme::hairline_strong()
                            })
                            .shadow_md()
                            .overflow_hidden()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .cursor_text()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_input = ActiveInput::QuickAsk;
                                cx.notify();
                            }))
                            .child(
                                svg()
                                    .path("icons/message-square.svg")
                                    .size(px(14.0))
                                    .flex_shrink_0()
                                    .text_color(theme::lavender()),
                            )
                            .child(input_line(
                                &self.quick_input,
                                "Ask anything…",
                                quick_active,
                                self.reduce_motion,
                                "quick",
                                36,
                            ))
                            .child(
                                div()
                                    .id("quick-send-btn")
                                    .flex_shrink_0()
                                    .px(px(4.0))
                                    .rounded(theme::r_xs())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .text_xs()
                                    .text_color(if has_text {
                                        theme::text_primary()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let q = std::mem::take(&mut this.quick_input);
                                        this.submit_prompt(q, true, cx);
                                    }))
                                    .child("⏎"),
                            );
                        if self.reduce_motion {
                            bar.into_any_element()
                        } else {
                            bar.with_animation(
                                "quick-expand-open",
                                Animation::new(Duration::from_millis(140))
                                    .with_easing(ease_out_quint()),
                                |el, t| el.w(px(34.0 + 266.0 * t)),
                            )
                            .into_any_element()
                        }
                    } else {
                        // Bespoke circular lavender-accented Quick Chat button on bottom-left (exact same 34px height & icon x/y!)
                        div()
                            .id("quick-ask-trigger")
                            .w(px(34.0))
                            .h(px(34.0))
                            .pl(px(10.0))
                            .rounded(px(99.0))
                            .bg(theme::popover_glass())
                            .border_1()
                            .border_color(theme::lavender_border())
                            .hover(|s| s.bg(theme::lavender_wash()).border_color(theme::lavender()))
                            .shadow_md()
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.active_input = ActiveInput::QuickAsk;
                                cx.notify();
                            }))
                            .child(
                                svg()
                                    .path("icons/message-square.svg")
                                    .size(px(15.0))
                                    .text_color(theme::lavender()),
                            )
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .when_some(self.armed_secrets.first().cloned(), |d, sec| {
                                let rem = sec.remaining_secs(secrets::now_unix());
                                let mins = rem / 60;
                                let secs = rem % 60;
                                let destroy_id = sec.id.clone();
                                d.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded(theme::r_sm())
                                        .bg(theme::amber_wash())
                                        .border_1()
                                        .border_color(gpui::rgba(0xfacc1544))
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_xs()
                                        .child(
                                            svg()
                                                .path("icons/lock.svg")
                                                .size(px(12.0))
                                                .text_color(theme::amber_text()),
                                        )
                                        .child(div().text_color(theme::amber_text()).child(
                                            format!(
                                                "{} {mins}:{secs:02}",
                                                truncate_end(&sec.label, 18)
                                            ),
                                        ))
                                        .child(
                                            div()
                                                .id("dock-destroy-secret")
                                                .px(px(5.0))
                                                .py(px(1.5))
                                                .rounded(theme::r_xs())
                                                .bg(theme::surface_active())
                                                .hover(|s| s.bg(theme::rose_wash()))
                                                .cursor_pointer()
                                                .text_size(px(10.5))
                                                .text_color(theme::rose_text())
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.destroy_secret_now(destroy_id.clone(), cx);
                                                }))
                                                .child("Destroy"),
                                        ),
                                )
                            })
                            .when_some(self.toast.clone(), |d, (msg, _)| {
                                let has_undo = self.undo_move.is_some();
                                d.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded(theme::r_sm())
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
                            // 1-click Refresh button that morphs in-place into the live Save/Sync status pill right beside `1 / 25  ●`
                            .child(if matches!(self.sync_state, SyncIndicator::Idle) {
                                div()
                                    .id("btn-dock-refresh")
                                    .w(px(22.0))
                                    .h(px(22.0))
                                    .rounded(theme::r_sm())
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_snapshot_async(true, cx);
                                    }))
                                    .child(
                                        svg()
                                            .path("icons/refresh-cw.svg")
                                            .size(px(12.5))
                                            .text_color(theme::text_muted()),
                                    )
                                    .into_any_element()
                            } else {
                                let epoch = self.sync_epoch;
                                let pill = match &self.sync_state {
                                    SyncIndicator::Working(lbl) => div()
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(px(99.0))
                                        .bg(theme::surface_subtle())
                                        .flex()
                                        .items_center()
                                        .gap(px(5.0))
                                        .child(
                                            svg()
                                                .path("icons/refresh-cw.svg")
                                                .size(px(12.0))
                                                .text_color(theme::lavender()),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.5))
                                                .text_color(theme::text_secondary())
                                                .child(lbl.clone()),
                                        ),
                                    SyncIndicator::Saved(lbl, _) => div()
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(px(99.0))
                                        .bg(theme::surface_subtle())
                                        .flex()
                                        .items_center()
                                        .gap(px(5.0))
                                        .child(
                                            svg()
                                                .path("icons/check.svg")
                                                .size(px(12.0))
                                                .text_color(theme::emerald()),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.5))
                                                .text_color(theme::text_secondary())
                                                .child(lbl.clone()),
                                        ),
                                    SyncIndicator::Failed(lbl, _) => div()
                                        .px_2()
                                        .py(px(3.0))
                                        .rounded(px(99.0))
                                        .bg(theme::rose_wash())
                                        .flex()
                                        .items_center()
                                        .gap(px(5.0))
                                        .child(
                                            div()
                                                .text_size(px(11.5))
                                                .text_color(theme::rose_text())
                                                .child(lbl.clone()),
                                        ),
                                    SyncIndicator::Idle => div(),
                                };
                                let is_ephemeral = matches!(
                                    self.sync_state,
                                    SyncIndicator::Saved(_, _) | SyncIndicator::Failed(_, _)
                                );
                                if self.reduce_motion {
                                    pill.into_any_element()
                                } else if is_ephemeral {
                                    pill.with_animation(
                                        SharedString::from(format!("sync-pill-{epoch}")),
                                        Animation::new(Duration::from_millis(2350)),
                                        |el, t| {
                                            let op = if t < 0.08 {
                                                t / 0.08
                                            } else if t > 0.78 {
                                                ((1.0 - t) / 0.22).clamp(0.0, 1.0)
                                            } else {
                                                1.0
                                            };
                                            el.opacity(op)
                                        },
                                    )
                                    .into_any_element()
                                } else {
                                    pill.with_animation(
                                        SharedString::from(format!("sync-pill-{epoch}")),
                                        Animation::new(Duration::from_millis(180))
                                            .with_easing(ease_out_quint()),
                                        |el, t| el.opacity(t),
                                    )
                                    .into_any_element()
                                }
                            })
                            .child(
                                div()
                                    .id("btn-budget")
                                    .px_2()
                                    .py_1()
                                    .rounded(theme::r_sm())
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(theme::text_secondary())
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_hover(cx.listener(|this, hovered, _, cx| {
                                        this.on_dock_trigger_hover(
                                            ActivePopover::Budget,
                                            *hovered,
                                            cx,
                                        );
                                    }))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_dock_popover_pin(ActivePopover::Budget, cx);
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
                                    .rounded(theme::r_sm())
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_hover(cx.listener(|this, hovered, _, cx| {
                                        this.on_dock_trigger_hover(
                                            ActivePopover::Health,
                                            *hovered,
                                            cx,
                                        );
                                    }))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_dock_popover_pin(ActivePopover::Health, cx);
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
                let role_path_for_chat = role.record_path.clone();
                let brief_opt = role.brief_path.clone();
                let source_opt = role.source_url.clone();
                let stage_label = title_case_stage(&role.stage);

                Some(
                    div()
                        .id("role-drawer")
                        .w_full()
                        .flex_shrink_0()
                        .h_full()
                        .p_5()
                        .bg(theme::panel_glass())
                        .border_l_1()
                        .border_color(theme::hairline())
                        .flex()
                        .flex_col()
                        .gap_3()
                        .overflow_hidden()
                        // PINNED non-scrolling header (zero vertical shift across tabs!)
                        .child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .items_start()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_base()
                                                .text_color(theme::text_primary())
                                                .child(role.company.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child(format!("{}  —  {}", role.title, stage_label)),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("close-drawer")
                                        .flex_shrink_0()
                                        .p_1()
                                        .rounded(theme::r_xs())
                                        .hover(|s| s.bg(theme::row_hover()))
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
                        // PINNED 3-tab strip (Overview · Brief · Record) — zero bounce, keyboard 1/2/3 or [/]
                        .child(
                            div()
                                .flex_shrink_0()
                                .pt_1()
                                .flex()
                                .gap_5()
                                .border_b_1()
                                .border_color(theme::hairline())
                                .children(
                                    [
                                        (DrawerTab::Overview, "Overview"),
                                        (DrawerTab::Brief, "Brief"),
                                        (DrawerTab::Notes, "Record"),
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
                                                this.switch_role_tab(tab, cx);
                                            }))
                                            .child(label)
                                    }),
                                ),
                        )
                        // SCROLLABLE tab content ONLY (header & bottom CTA stay rock-solid pinned!)
                        .child({
                            let tab_epoch = self.drawer_tab_epoch;
                            let tab_body: AnyElement = match self.drawer_tab {
                                DrawerTab::Overview => {
                                    let det = self.role_details.as_ref();
                                    let salary_val = det
                                        .and_then(|d| d.salary.clone())
                                        .unwrap_or_else(|| "Not listed".into());
                                    let loc_val = det
                                        .and_then(|d| d.location_mode.clone())
                                        .unwrap_or_else(|| "Not listed".into());
                                    let channel_opt = det.and_then(|d| d.source_channel.clone());
                                    let contact_opt = det.and_then(|d| d.contact.clone());
                                    let fit_opt = det.and_then(|d| d.fit_summary.clone());
                                    let sent_opt = det.and_then(|d| d.submitted_at.clone());
                                    let cv_opt = det
                                        .and_then(|d| d.packet_cv.clone())
                                        .or_else(|| role.cv_track.clone());
                                    let answers_opt = det.and_then(|d| d.packet_answers.clone());
                                    let sha_opt = det
                                        .and_then(|d| d.packet_sha.clone())
                                        .or_else(|| role.packet_hash.clone())
                                        .map(|h| h.chars().take(12).collect::<String>());
                                    let has_receipt = sent_opt.is_some()
                                        || cv_opt.is_some()
                                        || answers_opt.is_some()
                                        || sha_opt.is_some();
                                    let rec_short = role
                                        .record_path
                                        .rsplit('/')
                                        .next()
                                        .unwrap_or(&role.record_path);
                                    let rec_display = truncate_label(rec_short, 24);
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_5()
                                        // 1. At-a-glance snapshot
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_2()
                                                .child(kv_row("Location", loc_val))
                                                .child(kv_row("Comp", salary_val))
                                                .when_some(channel_opt, |d, ch| {
                                                    d.child(kv_row("Channel", ch))
                                                })
                                                .when_some(contact_opt, |d, ct| {
                                                    d.child(kv_row("Contact", ct))
                                                })
                                                .child(kv_row("Last checked", role.checked.clone()))
                                                .when_some(role.scheduled_start.clone(), |d, s| {
                                                    d.child(kv_row("Scheduled", s))
                                                }),
                                        )
                                        // 2. Fit & role summary
                                        .when_some(fit_opt, |d, fit| {
                                            d.child(
                                                div()
                                                    .pt_4()
                                                    .border_t_1()
                                                    .border_color(theme::hairline())
                                                    .flex()
                                                    .flex_col()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme::text_muted())
                                                            .child("Summary"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .line_height(px(18.0))
                                                            .text_color(theme::text_secondary())
                                                            .child(fit),
                                                    ),
                                            )
                                        })
                                        // 3. Next step
                                        .child(
                                            div()
                                                .pt_4()
                                                .border_t_1()
                                                .border_color(theme::hairline())
                                                .flex()
                                                .flex_col()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme::text_muted())
                                                        .child("Next step"),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .line_height(px(18.0))
                                                        .text_color(theme::text_primary())
                                                        .child(if !role.urgency_reason.is_empty() {
                                                            format!(
                                                                "{}\n{}",
                                                                role.urgency_reason,
                                                                role.next_action
                                                            )
                                                        } else if !role.next_action.is_empty() {
                                                            role.next_action.clone()
                                                        } else {
                                                            "No pending action recorded.".into()
                                                        }),
                                                ),
                                        )
                                        // 4. Compact Submission Receipt (replaces sparse separate tab!)
                                        .when(has_receipt, |d| {
                                            d.child(
                                                div()
                                                    .pt_4()
                                                    .border_t_1()
                                                    .border_color(theme::hairline())
                                                    .flex()
                                                    .flex_col()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme::text_muted())
                                                            .child("Submission receipt"),
                                                    )
                                                    .when_some(sent_opt, |r, s| {
                                                        r.child(kv_row("Submitted", s))
                                                    })
                                                    .when_some(cv_opt, |r, cv| {
                                                        let short = truncate_label(
                                                            cv.rsplit('/').next().unwrap_or(&cv),
                                                            24,
                                                        );
                                                        r.child(kv_row("CV sent", short))
                                                    })
                                                    .when_some(answers_opt, |r, ans| {
                                                        let short = truncate_label(
                                                            ans.rsplit('/').next().unwrap_or(&ans),
                                                            24,
                                                        );
                                                        r.child(
                                                            div()
                                                                .flex()
                                                                .items_center()
                                                                .justify_between()
                                                                .gap_4()
                                                                .text_xs()
                                                                .child(
                                                                    div()
                                                                        .text_color(
                                                                            theme::text_muted(),
                                                                        )
                                                                        .child("Answers"),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .id("open-pkt-answers")
                                                                        .text_color(
                                                                            theme::lavender(),
                                                                        )
                                                                        .underline()
                                                                        .cursor_pointer()
                                                                        .on_click(cx.listener(
                                                                            move |this, _, _, cx| {
                                                                                this.open_file_in_reader_async(
                                                                                    ans.clone(),
                                                                                    cx,
                                                                                );
                                                                            },
                                                                        ))
                                                                        .child(short),
                                                                ),
                                                        )
                                                    })
                                                    .when_some(sha_opt, |r, sha| {
                                                        r.child(kv_row("Receipt SHA", sha))
                                                    }),
                                            )
                                        })
                                        // 5. Links (external carries ↗, in-app lavender opens tab)
                                        .child(
                                            div()
                                                .pt_4()
                                                .border_t_1()
                                                .border_color(theme::hairline())
                                                .flex()
                                                .flex_col()
                                                .gap_2()
                                                .when_some(source_opt, |d, url| {
                                                    let short = url
                                                        .trim_start_matches("https://")
                                                        .trim_start_matches("http://")
                                                        .split('/')
                                                        .next()
                                                        .unwrap_or(&url)
                                                        .to_owned();
                                                    d.child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap_4()
                                                            .text_xs()
                                                            .child(
                                                                div()
                                                                    .flex_shrink_0()
                                                                    .text_color(theme::text_muted())
                                                                    .child("Posting"),
                                                            )
                                                            .child(
                                                                div()
                                                                    .id("open-role-url")
                                                                    .text_color(
                                                                        theme::text_secondary(),
                                                                    )
                                                                    .underline()
                                                                    .hover(|s| {
                                                                        s.text_color(
                                                                            theme::text_primary(),
                                                                        )
                                                                    })
                                                                    .cursor_pointer()
                                                                    .on_click(cx.listener(
                                                                        move |_, _, _, _| {
                                                                            open_url(&url);
                                                                        },
                                                                    ))
                                                                    .child(format!("{short} ↗")),
                                                            ),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .gap_4()
                                                        .text_xs()
                                                        .child(
                                                            div()
                                                                .flex_shrink_0()
                                                                .text_color(theme::text_muted())
                                                                .child("Record"),
                                                        )
                                                        .child(
                                                            div()
                                                                .id("cite-record")
                                                                .text_color(theme::lavender())
                                                                .underline()
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        this.citation_title =
                                                                            Some(role_path.clone());
                                                                        this.switch_role_tab(
                                                                            DrawerTab::Notes,
                                                                            cx,
                                                                        );
                                                                    },
                                                                ))
                                                                .child(rec_display),
                                                        ),
                                                )
                                                .when_some(brief_opt, |d, bp| {
                                                    let short = truncate_label(
                                                        bp.rsplit('/').next().unwrap_or(&bp),
                                                        24,
                                                    );
                                                    d.child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap_4()
                                                            .text_xs()
                                                            .child(
                                                                div()
                                                                    .flex_shrink_0()
                                                                    .text_color(theme::text_muted())
                                                                    .child("Brief"),
                                                            )
                                                            .child(
                                                                div()
                                                                    .id("cite-brief")
                                                                    .text_color(theme::lavender())
                                                                    .underline()
                                                                    .cursor_pointer()
                                                                    .on_click(cx.listener(
                                                                        move |this, _, _, cx| {
                                                                            this.drawer_tab =
                                                                                DrawerTab::Brief;
                                                                            this.drawer_tab_epoch =
                                                                                this.drawer_tab_epoch
                                                                                    .wrapping_add(1);
                                                                            this.read_citation_async(
                                                                                bp.clone(),
                                                                                cx,
                                                                            );
                                                                            cx.notify();
                                                                        },
                                                                    ))
                                                                    .child(short),
                                                            ),
                                                    )
                                                }),
                                        )
                                        .into_any_element()
                                }
                                DrawerTab::Brief | DrawerTab::Notes => {
                                    let title_path = self
                                        .citation_title
                                        .clone()
                                        .unwrap_or_else(|| role.record_path.clone());
                                    let body_text = self
                                        .citation_body
                                        .clone()
                                        .unwrap_or_else(|| "Loading…".into());
                                    let short_file = truncate_label(
                                        title_path.rsplit('/').next().unwrap_or(&title_path),
                                        26,
                                    );
                                    let local_open = self
                                        .client
                                        .target
                                        .local_root()
                                        .map(|r| r.join(&title_path));
                                    let local_reveal = local_open.clone();
                                    let reader_title = title_path.clone();
                                    let reader_body = body_text.clone();
                                    let reader_title_tap = title_path.clone();
                                    let reader_body_tap = body_text.clone();

                                    let sections = if self.drawer_tab == DrawerTab::Brief {
                                        parse_brief_sections(&body_text)
                                    } else {
                                        Vec::new()
                                    };
                                    let active_sec =
                                        self.brief_section.filter(|&i| i < sections.len());
                                    let visible_md = match active_sec {
                                        Some(i) => sections[i].body.clone(),
                                        None => body_text.clone(),
                                    };
                                    let sec_epoch = self.brief_section_epoch;
                                    let sec_count = sections.len();

                                    let md_preview = div()
                                        .id("md-tap-expand")
                                        .p_2()
                                        .mx(px(-8.0))
                                        .rounded(theme::r_sm())
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.expanded_reader = Some((
                                                reader_title_tap.clone(),
                                                reader_body_tap.clone(),
                                            ));
                                            cx.notify();
                                        }))
                                        .child(markdown::render_markdown(
                                            "drawer-md",
                                            &visible_md,
                                            false,
                                            self.active_text_selection().as_ref(),
                                            self.md_link_handler(cx),
                                            self.md_select_handler(cx),
                                        ));
                                    let md_block = if self.reduce_motion {
                                        md_preview.into_any_element()
                                    } else {
                                        md_preview
                                            .with_animation(
                                                SharedString::from(format!("bsec-{sec_epoch}")),
                                                Animation::new(Duration::from_millis(120))
                                                    .with_easing(ease_out_quint()),
                                                |el, t| el.opacity(0.4 + 0.6 * t),
                                            )
                                            .into_any_element()
                                    };

                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_3()
                                        // Show, don't tell toolbar: underlined filename reveals in Finder + icon-only Editor & Expand!
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .id("md-reveal-file")
                                                        .text_xs()
                                                        .text_color(theme::text_muted())
                                                        .underline()
                                                        .hover(|s| {
                                                            s.text_color(theme::text_primary())
                                                        })
                                                        .cursor_pointer()
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                if let Some(p) = &local_reveal {
                                                                    reveal_in_file_manager(p);
                                                                } else {
                                                                    this.set_toast(
                                                                        "Reveal requires local workspace",
                                                                        cx,
                                                                    );
                                                                }
                                                            },
                                                        ))
                                                        .child(short_file),
                                                )
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_1()
                                                        .child(
                                                            div()
                                                                .id("md-open-file")
                                                                .p(px(5.0))
                                                                .rounded(theme::r_sm())
                                                                .hover(|s| {
                                                                    s.bg(theme::row_hover())
                                                                })
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        if let Some(p) = &local_open
                                                                        {
                                                                            open_in_system(p);
                                                                        } else {
                                                                            this.set_toast(
                                                                                "Open requires local workspace",
                                                                                cx,
                                                                            );
                                                                        }
                                                                    },
                                                                ))
                                                                .child(
                                                                    svg()
                                                                        .path(
                                                                            "icons/external-link.svg",
                                                                        )
                                                                        .size(px(13.5))
                                                                        .text_color(
                                                                            theme::text_muted(),
                                                                        ),
                                                                ),
                                                        )
                                                        .child(
                                                            div()
                                                                .id("md-expand-reader")
                                                                .p(px(5.0))
                                                                .rounded(theme::r_sm())
                                                                .hover(|s| {
                                                                    s.bg(theme::row_hover())
                                                                })
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        this.expanded_reader =
                                                                            Some((
                                                                                reader_title
                                                                                    .clone(),
                                                                                reader_body.clone(),
                                                                            ));
                                                                        cx.notify();
                                                                    },
                                                                ))
                                                                .child(
                                                                    svg()
                                                                        .path("icons/expand.svg")
                                                                        .size(px(13.5))
                                                                        .text_color(
                                                                            theme::text_muted(),
                                                                        ),
                                                                ),
                                                        ),
                                                ),
                                        )
                                        // Compact 1-row section stepper (no multi-line tab soup in narrow drawer!)
                                        .when(sec_count >= 2, |d| {
                                            let cur_label = match active_sec {
                                                Some(i) => format!(
                                                    "{}/{}  {}",
                                                    i + 1,
                                                    sec_count,
                                                    truncate_end(&sections[i].label, 24)
                                                ),
                                                None => format!("All {sec_count} sections"),
                                            };
                                            d.child(
                                                div()
                                                    .pb_2()
                                                    .border_b_1()
                                                    .border_color(theme::hairline())
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .id("bsec-all")
                                                            .min_w_0()
                                                            .overflow_hidden()
                                                            .px_2()
                                                            .py(px(3.0))
                                                            .rounded(theme::r_xs())
                                                            .when(active_sec.is_some(), |b| {
                                                                b.bg(theme::surface_subtle())
                                                            })
                                                            .hover(|s| s.bg(theme::row_hover()))
                                                            .cursor_pointer()
                                                            .text_xs()
                                                            .text_color(if active_sec.is_some() {
                                                                theme::text_primary()
                                                            } else {
                                                                theme::text_muted()
                                                            })
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.brief_section = None;
                                                                    this.brief_section_epoch =
                                                                        this.brief_section_epoch
                                                                            .wrapping_add(1);
                                                                    cx.notify();
                                                                },
                                                            ))
                                                            .child(cur_label),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex_shrink_0()
                                                            .flex()
                                                            .items_center()
                                                            .gap(px(2.0))
                                                            .child(
                                                                div()
                                                                    .id("bsec-prev")
                                                                    .w(px(22.0))
                                                                    .h(px(22.0))
                                                                    .rounded(theme::r_xs())
                                                                    .hover(|s| {
                                                                        s.bg(theme::row_hover())
                                                                            .text_color(
                                                                                theme::text_primary(),
                                                                            )
                                                                    })
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .cursor_pointer()
                                                                    .text_xs()
                                                                    .text_color(theme::text_muted())
                                                                    .on_click(cx.listener(
                                                                        move |this, _, _, cx| {
                                                                            this.brief_section =
                                                                                match this
                                                                                    .brief_section
                                                                                {
                                                                                    None
                                                                                    | Some(0) => {
                                                                                        Some(
                                                                                            sec_count
                                                                                                .saturating_sub(
                                                                                                    1,
                                                                                                ),
                                                                                        )
                                                                                    }
                                                                                    Some(i) => {
                                                                                        Some(i - 1)
                                                                                    }
                                                                                };
                                                                            this.brief_section_epoch =
                                                                                this.brief_section_epoch
                                                                                    .wrapping_add(1);
                                                                            cx.notify();
                                                                        },
                                                                    ))
                                                                    .child("‹"),
                                                            )
                                                            .child(
                                                                div()
                                                                    .id("bsec-next")
                                                                    .w(px(22.0))
                                                                    .h(px(22.0))
                                                                    .rounded(theme::r_xs())
                                                                    .hover(|s| {
                                                                        s.bg(theme::row_hover())
                                                                            .text_color(
                                                                                theme::text_primary(),
                                                                            )
                                                                    })
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .cursor_pointer()
                                                                    .text_xs()
                                                                    .text_color(theme::text_muted())
                                                                    .on_click(cx.listener(
                                                                        move |this, _, _, cx| {
                                                                            this.brief_section =
                                                                                match this
                                                                                    .brief_section
                                                                                {
                                                                                    None => Some(0),
                                                                                    Some(i)
                                                                                        if i + 1
                                                                                            < sec_count =>
                                                                                    {
                                                                                        Some(i + 1)
                                                                                    }
                                                                                    Some(_) => None,
                                                                                };
                                                                            this.brief_section_epoch =
                                                                                this.brief_section_epoch
                                                                                    .wrapping_add(1);
                                                                            cx.notify();
                                                                        },
                                                                    ))
                                                                    .child("›"),
                                                            ),
                                                    ),
                                            )
                                        })
                                        .child(md_block)
                                        .into_any_element()
                                }
                            };
                            let scroll_body = div()
                                .id("role-drawer-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .pt_1()
                                .pr_1()
                                .child(tab_body);
                            if self.reduce_motion {
                                scroll_body.into_any_element()
                            } else {
                                scroll_body
                                    .with_animation(
                                        SharedString::from(format!("dtab-fade-{tab_epoch}")),
                                        Animation::new(Duration::from_millis(130))
                                            .with_easing(ease_out_quint()),
                                        |el, t| el.opacity(t),
                                    )
                                    .into_any_element()
                            }
                        })
                        // Upgraded high-craft pinned bottom CTA
                        .child(
                            div()
                                .id("ask-role-chat")
                                .flex_shrink_0()
                                .py(px(9.0))
                                .px_3()
                                .rounded(theme::r_md())
                                .bg(theme::lavender_wash())
                                .border_1()
                                .border_color(theme::lavender_border())
                                .hover(|s| s.bg(theme::surface_active()))
                                .flex()
                                .items_center()
                                .justify_between()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.chat_open {
                                        this.chat_anim_epoch = this.chat_anim_epoch.wrapping_add(1);
                                    }
                                    this.chat_open = true;
                                    this.attached_role =
                                        Some((role_company.clone(), role_path_for_chat.clone()));
                                    this.active_input = ActiveInput::Chat;
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_primary())
                                        .child("Ask in Chat about this role"),
                                )
                                .child(div().text_xs().text_color(theme::lavender()).child("→")),
                        )
                        .into_any_element(),
                )
            }
            RightDrawer::Profile => {
                let groups = [
                    "Defaults & screening",
                    "Career context",
                    "Projects & evidence",
                ];
                let ws_path = self
                    .client
                    .target
                    .local_root()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| self.client.target.label());
                Some(
                    div()
                        .id("profile-drawer")
                        .w_full()
                        .flex_shrink_0()
                        .h_full()
                        .p_4()
                        .bg(theme::panel_glass())
                        .border_l_1()
                        .border_color(theme::hairline())
                        .flex()
                        .flex_col()
                        .gap_3()
                        .overflow_hidden()
                        .child(drawer_header(
                            "Profile & Career Context",
                            Some(
                                "Canonical facts & defaults shared across every connected agent."
                                    .into(),
                            ),
                            cx,
                        ))
                        .child(
                            div()
                                .id("profile-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_4()
                                // Inline editable personal defaults (canonical config/career-ledger.json shared by all agents)
                                .child({
                                    let ps = &self.snapshot.profile_settings;
                                    let cur = if ps.currency.is_empty() {
                                        "GBP"
                                    } else {
                                        ps.currency.as_str()
                                    };
                                    div()
                                        .pb_3()
                                        .border_b_1()
                                        .border_color(theme::hairline())
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child("Personal defaults (click to edit)"),
                                        )
                                        .child(self.render_editable_kv(
                                            "application_email",
                                            "Application email",
                                            if ps.application_email.is_empty() {
                                                if ps.sender_email.is_empty() {
                                                    "Set email".into()
                                                } else {
                                                    format!("Same as reply ({})", truncate_label(&ps.sender_email, 16))
                                                }
                                            } else {
                                                truncate_label(&ps.application_email, 26)
                                            },
                                            ps.application_email.clone(),
                                            "cv@domain.com (forms & portals)",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "sender_email",
                                            "Reply email",
                                            if ps.sender_email.is_empty() {
                                                "Set reply email".into()
                                            } else {
                                                truncate_label(&ps.sender_email, 26)
                                            },
                                            ps.sender_email.clone(),
                                            "you@domain.com (connected inbox)",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "timezone",
                                            "Timezone",
                                            ps.timezone.clone(),
                                            ps.timezone.clone(),
                                            "Europe/London",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "notice_period",
                                            "Notice period",
                                            truncate_end(
                                                if ps.notice_period.is_empty() {
                                                    "Set notice period"
                                                } else {
                                                    &ps.notice_period
                                                },
                                                26,
                                            ),
                                            ps.notice_period.clone(),
                                            "1 month / Immediate",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "min_base_salary",
                                            "Min salary",
                                            format!("{cur} {}", ps.min_base_salary),
                                            ps.min_base_salary.to_string(),
                                            "80000",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "target_base_salary",
                                            "Target salary",
                                            format!("{cur} {}", ps.target_base_salary),
                                            ps.target_base_salary.to_string(),
                                            "100000",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "location_preferences",
                                            "Location",
                                            truncate_end(
                                                if ps.location_preferences.is_empty() {
                                                    "Set preference"
                                                } else {
                                                    &ps.location_preferences
                                                },
                                                26,
                                            ),
                                            ps.location_preferences.clone(),
                                            "UK remote, London hybrid",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "target_roles",
                                            "Target roles",
                                            truncate_end(
                                                if ps.target_roles.is_empty() {
                                                    "Set target roles"
                                                } else {
                                                    &ps.target_roles
                                                },
                                                26,
                                            ),
                                            ps.target_roles.clone(),
                                            "Senior Backend, Product, AI",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "discovery_sources",
                                            "Job boards",
                                            truncate_end(
                                                if ps.discovery_sources.is_empty() {
                                                    "employer ATS, HN, Cord"
                                                } else {
                                                    &ps.discovery_sources
                                                },
                                                26,
                                            ),
                                            ps.discovery_sources.clone(),
                                            "employer ATS, HN, Cord, LinkedIn",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "avoid_list",
                                            "Avoid list",
                                            truncate_end(
                                                if ps.avoid_list.is_empty() {
                                                    "Companies or sectors to skip"
                                                } else {
                                                    &ps.avoid_list
                                                },
                                                26,
                                            ),
                                            ps.avoid_list.clone(),
                                            "e.g. gambling, defence, agencies",
                                            cx,
                                        ))
                                        .child(self.render_editable_kv(
                                            "tone",
                                            "Voice & tone",
                                            truncate_end(
                                                if ps.tone.is_empty() {
                                                    "Set writing voice"
                                                } else {
                                                    &ps.tone
                                                },
                                                26,
                                            ),
                                            ps.tone.clone(),
                                            "direct, concise, plain-language",
                                            cx,
                                        ))
                                        .child({
                                            let draft_on =
                                                ps.post_rejection_reply == "draft_after_interview";
                                            div()
                                                .py(px(3.0))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme::text_muted())
                                                        .child("Post-interview rejection"),
                                                )
                                                .child(
                                                    div()
                                                        .id("prof-toggle-post-rej")
                                                        .px(px(7.0))
                                                        .py(px(2.0))
                                                        .rounded(theme::r_xs())
                                                        .bg(theme::surface_subtle())
                                                        .hover(|s| s.bg(theme::row_hover()))
                                                        .cursor_pointer()
                                                        .text_size(px(11.0))
                                                        .text_color(if draft_on {
                                                            theme::emerald()
                                                        } else {
                                                            theme::text_secondary()
                                                        })
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                if draft_on {
                                                                    this.set_communication_enum(
                                                                        "post_rejection_reply",
                                                                        "off",
                                                                        "Post-interview rejection drafts off",
                                                                        cx,
                                                                    );
                                                                } else {
                                                                    this.set_communication_enum(
                                                                        "post_rejection_reply",
                                                                        "draft_after_interview",
                                                                        "Will draft thank-you + feedback ask after interviews (never auto-sent)",
                                                                        cx,
                                                                    );
                                                                }
                                                            },
                                                        ))
                                                        .child(if draft_on {
                                                            "Draft thank-you"
                                                        } else {
                                                            "Off"
                                                        }),
                                                )
                                        })
                                })
                                .children(groups.into_iter().filter_map(|grp| {
                                    let items: Vec<_> = self
                                        .snapshot
                                        .profile_docs
                                        .iter()
                                        .filter(|d| d.group == grp)
                                        .cloned()
                                        .collect();
                                    if items.is_empty() {
                                        return None;
                                    }
                                    Some(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .pb_1()
                                                    .text_xs()
                                                    .text_color(theme::text_muted())
                                                    .child(grp),
                                            )
                                            .children(items.into_iter().enumerate().map(
                                                |(idx, doc)| {
                                                    let rel = doc.relative_path.clone();
                                                    let full_open = doc.full_path.clone();
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "prof-{grp}-{idx}"
                                                        )))
                                                        .px_2()
                                                        .py(px(6.0))
                                                        .mx(px(-8.0))
                                                        .rounded(theme::r_sm())
                                                        .hover(|s| s.bg(theme::row_hover()))
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .id(SharedString::from(format!(
                                                                    "prof-read-{grp}-{idx}"
                                                                )))
                                                                .flex_1()
                                                                .min_w_0()
                                                                .cursor_pointer()
                                                                .flex()
                                                                .flex_col()
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        this.open_file_in_reader_async(
                                                                            rel.clone(),
                                                                            cx,
                                                                        );
                                                                    },
                                                                ))
                                                                .child(
                                                                    div()
                                                                        .text_xs()
                                                                        .text_color(
                                                                            theme::text_primary(),
                                                                        )
                                                                        .child(doc.label),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_size(px(11.0))
                                                                        .text_color(
                                                                            theme::text_muted(),
                                                                        )
                                                                        .child(truncate_label(
                                                                            &doc.relative_path,
                                                                            36,
                                                                        )),
                                                                ),
                                                        )
                                                        .when_some(full_open, |r, p| {
                                                            r.child(
                                                                div()
                                                                    .id(SharedString::from(format!(
                                                                        "prof-edit-{grp}-{idx}"
                                                                    )))
                                                                    .p(px(5.0))
                                                                    .rounded(theme::r_sm())
                                                                    .hover(|s| {
                                                                        s.bg(theme::surface_active())
                                                                    })
                                                                    .cursor_pointer()
                                                                    .on_click(cx.listener(
                                                                        move |_, _, _, _| {
                                                                            open_in_system(&p);
                                                                        },
                                                                    ))
                                                                    .child(
                                                                        svg()
                                                                            .path(
                                                                                "icons/external-link.svg",
                                                                            )
                                                                            .size(px(13.0))
                                                                            .text_color(
                                                                                theme::text_muted(),
                                                                            ),
                                                                    ),
                                                            )
                                                        })
                                                },
                                            )),
                                    )
                                }))
                                .child(
                                    div()
                                        .pt_3()
                                        .border_t_1()
                                        .border_color(theme::hairline())
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child("Connected agents & ledger"),
                                        )
                                        .child({
                                            let local_dir = self
                                                .client
                                                .target
                                                .local_root()
                                                .map(PathBuf::from);
                                            let copy_ws = ws_path.clone();
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_3()
                                                .text_xs()
                                                .child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .text_color(theme::text_muted())
                                                        .child("Workspace"),
                                                )
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .flex()
                                                        .items_center()
                                                        .gap_1()
                                                        .child({
                                                            let has_dir = local_dir.is_some();
                                                            div()
                                                                .id("profile-open-ws")
                                                                .min_w_0()
                                                                .overflow_hidden()
                                                                .text_color(theme::text_secondary())
                                                                .when(has_dir, |d| {
                                                                    let p = local_dir.unwrap();
                                                                    d.underline()
                                                                        .cursor_pointer()
                                                                        .hover(|s| {
                                                                            s.text_color(
                                                                                theme::text_primary(),
                                                                            )
                                                                        })
                                                                        .on_click(cx.listener(
                                                                            move |_, _, _, _| {
                                                                                open_in_system(&p);
                                                                            },
                                                                        ))
                                                                })
                                                                .child(truncate_label(&ws_path, 24))
                                                        })
                                                        .child(
                                                            div()
                                                                .id("profile-copy-ws")
                                                                .p(px(3.0))
                                                                .rounded(theme::r_xs())
                                                                .hover(|s| {
                                                                    s.bg(theme::row_hover())
                                                                })
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        cx.write_to_clipboard(
                                                                            ClipboardItem::new_string(
                                                                                copy_ws.clone(),
                                                                            ),
                                                                        );
                                                                        this.flash_copied("ws-copy", cx);
                                                                        this.set_toast(
                                                                            "Copied workspace path",
                                                                            cx,
                                                                        );
                                                                    },
                                                                ))
                                                                .child({
                                                                    let copied = self.is_copied_flash("ws-copy");
                                                                    svg()
                                                                        .path(if copied {
                                                                            "icons/check.svg"
                                                                        } else {
                                                                            "icons/copy.svg"
                                                                        })
                                                                        .size(px(12.0))
                                                                        .text_color(if copied {
                                                                            theme::emerald()
                                                                        } else {
                                                                            theme::text_muted()
                                                                        })
                                                                }),
                                                        ),
                                                )
                                        })
                                        .child(kv_row(
                                            "Active agent",
                                            self.runtime.label(),
                                        ))
                                        .child(kv_row(
                                            "Detected CLIs",
                                            self.available_runtimes
                                                .iter()
                                                .map(|r| r.short_name())
                                                .collect::<Vec<_>>()
                                                .join(", "),
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .id("profile-ask-chat")
                                .flex_shrink_0()
                                .py(px(9.0))
                                .px_3()
                                .rounded(theme::r_md())
                                .bg(theme::lavender_wash())
                                .border_1()
                                .border_color(theme::lavender_border())
                                .hover(|s| s.bg(theme::surface_active()))
                                .flex()
                                .items_center()
                                .justify_between()
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if !this.chat_open {
                                        this.chat_anim_epoch = this.chat_anim_epoch.wrapping_add(1);
                                    }
                                    this.chat_open = true;
                                    this.chat_input =
                                        "Update my screening defaults or profile context: ".into();
                                    this.active_input = ActiveInput::Chat;
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_primary())
                                        .child("Update profile via Chat"),
                                )
                                .child(div().text_xs().text_color(theme::lavender()).child("→")),
                        )
                        .into_any_element(),
                )
            }
            RightDrawer::Cvs => Some(
                div()
                    .id("cv-drawer")
                    .w_full()
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
                    .child(drawer_header(
                        "CVs & Tracks",
                        Some("Master CV and tailored role tracks.".into()),
                        cx,
                    ))
                    // Borderless Master -> Children Tree
                    .children(self.snapshot.cv_nodes.iter().enumerate().map(|(i, node)| {
                        let full = node.full_path.clone();
                        let full_reveal = node.full_path.clone();
                        let copy_str = node
                            .full_path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| node.relative_path.clone());
                        let rel_for_replace = node.relative_path.clone();
                        let is_master = node.is_master;
                        let is_enabled = node.enabled;
                        let track_for_toggle = node.name.clone();
                        let prefix = if is_master { "" } else { "└ " };
                        let roles_note = node
                            .matched_roles
                            .map(|n| format!("{n} recent roles"));

                        div()
                            .pl(if is_master { px(0.0) } else { px(14.0) })
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
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(if is_enabled {
                                                        theme::text_primary()
                                                    } else {
                                                        theme::text_muted()
                                                    })
                                                    .child(format!("{prefix}{}", node.name)),
                                            )
                                            .when_some(roles_note, |d, note| {
                                                d.child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme::text_muted())
                                                        .child(note),
                                                )
                                            }),
                                    )
                                    // Master keeps quiet status dot; child tracks get a subtle clean toggle right in that slot
                                    .child(if is_master {
                                        div()
                                            .w(px(7.0))
                                            .h(px(7.0))
                                            .rounded(px(99.0))
                                            .bg(if node.exists {
                                                theme::emerald()
                                            } else {
                                                theme::amber_accent()
                                            })
                                            .into_any_element()
                                    } else {
                                        div()
                                            .id(SharedString::from(format!("cv-toggle-{i}")))
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.toggle_cv_track(track_for_toggle.clone(), cx);
                                            }))
                                            .child(switch_pill(is_enabled))
                                            .into_any_element()
                                    }),
                            )
                            // Clickable underlined path reveals the PDF in Finder + inline Copy icon
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("cv-path-{i}")))
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .underline()
                                            .hover(|s| s.text_color(theme::text_primary()))
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if let Some(p) = &full_reveal {
                                                    reveal_in_file_manager(p);
                                                } else {
                                                    this.set_toast("Reveal requires local workspace", cx);
                                                }
                                            }))
                                            .child(truncate_label(&node.relative_path, 34)),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("cv-copy-{i}")))
                                            .p(px(4.0))
                                            .rounded(theme::r_xs())
                                            .hover(|s| s.bg(theme::row_hover()))
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy_str.clone(),
                                                ));
                                                this.flash_copied(format!("cv-copy-{i}"), cx);
                                                this.set_toast("Copied CV path", cx);
                                            }))
                                            .child({
                                                let copied = self.is_copied_flash(&format!("cv-copy-{i}"));
                                                svg()
                                                    .path(if copied {
                                                        "icons/check.svg"
                                                    } else {
                                                        "icons/copy.svg"
                                                    })
                                                    .size(px(13.0))
                                                    .text_color(if copied {
                                                        theme::emerald()
                                                    } else {
                                                        theme::text_muted()
                                                    })
                                            }),
                                    ),
                            )
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
                                            .child("Open PDF ↗"),
                                    )
                                    .when(!is_master, |d| {
                                        d.child(
                                            div()
                                                .id(SharedString::from(format!("cv-replace-{i}")))
                                                .text_xs()
                                                .text_color(theme::text_secondary())
                                                .hover(|s| s.text_color(theme::text_primary()))
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.replace_track_cv_dialog(
                                                        rel_for_replace.clone(),
                                                        cx,
                                                    );
                                                }))
                                                .child("Replace"),
                                        )
                                    }),
                            )
                    }))
                    .when(self.snapshot.suggest_new_track, |d| {
                        let samples: Vec<String> = self
                            .snapshot
                            .unmatched_track_titles
                            .iter()
                            .take(3)
                            .cloned()
                            .collect();
                        d.child(
                            div()
                                .pt_3()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .w(px(6.0))
                                                .h(px(6.0))
                                                .rounded(px(99.0))
                                                .bg(theme::amber_accent()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_primary())
                                                .child("Roles outside current tracks"),
                                        ),
                                )
                                .children(samples.into_iter().map(|title| {
                                    div()
                                        .pl_3()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(title.to_string())
                                }))
                                .child(
                                    div()
                                        .id("cv-draft-track")
                                        .pt_1()
                                        .text_xs()
                                        .text_color(theme::lavender())
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.submit_prompt(
                                                "Review roles outside my current CV tracks and suggest whether a new child track makes sense.".into(),
                                                false,
                                                cx,
                                            );
                                        }))
                                        .child("Review in Chat →"),
                                ),
                        )
                    })
                    .child({
                        let mode = self.snapshot.profile_settings.cover_letter_mode.clone();
                        let cl_enabled = mode != "never";
                        let tpl_rel = if self
                            .snapshot
                            .profile_settings
                            .cover_letter_template
                            .trim()
                            .is_empty()
                        {
                            "context/profile/cover-letter-template.md".to_owned()
                        } else {
                            self.snapshot
                                .profile_settings
                                .cover_letter_template
                                .trim()
                                .to_owned()
                        };
                        let tpl_exists = self
                            .client
                            .target
                            .local_root()
                            .is_some_and(|r| r.join(&tpl_rel).is_file());
                        let modes: [(&'static str, &'static str, &'static str); 2] = [
                            (
                                "only_when_required",
                                "When required",
                                "Cover letters: only when portal requires one",
                            ),
                            ("always", "Always", "Cover letters: draft for every application"),
                        ];
                        div()
                            .pt_3()
                            .border_t_1()
                            .border_color(theme::hairline())
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if cl_enabled {
                                                theme::text_primary()
                                            } else {
                                                theme::text_muted()
                                            })
                                            .child("Cover letter"),
                                    )
                                    .child(
                                        div()
                                            .id("cl-master-toggle")
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if cl_enabled {
                                                    this.set_communication_enum(
                                                        "cover_letter_mode",
                                                        "never",
                                                        "Cover letters skipped",
                                                        cx,
                                                    );
                                                } else {
                                                    this.set_communication_enum(
                                                        "cover_letter_mode",
                                                        "only_when_required",
                                                        "Cover letters enabled when required",
                                                        cx,
                                                    );
                                                }
                                            }))
                                            .child(switch_pill(cl_enabled)),
                                    ),
                            )
                            .when(cl_enabled, |d| {
                                d.child(
                                    div()
                                        .p(px(2.0))
                                        .rounded(theme::r_md())
                                        .bg(theme::window_glass())
                                        .border_1()
                                        .border_color(theme::hairline())
                                        .flex()
                                        .items_center()
                                        .gap(px(2.0))
                                        .children(modes.into_iter().map(|(val, lbl, toast)| {
                                            let active = mode == val;
                                            div()
                                                .id(SharedString::from(format!("cl-mode-{val}")))
                                                .flex_1()
                                                .py(px(4.0))
                                                .rounded(theme::r_sm())
                                                .bg(if active {
                                                    theme::surface_active()
                                                } else {
                                                    gpui::rgba(0x00000000)
                                                })
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_size(px(11.0))
                                                .text_color(if active {
                                                    theme::text_primary()
                                                } else {
                                                    theme::text_muted()
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.set_communication_enum(
                                                        "cover_letter_mode",
                                                        val,
                                                        toast,
                                                        cx,
                                                    );
                                                }))
                                                .child(lbl)
                                        })),
                                )
                                .child(
                                    div()
                                        .id("cl-template-row")
                                        .py(px(4.0))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .rounded(theme::r_sm())
                                        .px_1()
                                        .mx(px(-4.0))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.open_or_create_cover_letter_template(cx);
                                        }))
                                        .child(
                                            div()
                                                .text_size(px(11.0))
                                                .text_color(theme::text_secondary())
                                                .child(if tpl_exists {
                                                    "Cover letter template"
                                                } else {
                                                    "No template (optional skipped)"
                                                }),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.0))
                                                .text_color(theme::lavender())
                                                .child(if tpl_exists {
                                                    "Edit ↗"
                                                } else {
                                                    "Create"
                                                }),
                                        ),
                                )
                            })
                    })
                    .child(
                        div()
                            .id("import-primary-cv")
                            .mt_auto()
                            .py_2()
                            .rounded(theme::r_sm())
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
            RightDrawer::Notifications => {
                let tidy_n = self.tidy_count();
                let latest_at = self.snapshot.latest_activity_at.clone();
                Some(
                    div()
                        .id("notifications-drawer")
                        .w_full()
                        .flex_shrink_0()
                        .h_full()
                        .p_4()
                        .bg(theme::panel_glass())
                        .border_l_1()
                        .border_color(theme::hairline())
                        .flex()
                        .flex_col()
                        .gap_4()
                        .overflow_y_scroll()
                        .child(drawer_header(
                            "Notifications & Activity",
                            Some(
                                "Derived live from your ledger — items clear automatically once resolved."
                                    .into(),
                            ),
                            cx,
                        ))
                        // Section 1: Live Attention Queue (state-driven, stays visible until resolved)
                        .child(
                            div()
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
                                                .text_color(theme::text_muted())
                                                .child("Needs attention now"),
                                        )
                                        .when(tidy_n > 0, |d| {
                                            d.child(
                                                div()
                                                    .id("notif-open-tidy")
                                                    .text_xs()
                                                    .text_color(theme::lavender())
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.open_right_drawer(
                                                            RightDrawer::Tidy,
                                                            cx,
                                                        );
                                                    }))
                                                    .child(format!("Tidy ({tidy_n}) →")),
                                            )
                                        }),
                                )
                                .when(self.snapshot.attention_items.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .py_2()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .child("Nothing urgent waiting on you right now."),
                                    )
                                })
                                .children(
                                    self.snapshot
                                        .attention_items
                                        .iter()
                                        .take(10)
                                        .enumerate()
                                        .map(|(idx, item)| {
                                            let is_urg = item.urgency == "urgent_now";
                                            let is_long_quiet = item
                                                .detail
                                                .strip_prefix("Quiet ")
                                                .and_then(|s| s.split('d').next())
                                                .and_then(|n| n.parse::<u64>().ok())
                                                .is_some_and(|d| d >= 14);
                                            let dot_color = if is_urg {
                                                theme::amber_accent()
                                            } else if is_long_quiet {
                                                theme::rose_text()
                                            } else {
                                                theme::text_muted()
                                            };
                                            let company_only = item
                                                .company
                                                .split(" — ")
                                                .next()
                                                .unwrap_or(&item.company);
                                            let path_opt = item.record_path.clone();
                                            let long_detail = item.detail.len() > 26;
                                            let detail_col = if is_long_quiet {
                                                theme::rose_text()
                                            } else {
                                                theme::text_muted()
                                            };
                                            let row = div()
                                                .id(SharedString::from(format!("att-{idx}")))
                                                .flex_shrink_0()
                                                .px_2()
                                                .py_2()
                                                .mx(px(-8.0))
                                                .rounded(theme::r_sm())
                                                .when(idx > 0, |d| {
                                                    d.border_t_1().border_color(theme::hairline())
                                                })
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if let Some(p) = &path_opt {
                                                        this.open_citation(p, cx);
                                                    }
                                                }));
                                            let header_left = div()
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
                                                        .bg(dot_color),
                                                )
                                                .child(
                                                    div()
                                                        .whitespace_nowrap()
                                                        .overflow_hidden()
                                                        .text_xs()
                                                        .text_color(if is_urg {
                                                            theme::amber_text()
                                                        } else {
                                                            theme::text_primary()
                                                        })
                                                        .child(truncate_end(company_only, 24)),
                                                );
                                            if long_detail {
                                                row.flex()
                                                    .flex_col()
                                                    .gap(px(3.0))
                                                    .child(header_left)
                                                    .child(
                                                        div()
                                                            .pl(px(14.0))
                                                            .text_size(px(11.0))
                                                            .text_color(detail_col)
                                                            .child(item.detail.clone()),
                                                    )
                                            } else {
                                                row.flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_3()
                                                    .child(header_left)
                                                    .child(
                                                        div()
                                                            .flex_shrink_0()
                                                            .whitespace_nowrap()
                                                            .text_xs()
                                                            .text_color(detail_col)
                                                            .child(item.detail.clone()),
                                                    )
                                            }
                                        }),
                                ),
                        )
                        // Section 2: Persisted Agent Digests & Ledger Event Replay Feed
                        .child(
                            div()
                                .pt_3()
                                .border_t_1()
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
                                                .text_color(theme::text_muted())
                                                .child("Recent activity & digests"),
                                        )
                                        .child(
                                            div()
                                                .id("notif-catchup-chat")
                                                .text_xs()
                                                .text_color(theme::lavender())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    let prompt = match &latest_at {
                                                        Some(ts) if !ts.is_empty() => format!(
                                                            "Catch me up on recent inbox, calendar, and ledger updates (latest recorded event at {ts})."
                                                        ),
                                                        _ => "Run a quick morning check and summarize what changed.".into(),
                                                    };
                                                    this.submit_prompt(prompt, false, cx);
                                                }))
                                                .child("Catch up in Chat →"),
                                        ),
                                )
                                .when(self.snapshot.recent_activity.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .py_2()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .child("No recorded run digests yet."),
                                    )
                                })
                                .children(
                                    self.snapshot
                                        .recent_activity
                                        .iter()
                                        .enumerate()
                                        .map(|(i, act)| {
                                            let path_opt = act.record_path.clone();
                                            let summary_copy = act.summary.clone();
                                            let at_copy = act.at.clone();
                                            let is_human = act.is_human;
                                            let has_modal_detail =
                                                path_opt.is_some() || act.summary.len() > 120;
                                            div()
                                                .id(SharedString::from(format!("act-ev-{i}")))
                                                .px_2()
                                                .py_2()
                                                .mx(px(-8.0))
                                                .rounded(theme::r_sm())
                                                .when(i > 0, |d| {
                                                    d.border_t_1().border_color(theme::hairline())
                                                })
                                                .when(has_modal_detail, |d| {
                                                    d.hover(|s| s.bg(theme::row_hover()))
                                                        .cursor_pointer()
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                if let Some(p) = &path_opt {
                                                                    this.open_citation(p, cx);
                                                                } else {
                                                                    this.expanded_reader = Some((
                                                                        format!("Digest ({at_copy})"),
                                                                        summary_copy.clone(),
                                                                    ));
                                                                    cx.notify();
                                                                }
                                                            },
                                                        ))
                                                })
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .gap_2()
                                                        .text_xs()
                                                        .child(
                                                            div()
                                                                .flex()
                                                                .items_center()
                                                                .gap(px(6.0))
                                                                .child(
                                                                    div()
                                                                        .w(px(5.5))
                                                                        .h(px(5.5))
                                                                        .rounded(px(99.0))
                                                                        .bg(if is_human {
                                                                            theme::emerald()
                                                                        } else {
                                                                            theme::lavender()
                                                                        }),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_size(px(10.5))
                                                                        .font_weight(
                                                                            FontWeight::MEDIUM,
                                                                        )
                                                                        .text_color(if is_human {
                                                                            theme::emerald()
                                                                        } else {
                                                                            theme::lavender()
                                                                        })
                                                                        .child(if is_human {
                                                                            "You"
                                                                        } else {
                                                                            "Agent"
                                                                        }),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_color(
                                                                            theme::text_muted(),
                                                                        )
                                                                        .child(act.tool.clone()),
                                                                ),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_color(theme::text_muted())
                                                                .child(act.age_label.clone()),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .line_height(px(17.5))
                                                        .text_color(theme::text_secondary())
                                                        .child(if has_modal_detail {
                                                            truncate_end(&act.summary, 140)
                                                        } else {
                                                            act.summary.clone()
                                                        }),
                                                )
                                        }),
                                ),
                        )
                        .into_any_element(),
                )
            }
            RightDrawer::Settings => {
                let p = &self.snapshot.permissions;
                let info = self.expanded_info;
                Some(
                    div()
                        .id("settings-drawer")
                        .w_full()
                        .flex_shrink_0()
                        .h_full()
                        .p_4()
                        .bg(theme::panel_glass())
                        .border_l_1()
                        .border_color(theme::hairline())
                        .flex()
                        .flex_col()
                        .gap_4()
                        .overflow_y_scroll()
                        .track_scroll(&self.settings_scroll)
                        .child(drawer_header(
                            "Settings",
                            Some("Agent runtime, automation rules, and interface preferences.".into()),
                            cx,
                        ))
                        // Section 1: Inference & runtime (model, speed, and zero-friction CLI sign-in)
                        .child(
                            div()
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
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme::text_primary())
                                                .child("Inference & runtime"),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(10.5))
                                                .text_color(theme::text_muted())
                                                .child(format!(
                                                    "{} ready",
                                                    self.available_runtimes.len()
                                                )),
                                        ),
                                )
                                .child(self.render_agent_controls("set", false, cx))
                                .child({
                                    let open = self.show_connect_help;
                                    let connect_rows: [(&str, &str, &str); 5] = [
                                        (
                                            "ChatGPT (Codex CLI)",
                                            "codex login",
                                            "codex login",
                                        ),
                                        ("Claude Code", "claude", "claude"),
                                        ("Pi coding agent", "pi auth login", "pi auth login"),
                                        (
                                            "Hermes / local models",
                                            "hermes model",
                                            "hermes model",
                                        ),
                                        (
                                            "Auto-setup via any agent",
                                            "Copy 1-line setup prompt",
                                            "Read https://raw.githubusercontent.com/machineyearningengineer/job-harness/main/docs/AGENT-SETUP.md and set up Job Harness for me.",
                                        ),
                                    ];
                                    div()
                                        .pt_1()
                                        .flex()
                                        .flex_col()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .id("set-toggle-connect-help")
                                                .px_2()
                                                .py(px(5.0))
                                                .rounded(theme::r_sm())
                                                .bg(theme::surface_subtle())
                                                .border_1()
                                                .border_color(theme::hairline())
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.show_connect_help =
                                                        !this.show_connect_help;
                                                    cx.notify();
                                                }))
                                                .child(
                                                    div()
                                                        .text_size(px(11.5))
                                                        .text_color(theme::text_secondary())
                                                        .child("Connect ChatGPT, Claude, or local"),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(10.5))
                                                        .text_color(theme::text_muted())
                                                        .child(if open { "▴" } else { "▾" }),
                                                ),
                                        )
                                        .when(open, |box_d| {
                                            box_d.child(
                                                div()
                                                    .p_2()
                                                    .rounded(theme::r_sm())
                                                    .bg(theme::window_glass())
                                                    .border_1()
                                                    .border_color(theme::hairline())
                                                    .flex()
                                                    .flex_col()
                                                    .gap(px(6.0))
                                                    .child(
                                                        div()
                                                            .text_size(px(10.5))
                                                            .text_color(theme::text_muted())
                                                            .child("Sign in once via your CLI (OAuth or local server) — no API keys stored in Job Harness."),
                                                    )
                                                    .children(connect_rows.into_iter().enumerate().map(|(ci, (title, hint, cmd))| {
                                                        div()
                                                            .id(SharedString::from(format!("set-conn-{ci}")))
                                                            .px_2()
                                                            .py(px(5.0))
                                                            .rounded(theme::r_sm())
                                                            .bg(theme::surface_subtle())
                                                            .hover(|s| s.bg(theme::row_hover()))
                                                            .cursor_pointer()
                                                            .flex()
                                                            .items_center()
                                                            .justify_between()
                                                            .gap_2()
                                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                                cx.write_to_clipboard(ClipboardItem::new_string(cmd.to_owned()));
                                                                this.set_toast(format!("Copied `{hint}`"), cx);
                                                            }))
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .flex_col()
                                                                    .min_w_0()
                                                                    .child(
                                                                        div()
                                                                            .text_size(px(11.0))
                                                                            .text_color(theme::text_primary())
                                                                            .child(title),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_size(px(10.0))
                                                                            .text_color(theme::text_muted())
                                                                            .child(hint),
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex_shrink_0()
                                                                    .text_size(px(10.5))
                                                                    .text_color(theme::text_secondary())
                                                                    .child("Copy"),
                                                            )
                                                    })),
                                            )
                                        })
                                }),
                        )
                        // Section 2: Delegation & Automation rules (3-state preset + fine-grained overrides, stored in config/career-ledger.json)
                        .child({
                            let active_preset = if !p.auto_apply_portal
                                && !p.auto_send_first_contact_email
                                && !p.auto_reply_threads
                                && !p.auto_archive_mail
                            {
                                Some("drafts")
                            } else if p.auto_apply_portal
                                && !p.auto_send_first_contact_email
                                && !p.auto_reply_threads
                                && p.auto_archive_mail
                            {
                                Some("assisted")
                            } else if p.auto_apply_portal
                                && p.auto_send_first_contact_email
                                && p.auto_reply_threads
                                && p.auto_archive_mail
                            {
                                Some("autonomous")
                            } else {
                                None
                            };
                            let preset_desc = match active_preset {
                                Some("drafts") => {
                                    "Prepares packets and saves Gmail drafts only — you submit and send."
                                }
                                Some("assisted") => {
                                    "Recommended: auto-submits clean ATS portals in your cap and archives verified receipts; cold emails & replies stay drafts."
                                }
                                Some("autonomous") => {
                                    "Submits eligible portals, sends verified cold emails & routine replies, and archives receipts."
                                }
                                _ => "Custom mix of fine-grained rules below.",
                            };
                            let presets: [(&'static str, &'static str); 3] = [
                                ("drafts", "Drafts only"),
                                ("assisted", "Assisted"),
                                ("autonomous", "Autonomous"),
                            ];
                            div()
                                .pt_3()
                                .border_t_1()
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
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme::text_primary())
                                                .child("Delegation"),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(10.5))
                                                .text_color(if active_preset == Some("assisted") {
                                                    theme::emerald()
                                                } else {
                                                    theme::text_muted()
                                                })
                                                .child(match active_preset {
                                                    Some("assisted") => "Recommended",
                                                    None => "Custom",
                                                    _ => "",
                                                }),
                                        ),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .p(px(2.0))
                                        .rounded(theme::r_sm())
                                        .bg(theme::surface_subtle())
                                        .flex()
                                        .items_center()
                                        .gap(px(2.0))
                                        .children(presets.into_iter().map(|(id, label)| {
                                            let on = active_preset == Some(id);
                                            div()
                                                .id(SharedString::from(format!("del-mode-{id}")))
                                                .flex_1()
                                                .py(px(4.5))
                                                .rounded(theme::r_sm())
                                                .bg(if on {
                                                    theme::surface_active()
                                                } else {
                                                    gpui::rgba(0x00000000)
                                                })
                                                .hover(|s| {
                                                    if on { s } else { s.bg(theme::row_hover()) }
                                                })
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_xs()
                                                .font_weight(if on {
                                                    FontWeight::MEDIUM
                                                } else {
                                                    FontWeight::NORMAL
                                                })
                                                .text_color(if on {
                                                    theme::text_primary()
                                                } else {
                                                    theme::text_muted()
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.set_delegation_preset(id, cx);
                                                }))
                                                .child(label)
                                        })),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .line_height(px(16.0))
                                        .text_color(theme::text_muted())
                                        .child(preset_desc),
                                )
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
                                        .pt_1()
                                        .text_size(px(11.0))
                                        .line_height(px(16.0))
                                        .text_color(theme::text_muted())
                                        .child("Always asks you first: booking slots, live interviews, offers, CAPTCHAs, and secrets."),
                                )
                        })
                        // Section 3: Interface & Timing
                        .child(
                            div()
                                .pt_3()
                                .border_t_1()
                                .border_color(theme::hairline())
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::text_primary())
                                        .child("Interface & timing"),
                                )
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
                                                        .child("Keep Mac awake during runs"),
                                                )
                                                .child(
                                                    div()
                                                        .id("info-keep_awake")
                                                        .px(px(4.0))
                                                        .rounded(theme::r_xs())
                                                        .cursor_pointer()
                                                        .hover(|s| {
                                                            s.text_color(theme::text_primary())
                                                        })
                                                        .text_xs()
                                                        .text_color(
                                                            if info == Some("keep_awake") {
                                                                theme::lavender()
                                                            } else {
                                                                theme::text_muted()
                                                            },
                                                        )
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.expanded_info = if this
                                                                .expanded_info
                                                                == Some("keep_awake")
                                                            {
                                                                None
                                                            } else {
                                                                Some("keep_awake")
                                                            };
                                                            cx.notify();
                                                        }))
                                                        .child("ⓘ"),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("toggle-keep-awake")
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.agent_opts.keep_awake =
                                                        !this.agent_opts.keep_awake;
                                                    let on = this.agent_opts.keep_awake;
                                                    this.set_toast(
                                                        if on {
                                                            "Mac stays awake while an agent turn is running"
                                                        } else {
                                                            "Normal Mac sleep restored"
                                                        },
                                                        cx,
                                                    );
                                                }))
                                                .child(switch_pill(self.agent_opts.keep_awake)),
                                        ),
                                )
                                .when(info == Some("keep_awake"), |d| {
                                    d.child(
                                        div()
                                            .pb_1()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .child("Uses macOS caffeinate only while a run is active; releases immediately when the turn ends."),
                                    )
                                })
                                .child({
                                    let auth = crate::macos_notify::auth_status();
                                    let (action_lbl, action_col, action_kind) = match auth {
                                        crate::macos_notify::NotificationAuthStatus::Authorized => {
                                            ("Test", theme::text_muted(), 0u8)
                                        }
                                        crate::macos_notify::NotificationAuthStatus::NotDetermined => {
                                            ("Enable", theme::amber_text(), 1u8)
                                        }
                                        crate::macos_notify::NotificationAuthStatus::Denied => {
                                            ("System Settings", theme::amber_text(), 2u8)
                                        }
                                        crate::macos_notify::NotificationAuthStatus::Unbundled => {
                                            ("App bundle only", theme::text_muted(), 3u8)
                                        }
                                    };
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
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme::text_primary())
                                                        .child("Desktop notifications"),
                                                )
                                                .child(
                                                    div()
                                                        .id("test-desktop-notif")
                                                        .px(px(6.0))
                                                        .py(px(1.0))
                                                        .rounded(theme::r_xs())
                                                        .when(action_kind < 3, |d| {
                                                            d.hover(|s| {
                                                                s.bg(theme::row_hover())
                                                                    .text_color(theme::text_primary())
                                                            })
                                                            .cursor_pointer()
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| match action_kind {
                                                                    0 => {
                                                                        this.send_desktop_notification(
                                                                            "test-notif",
                                                                            "Job Harness",
                                                                            "Notifications active",
                                                                            "Interviews, secret requests, and finished runs will alert you here.",
                                                                            None,
                                                                            true,
                                                                        );
                                                                        this.set_toast(
                                                                            "Sent native macOS notification",
                                                                            cx,
                                                                        );
                                                                    }
                                                                    1 => {
                                                                        crate::macos_notify::request_authorization();
                                                                        this.set_toast(
                                                                            "Requested macOS notification permission",
                                                                            cx,
                                                                        );
                                                                    }
                                                                    _ => {
                                                                        crate::macos_notify::open_system_settings();
                                                                        this.set_toast(
                                                                            "Opened macOS Notifications settings",
                                                                            cx,
                                                                        );
                                                                    }
                                                                },
                                                            ))
                                                        })
                                                        .text_size(px(10.5))
                                                        .text_color(action_col)
                                                        .child(action_lbl),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("toggle-desktop-notifications")
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.desktop_notifications =
                                                        !this.desktop_notifications;
                                                    let on = this.desktop_notifications;
                                                    if on
                                                        && crate::macos_notify::auth_status()
                                                            == crate::macos_notify::NotificationAuthStatus::NotDetermined
                                                    {
                                                        crate::macos_notify::request_authorization();
                                                    }
                                                    this.set_toast(
                                                        if on {
                                                            "macOS notifications enabled"
                                                        } else {
                                                            "macOS notifications muted"
                                                        },
                                                        cx,
                                                    );
                                                }))
                                                .child(switch_pill(self.desktop_notifications)),
                                        )
                                })
                                .child(
                                    div()
                                        .py_1()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_primary())
                                                .child("Reduce motion"),
                                        )
                                        .child(
                                            div()
                                                .id("toggle-reduce-motion")
                                                .cursor_pointer()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.reduce_motion = !this.reduce_motion;
                                                    cx.notify();
                                                }))
                                                .child(switch_pill(self.reduce_motion)),
                                        ),
                                )
                                .child(self.render_editable_kv(
                                    "prep_reminder_minutes",
                                    "Prep reminder",
                                    format!("{} min prior", p.brief_lead_time_minutes),
                                    p.brief_lead_time_minutes.to_string(),
                                    "Minutes (e.g. 90, 1440)",
                                    cx,
                                ))
                                .child(self.render_editable_kv(
                                    "applied_ghost_days",
                                    "Ghost archive",
                                    format!(
                                        "{} days",
                                        self.snapshot.profile_settings.applied_ghost_days
                                    ),
                                    self.snapshot.profile_settings.applied_ghost_days.to_string(),
                                    "Days (e.g. 30)",
                                    cx,
                                ))
                                .child(kv_row("Storage", self.snapshot.status_note.clone())),
                        )
                        // Section 4: Tools & connectors (with in-section sliding compartment for Career Ledger MCP setup)
                        .child({
                            let active_kind = self.runtime.kind();
                            let active_name = self.runtime.short_name().to_owned();
                            let local_root = self.client.target.local_root();
                            let mcp_cmd = match local_root {
                                Some(root) => format!("jh --root {} connect mcp", root.display()),
                                None => "jh connect mcp".to_owned(),
                            };
                            let setup_prompt = match local_root {
                                Some(root) => format!(
                                    "Connect the Job Harness Career Ledger MCP server for `{}` using `jh --root {} connect mcp`, then verify with `bootstrap`.",
                                    root.display(),
                                    root.display()
                                ),
                                None => "Connect the Job Harness Career Ledger MCP server using `jh connect mcp`, then verify with `bootstrap`.".to_owned(),
                            };
                            let web_search_access = match evaluate_capability(
                                &self.mcp_servers,
                                active_kind,
                                &[
                                    "search",
                                    "brave",
                                    "exa",
                                    "tavily",
                                    "perplexity",
                                    "firecrawl",
                                    "fetch",
                                ],
                            ) {
                                CapabilityAccess::Active { server, global } => {
                                    CapabilityAccess::Active { server, global }
                                }
                                _ if self.runtime.has_builtin_web_search() => {
                                    CapabilityAccess::BuiltIn
                                }
                                other => other,
                            };
                            let browser_access = match evaluate_capability(
                                &self.mcp_servers,
                                active_kind,
                                &[
                                    "browser",
                                    "playwright",
                                    "puppeteer",
                                    "chrome",
                                    "computer-use",
                                ],
                            ) {
                                CapabilityAccess::Active { server, global } => {
                                    CapabilityAccess::Active { server, global }
                                }
                                _ if self.runtime.has_builtin_browser() => {
                                    CapabilityAccess::BuiltIn
                                }
                                other => other,
                            };
                            let caps: [(&str, CapabilityAccess); 4] = [
                                (
                                    "Email",
                                    evaluate_capability(
                                        &self.mcp_servers,
                                        active_kind,
                                        &["mail", "gmail", "google", "outlook", "imap", "inbox"],
                                    ),
                                ),
                                (
                                    "Calendar",
                                    evaluate_capability(
                                        &self.mcp_servers,
                                        active_kind,
                                        &["calendar", "google", "cal", "gcal"],
                                    ),
                                ),
                                ("Web Search", web_search_access),
                                ("Browser", browser_access),
                            ];
                            let visible_servers: Vec<_> = self
                                .mcp_servers
                                .iter()
                                .filter(|s| s.visible_to(active_kind))
                                .collect();
                            let mcp_open = self.show_mcp_servers;
                            let in_subview = self.tools_subview;
                            let sub_epoch = self.tools_subview_epoch;
                            let reduce_motion = self.reduce_motion;

                            let section_body = if in_subview {
                                let runtime_rows: Vec<(String, bool, String, String)> = self
                                    .available_runtimes
                                    .iter()
                                    .map(|rt| {
                                        let kind = rt.kind();
                                        let connected = self.mcp_servers.iter().any(|s| {
                                            let low = s.name.to_ascii_lowercase();
                                            (low.contains("career")
                                                || low.contains("job-harness")
                                                || low.contains("job_harness"))
                                                && s.visible_to(kind)
                                        });
                                        (
                                            rt.short_name().to_owned(),
                                            connected,
                                            rt.mcp_connect_command(local_root),
                                            format!("connect-rt-{kind}"),
                                        )
                                    })
                                    .collect();
                                let copied_cmd = self.is_copied_flash("sub-copy-mcp-cmd");
                                let copied_prompt = self.is_copied_flash("sub-copy-agent-prompt");
                                div()
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
                                                    .id("tools-subview-back")
                                                    .px_1()
                                                    .py(px(2.0))
                                                    .mx(px(-4.0))
                                                    .rounded(theme::r_xs())
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .cursor_pointer()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(5.0))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.tools_subview = false;
                                                        this.tools_subview_epoch =
                                                            this.tools_subview_epoch.wrapping_add(1);
                                                        cx.notify();
                                                    }))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme::text_muted())
                                                            .child("‹"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .font_weight(FontWeight::MEDIUM)
                                                            .text_color(theme::text_primary())
                                                            .child("Connect Career Ledger"),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(theme::text_muted())
                                            .child("Detected runtimes on this Mac"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .children(runtime_rows.into_iter().map(
                                                |(name, connected, cmd, key_id)| {
                                                    let flash_key = key_id.clone();
                                                    let copied = self.is_copied_flash(&key_id);
                                                    let name_toast = name.clone();
                                                    div()
                                                        .id(SharedString::from(key_id))
                                                        .py(px(6.0))
                                                        .border_b_1()
                                                        .border_color(theme::hairline())
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .gap_2()
                                                        .when(!connected, |r| {
                                                            r.cursor_pointer()
                                                                .hover(|s| s.bg(theme::row_hover()))
                                                                .px_2()
                                                                .mx(px(-8.0))
                                                                .rounded(theme::r_sm())
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        cx.write_to_clipboard(
                                                                            ClipboardItem::new_string(
                                                                                cmd.clone(),
                                                                            ),
                                                                        );
                                                                        this.flash_copied(
                                                                            flash_key.clone(),
                                                                            cx,
                                                                        );
                                                                        this.set_toast(
                                                                            format!(
                                                                                "Copied MCP connect command for {name_toast}"
                                                                            ),
                                                                            cx,
                                                                        );
                                                                    },
                                                                ))
                                                        })
                                                        .child(
                                                            div()
                                                                .flex()
                                                                .items_center()
                                                                .gap_2()
                                                                .child(
                                                                    div()
                                                                        .w(px(6.0))
                                                                        .h(px(6.0))
                                                                        .rounded(px(99.0))
                                                                        .bg(if connected {
                                                                            theme::emerald()
                                                                        } else {
                                                                            theme::text_muted()
                                                                        }),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_xs()
                                                                        .text_color(
                                                                            theme::text_primary(),
                                                                        )
                                                                        .child(name),
                                                                ),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(11.0))
                                                                .text_color(if connected || copied {
                                                                    theme::emerald()
                                                                } else {
                                                                    theme::lavender()
                                                                })
                                                                .child(if connected {
                                                                    "Connected"
                                                                } else if copied {
                                                                    "Copied ✓"
                                                                } else {
                                                                    "Copy config"
                                                                }),
                                                        )
                                                },
                                            )),
                                    )
                                    .child(
                                        div()
                                            .pt_1()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.0))
                                            .child(
                                                div()
                                                    .id("sub-copy-mcp-cmd")
                                                    .px_2()
                                                    .py(px(5.0))
                                                    .mx(px(-8.0))
                                                    .rounded(theme::r_sm())
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .cursor_pointer()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(
                                                                mcp_cmd.clone(),
                                                            ),
                                                        );
                                                        this.flash_copied("sub-copy-mcp-cmd", cx);
                                                        this.set_toast(
                                                            "Copied `jh connect mcp` command",
                                                            cx,
                                                        );
                                                    }))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme::text_secondary())
                                                            .child("Copy MCP command"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(11.0))
                                                            .text_color(if copied_cmd {
                                                                theme::emerald()
                                                            } else {
                                                                theme::text_muted()
                                                            })
                                                            .child(if copied_cmd {
                                                                "Copied ✓"
                                                            } else {
                                                                "CLI"
                                                            }),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .id("sub-copy-agent-prompt")
                                                    .px_2()
                                                    .py(px(5.0))
                                                    .mx(px(-8.0))
                                                    .rounded(theme::r_sm())
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .cursor_pointer()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        cx.write_to_clipboard(
                                                            ClipboardItem::new_string(
                                                                setup_prompt.clone(),
                                                            ),
                                                        );
                                                        this.flash_copied(
                                                            "sub-copy-agent-prompt",
                                                            cx,
                                                        );
                                                        this.set_toast(
                                                            "Copied agent setup prompt",
                                                            cx,
                                                        );
                                                    }))
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(theme::text_secondary())
                                                            .child("Copy setup prompt for agent"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(11.0))
                                                            .text_color(if copied_prompt {
                                                                theme::emerald()
                                                            } else {
                                                                theme::text_muted()
                                                            })
                                                            .child(if copied_prompt {
                                                                "Copied ✓"
                                                            } else {
                                                                "Prompt"
                                                            }),
                                                    ),
                                            ),
                                    )
                                    .into_any_element()
                            } else {
                                div()
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
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme::text_primary())
                                                    .child("Tools & connectors"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(10.5))
                                                    .text_color(theme::text_muted())
                                                    .child(format!("for {active_name}")),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.0))
                                            // Career Ledger: slides this section into runtime & MCP connection view
                                            .child(
                                                div()
                                                    .id("open-career-ledger-subview")
                                                    .px_2()
                                                    .py(px(5.0))
                                                    .mx(px(-8.0))
                                                    .rounded(theme::r_sm())
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .cursor_pointer()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_2()
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.tools_subview = true;
                                                        this.tools_subview_epoch =
                                                            this.tools_subview_epoch.wrapping_add(1);
                                                        cx.notify();
                                                    }))
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .gap_2()
                                                            .child(
                                                                div()
                                                                    .w(px(6.0))
                                                                    .h(px(6.0))
                                                                    .rounded(px(99.0))
                                                                    .bg(theme::emerald()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(
                                                                        theme::text_primary(),
                                                                    )
                                                                    .child("Career Ledger"),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .items_center()
                                                            .gap(px(5.0))
                                                            .text_size(px(11.0))
                                                            .text_color(theme::text_muted())
                                                            .child("Connect")
                                                            .child("›"),
                                                    ),
                                            )
                                            .children(caps.into_iter().enumerate().map(
                                                |(ci, (label, access))| {
                                                    let (
                                                        dot_col,
                                                        status_txt,
                                                        status_col,
                                                        action_kind,
                                                    ) = match access {
                                                        CapabilityAccess::Active {
                                                            server, ..
                                                        } => (
                                                            theme::emerald(),
                                                            truncate_end(&server, 18),
                                                            theme::text_muted(),
                                                            0u8,
                                                        ),
                                                        CapabilityAccess::BuiltIn => (
                                                            theme::emerald(),
                                                            "Built-in".to_owned(),
                                                            theme::text_muted(),
                                                            0u8,
                                                        ),
                                                        CapabilityAccess::OtherRuntime {
                                                            owner,
                                                            ..
                                                        } => (
                                                            theme::amber_accent(),
                                                            format!("Switch to {owner} →"),
                                                            theme::amber_text(),
                                                            1u8,
                                                        ),
                                                        CapabilityAccess::NotConnected => (
                                                            theme::text_muted(),
                                                            "Connect →".to_owned(),
                                                            theme::lavender(),
                                                            2u8,
                                                        ),
                                                    };
                                                    let cap_label = label.to_owned();
                                                    let owner_target = status_txt.clone();
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "cap-row-{ci}"
                                                        )))
                                                        .px_2()
                                                        .py(px(5.0))
                                                        .mx(px(-8.0))
                                                        .rounded(theme::r_sm())
                                                        .when(action_kind > 0, |d| {
                                                            d.hover(|s| s.bg(theme::row_hover()))
                                                                .cursor_pointer()
                                                                .on_click(cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        if action_kind == 1 {
                                                                            let low = owner_target
                                                                                .to_ascii_lowercase();
                                                                            if let Some(rt) = this
                                                                                .available_runtimes
                                                                                .iter()
                                                                                .find(|r| {
                                                                                    low.contains(
                                                                                        &r.short_name()
                                                                                            .to_ascii_lowercase(),
                                                                                    )
                                                                                })
                                                                                .cloned()
                                                                            {
                                                                                let nm = rt
                                                                                    .short_name()
                                                                                    .to_owned();
                                                                                this.runtime = rt;
                                                                                this.set_toast(
                                                                                    format!(
                                                                                        "Switched to {nm}"
                                                                                    ),
                                                                                    cx,
                                                                                );
                                                                            }
                                                                        } else {
                                                                            this.submit_prompt(
                                                                                format!(
                                                                                    "Help me connect {cap_label} tools for {} in this Job Harness workspace.",
                                                                                    this.runtime.short_name()
                                                                                ),
                                                                                false,
                                                                                cx,
                                                                            );
                                                                        }
                                                                    },
                                                                ))
                                                        })
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .flex()
                                                                .items_center()
                                                                .gap_2()
                                                                .child(
                                                                    div()
                                                                        .w(px(6.0))
                                                                        .h(px(6.0))
                                                                        .rounded(px(99.0))
                                                                        .bg(dot_col),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_xs()
                                                                        .text_color(
                                                                            theme::text_primary(),
                                                                        )
                                                                        .child(label),
                                                                ),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(11.0))
                                                                .text_color(status_col)
                                                                .child(status_txt),
                                                        )
                                                },
                                            )),
                                    )
                                    .when(!visible_servers.is_empty(), |d| {
                                        let srv_count = visible_servers.len();
                                        d.child(
                                            div()
                                                .id("toggle-mcp-servers")
                                                .pt_1()
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .text_size(px(11.0))
                                                .text_color(theme::text_muted())
                                                .hover(|s| s.text_color(theme::text_primary()))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.show_mcp_servers = !this.show_mcp_servers;
                                                    cx.notify();
                                                }))
                                                .child(format!("Other MCP servers ({srv_count})"))
                                                .child(if mcp_open { "▾" } else { "▸" }),
                                        )
                                        .when(mcp_open, |box_d| {
                                            box_d.child(
                                                div()
                                                    .pt_1()
                                                    .flex()
                                                    .flex_col()
                                                    .children(
                                                        visible_servers
                                                            .into_iter()
                                                            .take(16)
                                                            .map(|srv| {
                                                                div()
                                                                    .py(px(4.5))
                                                                    .border_t_1()
                                                                    .border_color(theme::hairline())
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_between()
                                                                    .gap_2()
                                                                    .child(
                                                                        div()
                                                                            .flex_1()
                                                                            .min_w_0()
                                                                            .text_size(px(11.0))
                                                                            .text_color(
                                                                                theme::text_secondary(),
                                                                            )
                                                                            .child(truncate_end(
                                                                                &srv.name, 26,
                                                                            )),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .flex_shrink_0()
                                                                            .text_size(px(10.5))
                                                                            .text_color(
                                                                                theme::text_muted(),
                                                                            )
                                                                            .child(
                                                                                srv.source.clone(),
                                                                            ),
                                                                    )
                                                            }),
                                                    ),
                                            )
                                        })
                                    })
                                    .into_any_element()
                            };

                            div()
                                .pt_3()
                                .border_t_1()
                                .border_color(theme::hairline())
                                .overflow_hidden()
                                .child(section_body)
                                .with_animation(
                                    SharedString::from(format!("tools-compartment-{sub_epoch}")),
                                    Animation::new(Duration::from_millis(if reduce_motion {
                                        1
                                    } else {
                                        150
                                    }))
                                    .with_easing(ease_out_quint()),
                                    move |el, delta| {
                                        let shift = if in_subview {
                                            (1.0 - delta) * 14.0
                                        } else {
                                            -(1.0 - delta) * 14.0
                                        };
                                        el.ml(px(shift)).opacity(0.45 + 0.55 * delta)
                                    },
                                )
                        })
                        // Section 5: Workspace switching, Setup Wizard & Spotlight UI Tour
                        .child(
                            div()
                                .pt_3()
                                .border_t_1()
                                .border_color(theme::hairline())
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("set-rerun-wizard")
                                                .flex_1()
                                                .px_2()
                                                .py(px(6.0))
                                                .rounded(theme::r_sm())
                                                .bg(theme::surface_subtle())
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_xs()
                                                .text_color(theme::text_primary())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.setup_step = 0;
                                                    this.active_popover = ActivePopover::SetupWizard;
                                                    this.cmd_anim_epoch =
                                                        this.cmd_anim_epoch.wrapping_add(1);
                                                    cx.notify();
                                                }))
                                                .child("Setup wizard"),
                                        )
                                        .child(
                                            div()
                                                .id("set-start-tour")
                                                .flex_1()
                                                .px_2()
                                                .py(px(6.0))
                                                .rounded(theme::r_sm())
                                                .bg(theme::surface_subtle())
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_xs()
                                                .text_color(theme::text_secondary())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.right_drawer = RightDrawer::Closed;
                                                    this.tour_step = 0;
                                                    this.tour_prev_step = 0;
                                                    this.tour_anim_epoch =
                                                        this.tour_anim_epoch.wrapping_add(1);
                                                    this.active_popover = ActivePopover::Tour;
                                                    cx.notify();
                                                }))
                                                .child("Quick UI tour"),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("set-switch-ws")
                                                .flex_1()
                                                .px_2()
                                                .py(px(6.0))
                                                .rounded(theme::r_sm())
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.open_workspace_dialog(cx);
                                                }))
                                                .child("Open folder…"),
                                        )
                                        .child(
                                            div()
                                                .id("set-try-sample")
                                                .flex_1()
                                                .px_2()
                                                .py(px(6.0))
                                                .rounded(theme::r_sm())
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.launch_sample_workspace(cx);
                                                }))
                                                .child("Try sample data"),
                                        ),
                                ),
                        )
                        .into_any_element(),
                )
            }
            RightDrawer::Tidy => {
                let quiet_roles: Vec<RoleCard> =
                    self.tidy_candidates().into_iter().cloned().collect();
                Some(
                    div()
                        .id("tidy-drawer")
                        .w_full()
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
                        .child(drawer_header(
                            "Tidy",
                            Some("Manual review only — nothing archives automatically.".into()),
                            cx,
                        ))
                        .when(self.snapshot.unresolved_attempts > 0, |d| {
                            let n = self.snapshot.unresolved_attempts;
                            d.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .rounded(theme::r_sm())
                                    .bg(theme::amber_wash())
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
                            let today = chrono::Utc::now().date_naive();
                            let short_stage = match card.stage.as_str() {
                                "introduction" => "Intro",
                                "screening" => "Screening",
                                "technical" => "Technical",
                                "final" => "Final",
                                "offer" => "Offer",
                                "applied" => "Applied",
                                "saved" => "Saved",
                                "prepared" => "Prepared",
                                _ => "Active",
                            };
                            let (days, age_label) = if let Some(d) = card.quiet_days {
                                (d, format!("{short_stage} {d}d"))
                            } else if let Ok(checked) =
                                chrono::NaiveDate::parse_from_str(&card.checked, "%Y-%m-%d")
                            {
                                let d = (today - checked).num_days().max(0) as u64;
                                (d, format!("{short_stage} {d}d"))
                            } else {
                                (0, short_stage.to_owned())
                            };
                            let card_archive = card.clone();
                            let company_follow = card.company.clone();
                            let path_keep = card.record_path.clone();
                            let card_open = card.clone();
                            let card_inspect = card.clone();
                            let is_saved_lead =
                                matches!(card.stage.as_str(), "saved" | "prepared");

                            div()
                                .py_3()
                                .when(i > 0, |d| d.border_t_1().border_color(theme::hairline()))
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
                                                .child(age_label),
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
                                        .child(if is_saved_lead {
                                            div()
                                                .id(SharedString::from(format!("tidy-ins-{i}")))
                                                .text_xs()
                                                .text_color(theme::lavender())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.open_role_drawer(
                                                        card_inspect.clone(),
                                                        DrawerTab::Overview,
                                                        cx,
                                                    );
                                                }))
                                                .child("Inspect")
                                                .into_any_element()
                                        } else {
                                            div()
                                                .id(SharedString::from(format!("tidy-fup-{i}")))
                                                .text_xs()
                                                .text_color(theme::lavender())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.submit_prompt(
                                                        format!(
                                                            "Draft a short follow-up email for {company_follow} (check Gmail Sent first; do not send)."
                                                        ),
                                                        false,
                                                        cx,
                                                    );
                                                }))
                                                .child("Draft follow-up")
                                                .into_any_element()
                                        })
                                        .child({
                                            let stage_keep = card.stage.clone();
                                            let next_keep = card.next_action.clone();
                                            let has_sched_keep = card.scheduled_start.is_some();
                                            div()
                                                .id(SharedString::from(format!("tidy-keep-{i}")))
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .hover(|s| s.text_color(theme::text_primary()))
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.dismissed_tidy.insert(path_keep.clone());
                                                    let client = this.client.clone();
                                                    let rp = path_keep.clone();
                                                    let st = stage_keep.clone();
                                                    let na = next_keep.clone();
                                                    cx.spawn(async move |this, cx| {
                                                        let res = cx
                                                            .background_executor()
                                                            .spawn(async move {
                                                                client.move_role_stage(
                                                                    &rp,
                                                                    &st,
                                                                    &st,
                                                                    &na,
                                                                    has_sched_keep,
                                                                )
                                                            })
                                                            .await;
                                                        let _ = this.update(cx, |app, cx| {
                                                            if res.is_ok() {
                                                                app.refresh_snapshot_async(
                                                                    false, cx,
                                                                );
                                                            }
                                                        });
                                                    })
                                                    .detach();
                                                    this.set_toast(
                                                        "Kept active — check date refreshed",
                                                        cx,
                                                    );
                                                }))
                                                .child("Keep active")
                                        }),
                                )
                        }))
                        .into_any_element(),
                )
            }
        }
    }

    fn bottom_popover_right_offset(&self) -> f32 {
        match self.right_drawer {
            RightDrawer::Closed => 16.0,
            _ => self.drawer_width + 20.0,
        }
    }

    fn render_popover(
        &self,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match self.active_popover {
            ActivePopover::None => None,
            ActivePopover::CommandBar => {
                let entries = self.command_entries();
                let selected = self.command_index.min(entries.len().saturating_sub(1));
                let epoch = self.cmd_anim_epoch;
                Some(
                    div()
                        .id("cmd-backdrop")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .bg(gpui::rgba(0x040408cc))
                        .flex()
                        .justify_center()
                        .pt(px(76.0))
                        .on_scroll_wheel(|_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.active_popover = ActivePopover::None;
                                this.active_input = ActiveInput::None;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .id("cmd-bar")
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .w(px(680.0))
                                .max_h(px(540.0))
                                .rounded(theme::r_lg())
                                .bg(theme::popover_glass())
                                .border_1()
                                .border_color(theme::hairline_strong())
                                .shadow_lg()
                                .flex()
                                .flex_col()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .px_5()
                                        .py(px(16.0))
                                        .border_b_1()
                                        .border_color(theme::hairline())
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .text_size(px(15.0))
                                        .child(
                                            svg()
                                                .path("icons/search.svg")
                                                .size(px(17.0))
                                                .flex_shrink_0()
                                                .text_color(theme::text_muted()),
                                        )
                                        .child(input_line(
                                            &self.command_query,
                                            "Search roles, actions, or ask Chat…",
                                            self.active_input == ActiveInput::CommandBar,
                                            self.reduce_motion,
                                            "cmd",
                                            64,
                                        )),
                                )
                                .child(
                                    div()
                                        .id("cmd-list")
                                        .flex_1()
                                        .min_h_0()
                                        .px_3()
                                        .py_3()
                                        .overflow_y_scroll()
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.0))
                                        .children({
                                            let mut items: Vec<AnyElement> = Vec::new();
                                            let mut last_section: Option<&'static str> = None;
                                            for (i, entry) in entries.into_iter().enumerate() {
                                                let section = match &entry.action {
                                                    CommandAction::OpenRole(_) => "Roles",
                                                    _ => "Actions",
                                                };
                                                if last_section != Some(section) {
                                                    items.push(
                                                        div()
                                                            .flex_shrink_0()
                                                            .px_3()
                                                            .pt(if last_section.is_some() {
                                                                px(14.0)
                                                            } else {
                                                                px(4.0)
                                                            })
                                                            .pb(px(6.0))
                                                            .text_size(px(12.0))
                                                            .text_color(theme::text_muted())
                                                            .child(section)
                                                            .into_any_element(),
                                                    );
                                                    last_section = Some(section);
                                                }
                                                let active = i == selected;
                                                let row_icon = match &entry.action {
                                                    CommandAction::OpenRole(_) => {
                                                        "icons/briefcase.svg"
                                                    }
                                                    CommandAction::OpenCvs => "icons/file-text.svg",
                                                    CommandAction::OpenProfile => "icons/user.svg",
                                                    CommandAction::OpenSettings => {
                                                        "icons/settings.svg"
                                                    }
                                                    CommandAction::OpenTidy => "icons/tidy.svg",
                                                    CommandAction::OpenBudget => "icons/gauge.svg",
                                                    CommandAction::ToggleChat => {
                                                        "icons/panel-left.svg"
                                                    }
                                                    CommandAction::ToggleAttention => {
                                                        "icons/filter.svg"
                                                    }
                                                    CommandAction::ToggleArchive => {
                                                        "icons/archive.svg"
                                                    }
                                                    CommandAction::RefreshWorkspace => {
                                                        "icons/refresh-cw.svg"
                                                    }
                                                    CommandAction::ImportMasterCv => {
                                                        "icons/upload.svg"
                                                    }
                                                    CommandAction::PassSecret => "icons/lock.svg",
                                                    CommandAction::CopyTranscript => {
                                                        "icons/copy.svg"
                                                    }
                                                    CommandAction::ExportTranscript => {
                                                        "icons/file-text.svg"
                                                    }
                                                    CommandAction::OpenSetupWizard => {
                                                        "icons/settings.svg"
                                                    }
                                                    CommandAction::StartTour => "icons/eye.svg",
                                                    CommandAction::AskAgent(_) => {
                                                        "icons/terminal.svg"
                                                    }
                                                };
                                                let action = entry.action;
                                                items.push(
                                                    div()
                                                        .id(SharedString::from(format!(
                                                            "cmd-row-{i}"
                                                        )))
                                                        .flex_shrink_0()
                                                        .px_3()
                                                        .py(px(11.0))
                                                        .rounded(theme::r_md())
                                                        .overflow_hidden()
                                                        .when(active, |d| {
                                                            d.bg(theme::surface_subtle())
                                                        })
                                                        .hover(|s| s.bg(theme::row_hover()))
                                                        .flex()
                                                        .items_center()
                                                        .justify_between()
                                                        .gap_3()
                                                        .cursor_pointer()
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.execute_command(
                                                                    action.clone(),
                                                                    cx,
                                                                );
                                                            },
                                                        ))
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .flex()
                                                                .items_center()
                                                                .gap(px(12.0))
                                                                .child(
                                                                    svg()
                                                                        .path(row_icon)
                                                                        .size(px(16.0))
                                                                        .flex_shrink_0()
                                                                        .text_color(if active {
                                                                            theme::text_primary()
                                                                        } else {
                                                                            theme::text_muted()
                                                                        }),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .flex_1()
                                                                        .min_w_0()
                                                                        .overflow_hidden()
                                                                        .text_size(px(14.0))
                                                                        .text_color(if active {
                                                                            theme::text_primary()
                                                                        } else {
                                                                            theme::text_secondary()
                                                                        })
                                                                        .child(truncate_end(
                                                                            &entry.label,
                                                                            60,
                                                                        )),
                                                                ),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex_shrink_0()
                                                                .text_size(px(12.0))
                                                                .text_color(theme::text_muted())
                                                                .child(entry.meta),
                                                        )
                                                        .into_any_element(),
                                                );
                                            }
                                            items
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .px_5()
                                        .py(px(13.0))
                                        .border_t_1()
                                        .border_color(theme::hairline())
                                        .bg(theme::window_glass())
                                        .flex()
                                        .items_center()
                                        .gap_4()
                                        .text_size(px(12.0))
                                        .text_color(theme::text_muted())
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(4.0))
                                                .child(cmd_keycap("↑"))
                                                .child(cmd_keycap("↓"))
                                                .child(div().ml(px(2.0)).child("Navigate")),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(5.0))
                                                .child(cmd_keycap("Enter"))
                                                .child("Select"),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(5.0))
                                                .child(cmd_keycap("Esc"))
                                                .child("Close"),
                                        ),
                                )
                                .with_animation(
                                    SharedString::from(format!("cmd-anim-{epoch}")),
                                    Animation::new(Duration::from_millis(if self.reduce_motion {
                                        1
                                    } else {
                                        140
                                    }))
                                    .with_easing(ease_out_quint()),
                                    |el, t| el.opacity(t).mt(px(-8.0 * (1.0 - t))),
                                ),
                        )
                        .into_any_element(),
                )
            }
            ActivePopover::Agent => {
                let epoch = self.cmd_anim_epoch;
                let pop_w = if self.chat_fullscreen {
                    340.0
                } else {
                    (self.chat_width - 24.0).clamp(276.0, 348.0)
                };
                let card = div()
                    .id("agent-popover")
                    .absolute()
                    .left(px(12.0))
                    .top(px(96.0))
                    .w(px(pop_w))
                    .p_4()
                    .rounded(theme::r_lg())
                    .bg(theme::popover_glass())
                    .border_1()
                    .border_color(theme::hairline_strong())
                    .shadow_lg()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.render_agent_controls("pop", true, cx));
                Some(
                    div()
                        .id("agent-backdrop")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.active_popover = ActivePopover::None;
                                if this.active_input == ActiveInput::ModelField {
                                    this.active_input = ActiveInput::None;
                                }
                                cx.notify();
                            }),
                        )
                        .child(if self.reduce_motion {
                            card.into_any_element()
                        } else {
                            card.with_animation(
                                SharedString::from(format!("pop-agent-{epoch}")),
                                Animation::new(Duration::from_millis(140))
                                    .with_easing(ease_out_quint()),
                                |el, t| el.opacity(t).mt(px(-6.0 * (1.0 - t))),
                            )
                            .into_any_element()
                        })
                        .into_any_element(),
                )
            }
            ActivePopover::Sessions => {
                let epoch = self.cmd_anim_epoch;
                let rt_name = self.runtime.short_name().to_owned();
                let is_fresh = !self.agent_opts.resume_existing;
                let active_id = self.agent_opts.session_name.clone();
                let sessions = self.host_sessions.clone();
                let pop_w = if self.chat_fullscreen {
                    340.0
                } else {
                    (self.chat_width - 24.0).clamp(276.0, 348.0)
                };
                let card = div()
                    .id("sessions-popover")
                    .absolute()
                    .when(self.chat_fullscreen, |d| d.right(px(14.0)))
                    .when(!self.chat_fullscreen, |d| {
                        d.left(px((self.chat_width - pop_w - 12.0).max(12.0)))
                    })
                    .top(px(96.0))
                    .w(px(pop_w))
                    .p_4()
                    .rounded(theme::r_lg())
                    .bg(theme::popover_glass())
                    .border_1()
                    .border_color(theme::hairline_strong())
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text_primary())
                                    .child(format!("Recent {rt_name} sessions")),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(theme::text_muted())
                                    .child("Read-only"),
                            ),
                    )
                    .child(
                        div()
                            .id("session-start-fresh")
                            .px_2()
                            .py(px(6.0))
                            .rounded(theme::r_sm())
                            .bg(if is_fresh {
                                theme::surface_active()
                            } else {
                                theme::surface_subtle()
                            })
                            .hover(|s| s.bg(theme::row_hover()))
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .justify_between()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.chat_messages.clear();
                                this.agent_opts.session_name =
                                    Some(format!("career-gui-{}", chrono::Utc::now().timestamp()));
                                this.agent_opts.resume_existing = false;
                                this.resumed_session_label = None;
                                this.active_popover = ActivePopover::None;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_primary())
                                    .child("Start fresh chat (default)"),
                            )
                            .when(is_fresh, |d| {
                                d.child(
                                    div()
                                        .w(px(6.0))
                                        .h(px(6.0))
                                        .rounded(px(99.0))
                                        .bg(theme::emerald()),
                                )
                            }),
                    )
                    .child(div().h(px(1.0)).w_full().bg(theme::hairline()))
                    .child(if sessions.is_empty() {
                        div()
                            .py_3()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(format!("No recent {rt_name} sessions found on disk."))
                            .into_any_element()
                    } else {
                        div()
                            .id("sessions-list-scroll")
                            .max_h(px(280.0))
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .children(sessions.into_iter().enumerate().map(|(si, s)| {
                                let is_cur =
                                    !is_fresh && active_id.as_deref() == Some(s.id.as_str());
                                let sid = s.id.clone();
                                let stitle = s.title.clone();
                                let rt_toast = rt_name.clone();
                                let meta = if s.message_count > 0 {
                                    format!("{}   {} msgs", s.age_label, s.message_count)
                                } else {
                                    s.age_label.clone()
                                };
                                div()
                                    .id(SharedString::from(format!("host-sess-{si}")))
                                    .flex_shrink_0()
                                    .px_2()
                                    .py(px(6.0))
                                    .rounded(theme::r_sm())
                                    .when(is_cur, |d| d.bg(theme::surface_active()))
                                    .hover(|d| d.bg(theme::row_hover()))
                                    .cursor_pointer()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.0))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.chat_messages.clear();
                                        this.agent_opts.session_name = Some(sid.clone());
                                        this.agent_opts.resume_existing = true;
                                        this.resumed_session_label = Some(stitle.clone());
                                        this.active_popover = ActivePopover::None;
                                        this.set_toast(format!("Resuming {rt_toast} session"), cx);
                                    }))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_xs()
                                            .text_color(theme::text_primary())
                                            .child(truncate_end(&s.title, 40)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.0))
                                            .text_color(theme::text_muted())
                                            .child(meta),
                                    )
                            }))
                            .into_any_element()
                    });
                Some(
                    div()
                        .id("sessions-backdrop")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.active_popover = ActivePopover::None;
                                cx.notify();
                            }),
                        )
                        .child(if self.reduce_motion {
                            card.into_any_element()
                        } else {
                            card.with_animation(
                                SharedString::from(format!("pop-sess-{epoch}")),
                                Animation::new(Duration::from_millis(140))
                                    .with_easing(ease_out_quint()),
                                |el, t| el.opacity(t).mt(px(-6.0 * (1.0 - t))),
                            )
                            .into_any_element()
                        })
                        .into_any_element(),
                )
            }
            ActivePopover::Secret => {
                let req = self.active_secret_req.clone()?;
                let epoch = self.cmd_anim_epoch;
                let active_idx = self.secret_field_idx;
                let drafts = self.secret_drafts.clone();
                let rem = req.remaining_secs(secrets::now_unix());
                let ttl_mins = (rem / 60).max(1);
                let url_opt = req.url.clone();
                let card = div()
                    .id("secret-modal-card")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .w(px(480.0))
                    .rounded(theme::r_lg())
                    .bg(theme::popover_glass())
                    .border_1()
                    .border_color(theme::hairline_strong())
                    .shadow_lg()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_5()
                            .py_4()
                            .border_b_1()
                            .border_color(theme::hairline())
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                svg()
                                                    .path("icons/lock.svg")
                                                    .size(px(15.0))
                                                    .text_color(theme::amber_text()),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme::text_primary())
                                                    .child(req.label.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .px(px(6.0))
                                            .py(px(2.0))
                                            .rounded(theme::r_xs())
                                            .bg(theme::amber_wash())
                                            .text_size(px(10.5))
                                            .text_color(theme::amber_text())
                                            .child(format!("1-use  {ttl_mins}m TTL")),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_secondary())
                                    .child(req.reason.clone()),
                            )
                            .when_some(url_opt, |d, u| {
                                let u_open = u.clone();
                                d.child(
                                    div()
                                        .id("secret-open-url")
                                        .text_size(px(11.0))
                                        .text_color(theme::text_muted())
                                        .hover(|s| s.text_color(theme::text_primary()))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |_, _, _, _| {
                                            open_url(&u_open);
                                        }))
                                        .child(truncate_end(&u, 58)),
                                )
                            }),
                    )
                    .child(div().px_5().py_4().flex().flex_col().gap_3().children(
                        req.fields.iter().enumerate().map(|(fi, field)| {
                            let focused =
                                self.active_input == ActiveInput::SecretField && active_idx == fi;
                            let raw_val = drafts.get(fi).cloned().unwrap_or_default();
                            let display_val = if field.secret {
                                "•".repeat(raw_val.chars().count())
                            } else {
                                raw_val
                            };
                            let ph = field
                                .placeholder
                                .clone()
                                .unwrap_or_else(|| format!("Enter {}…", field.label));
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(5.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(field.label.clone()),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("sec-field-{fi}")))
                                        .px_3()
                                        .py(px(8.0))
                                        .rounded(theme::r_md())
                                        .bg(if focused {
                                            theme::surface_active()
                                        } else {
                                            theme::surface_subtle()
                                        })
                                        .border_1()
                                        .border_color(if focused {
                                            theme::hairline_strong()
                                        } else {
                                            gpui::rgba(0x00000000)
                                        })
                                        .hover(|s| {
                                            if focused {
                                                s
                                            } else {
                                                s.bg(gpui::rgba(0xffffff0e))
                                            }
                                        })
                                        .cursor_text()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.secret_field_idx = fi;
                                            this.active_input = ActiveInput::SecretField;
                                            cx.notify();
                                        }))
                                        .child(input_line(
                                            &display_val,
                                            &ph,
                                            focused,
                                            self.reduce_motion,
                                            "sec",
                                            52,
                                        )),
                                )
                        }),
                    ))
                    .child(
                        div()
                            .px_5()
                            .py_3()
                            .border_t_1()
                            .border_color(theme::hairline())
                            .bg(theme::window_glass())
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(theme::text_muted())
                                    .child("0600 temp file — auto-shreds after read"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("secret-cancel-btn")
                                            .px_3()
                                            .py(px(5.0))
                                            .rounded(theme::r_sm())
                                            .hover(|s| s.bg(theme::row_hover()))
                                            .cursor_pointer()
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.close_secret_modal(true, cx);
                                            }))
                                            .child("Cancel"),
                                    )
                                    .child(
                                        div()
                                            .id("secret-submit-btn")
                                            .px_3()
                                            .py(px(5.0))
                                            .rounded(theme::r_sm())
                                            .bg(theme::white_solid())
                                            .cursor_pointer()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme::ink_dark())
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.submit_secret_modal(cx);
                                            }))
                                            .child("Arm secret"),
                                    ),
                            ),
                    );
                Some(
                    div()
                        .id("secret-backdrop")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .bg(gpui::rgba(0x040408cc))
                        .flex()
                        .justify_center()
                        .pt(px(96.0))
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.close_secret_modal(true, cx);
                            }),
                        )
                        .child(if self.reduce_motion {
                            card.into_any_element()
                        } else {
                            card.with_animation(
                                SharedString::from(format!("pop-secret-{epoch}")),
                                Animation::new(Duration::from_millis(140))
                                    .with_easing(ease_out_quint()),
                                |el, t| el.opacity(t).mt(px(-8.0 * (1.0 - t))),
                            )
                            .into_any_element()
                        })
                        .into_any_element(),
                )
            }
            ActivePopover::SetupWizard => {
                let epoch = self.cmd_anim_epoch;
                let step = self.setup_step.min(3);
                let step_titles = [
                    "1. Agent & tools",
                    "2. Master CV",
                    "3. Defaults & guardrails",
                    "4. Delegation mode",
                ];
                let ps = &self.snapshot.profile_settings;
                let cur = if ps.currency.is_empty() {
                    "GBP"
                } else {
                    ps.currency.as_str()
                };
                let p = &self.snapshot.permissions;
                let active_preset = if !p.auto_apply_portal
                    && !p.auto_send_first_contact_email
                    && !p.auto_reply_threads
                    && !p.auto_archive_mail
                {
                    "drafts"
                } else if p.auto_apply_portal
                    && !p.auto_send_first_contact_email
                    && !p.auto_reply_threads
                    && p.auto_archive_mail
                {
                    "assisted"
                } else if p.auto_apply_portal
                    && p.auto_send_first_contact_email
                    && p.auto_reply_threads
                    && p.auto_archive_mail
                {
                    "autonomous"
                } else {
                    "custom"
                };

                let step_body: AnyElement = match step {
                    0 => {
                        let auth = crate::macos_notify::auth_status();
                        let (perm_txt, perm_col, perm_kind) = match auth {
                            crate::macos_notify::NotificationAuthStatus::Authorized => {
                                ("Allowed", theme::emerald(), 0u8)
                            }
                            crate::macos_notify::NotificationAuthStatus::NotDetermined => {
                                ("Enable", theme::lavender(), 1u8)
                            }
                            crate::macos_notify::NotificationAuthStatus::Denied => {
                                ("System Settings", theme::amber_text(), 2u8)
                            }
                            crate::macos_notify::NotificationAuthStatus::Unbundled => {
                                ("Ready in .app", theme::text_muted(), 0u8)
                            }
                        };
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_secondary())
                                    .child("Pick your local agent runtime. Job Harness keeps all career files locally and never locks you into one model."),
                            )
                            .child(self.render_agent_controls("wiz", false, cx))
                            .child(
                                div()
                                    .pt_2()
                                    .border_t_1()
                                    .border_color(theme::hairline())
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.0))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme::text_primary())
                                                    .child("macOS notifications"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.0))
                                                    .text_color(theme::text_muted())
                                                    .child("Optional alerts for interviews & secrets. No Accessibility or Full Disk Access needed."),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("wiz-notif-perm")
                                            .flex_shrink_0()
                                            .px_2()
                                            .py(px(4.0))
                                            .rounded(theme::r_sm())
                                            .when(perm_kind > 0, |d| {
                                                d.bg(theme::surface_subtle())
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .cursor_pointer()
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            if perm_kind == 1 {
                                                                crate::macos_notify::request_authorization();
                                                                this.set_toast(
                                                                    "Requested macOS notification permission",
                                                                    cx,
                                                                );
                                                            } else {
                                                                crate::macos_notify::open_system_settings();
                                                            }
                                                        },
                                                    ))
                                            })
                                            .text_xs()
                                            .text_color(perm_col)
                                            .child(perm_txt),
                                    ),
                            )
                            .into_any_element()
                    }
                    1 => {
                        let cv_count = self.snapshot.cv_nodes.len();
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_secondary())
                                    .child("Your Master CV is the single source of truth for facts; track CVs only change emphasis."),
                            )
                            .child(
                                div()
                                    .p_3()
                                    .rounded(theme::r_md())
                                    .bg(theme::window_glass())
                                    .border_1()
                                    .border_color(theme::hairline())
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.0))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme::text_primary())
                                                    .child(if cv_count > 0 {
                                                        format!("{cv_count} CV document(s) detected")
                                                    } else {
                                                        "No Master CV imported yet".to_owned()
                                                    }),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.0))
                                                    .text_color(theme::text_muted())
                                                    .child("Import a PDF anytime to populate or refresh your baseline."),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("wiz-import-cv")
                                            .px_3()
                                            .py(px(6.0))
                                            .rounded(theme::r_sm())
                                            .bg(theme::surface_active())
                                            .hover(|s| s.bg(theme::row_hover()))
                                            .cursor_pointer()
                                            .text_xs()
                                            .text_color(theme::text_primary())
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.import_cv_dialog(cx);
                                            }))
                                            .child("Import PDF…"),
                                    ),
                            )
                            .into_any_element()
                    }
                    2 => div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .pb_1()
                                .text_xs()
                                .text_color(theme::text_secondary())
                                .child("Click any value to edit — saved straight to config/career-ledger.json and shared by every connected agent."),
                        )
                        .child(self.render_editable_kv(
                            "application_email",
                            "Application email",
                            if ps.application_email.is_empty() {
                                "Set form email".into()
                            } else {
                                truncate_label(&ps.application_email, 28)
                            },
                            ps.application_email.clone(),
                            "cv@domain.com",
                            cx,
                        ))
                        .child(self.render_editable_kv(
                            "sender_email",
                            "Reply email",
                            if ps.sender_email.is_empty() {
                                "Set reply email".into()
                            } else {
                                truncate_label(&ps.sender_email, 28)
                            },
                            ps.sender_email.clone(),
                            "you@domain.com",
                            cx,
                        ))
                        .child(self.render_editable_kv(
                            "min_base_salary",
                            "Min salary",
                            format!("{cur} {}", ps.min_base_salary),
                            ps.min_base_salary.to_string(),
                            "80000",
                            cx,
                        ))
                        .child(self.render_editable_kv(
                            "target_base_salary",
                            "Target salary",
                            format!("{cur} {}", ps.target_base_salary),
                            ps.target_base_salary.to_string(),
                            "100000",
                            cx,
                        ))
                        .child(self.render_editable_kv(
                            "location_preferences",
                            "Location",
                            truncate_end(
                                if ps.location_preferences.is_empty() {
                                    "Set preference"
                                } else {
                                    &ps.location_preferences
                                },
                                30,
                            ),
                            ps.location_preferences.clone(),
                            "UK remote, London hybrid",
                            cx,
                        ))
                        .child(self.render_editable_kv(
                            "notice_period",
                            "Notice period",
                            truncate_end(
                                if ps.notice_period.is_empty() {
                                    "Set notice period"
                                } else {
                                    &ps.notice_period
                                },
                                30,
                            ),
                            ps.notice_period.clone(),
                            "1 month / Immediate",
                            cx,
                        ))
                        .child(self.render_editable_kv(
                            "avoid_list",
                            "Avoid list",
                            truncate_end(
                                if ps.avoid_list.is_empty() {
                                    "Companies or sectors to skip"
                                } else {
                                    &ps.avoid_list
                                },
                                30,
                            ),
                            ps.avoid_list.clone(),
                            "gambling, defence, agencies",
                            cx,
                        ))
                        .into_any_element(),
                    _ => {
                        let modes: [(&'static str, &'static str, &'static str, bool); 3] = [
                            (
                                "drafts",
                                "Drafts only",
                                "Preps packets and saves Gmail drafts only — you submit and send everything.",
                                false,
                            ),
                            (
                                "assisted",
                                "Assisted",
                                "Auto-submits clean ATS portals within your daily cap and archives verified receipts; cold emails & replies stay drafts.",
                                true,
                            ),
                            (
                                "autonomous",
                                "Autonomous",
                                "Submits portals, sends verified cold emails & routine replies, and archives receipts.",
                                false,
                            ),
                        ];
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(modes.into_iter().map(|(id, title, desc, rec)| {
                                let on = active_preset == id;
                                div()
                                    .id(SharedString::from(format!("wiz-mode-{id}")))
                                    .p_3()
                                    .rounded(theme::r_md())
                                    .bg(if on {
                                        theme::surface_active()
                                    } else {
                                        theme::window_glass()
                                    })
                                    .border_1()
                                    .border_color(if on {
                                        theme::emerald()
                                    } else {
                                        theme::hairline()
                                    })
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .cursor_pointer()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.0))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_delegation_preset(id, cx);
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme::text_primary())
                                                    .child(title),
                                            )
                                            .when(rec, |r| {
                                                r.child(
                                                    div()
                                                        .text_size(px(10.5))
                                                        .text_color(theme::emerald())
                                                        .child("Recommended"),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .line_height(px(17.0))
                                            .text_color(theme::text_muted())
                                            .child(desc),
                                    )
                            }))
                            .child(
                                div()
                                    .pt_1()
                                    .text_size(px(11.0))
                                    .text_color(theme::text_muted())
                                    .child("Always human-confirmed: booking slots, live interviews, offers, CAPTCHAs, and secrets."),
                            )
                            .into_any_element()
                    }
                };

                let card =
                    div()
                        .id("setup-wizard-card")
                        .occlude()
                        .w(px(560.0))
                        .max_h(px(620.0))
                        .rounded(theme::r_lg())
                        .bg(theme::popover_glass())
                        .border_1()
                        .border_color(theme::hairline_strong())
                        .shadow_lg()
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .px_5()
                                .py_4()
                                .border_b_1()
                                .border_color(theme::hairline())
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.0))
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme::text_primary())
                                                .child("Workspace Setup"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme::text_muted())
                                                .child(step_titles[step]),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("wiz-close")
                                        .px(px(6.0))
                                        .py(px(2.0))
                                        .rounded(theme::r_xs())
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.active_popover = ActivePopover::None;
                                            cx.notify();
                                        }))
                                        .child("✕"),
                                ),
                        )
                        .child(div().px_5().pt_3().flex().items_center().gap_1().children(
                            (0..4).map(|si| {
                                div()
                                    .id(SharedString::from(format!("wiz-tab-{si}")))
                                    .flex_1()
                                    .h(px(3.0))
                                    .rounded(px(99.0))
                                    .bg(if si <= step {
                                        theme::lavender()
                                    } else {
                                        theme::surface_subtle()
                                    })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.setup_step = si;
                                        cx.notify();
                                    }))
                            }),
                        ))
                        .child(
                            div()
                                .id("wiz-body-scroll")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .px_5()
                                .py_4()
                                .child(step_body),
                        )
                        .child(
                            div()
                                .px_5()
                                .py_3()
                                .border_t_1()
                                .border_color(theme::hairline())
                                .bg(theme::window_glass())
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .id("wiz-prev")
                                        .px_3()
                                        .py(px(5.0))
                                        .rounded(theme::r_sm())
                                        .when(step > 0, |d| {
                                            d.hover(|s| s.bg(theme::row_hover())).cursor_pointer()
                                        })
                                        .text_xs()
                                        .text_color(if step > 0 {
                                            theme::text_secondary()
                                        } else {
                                            theme::hairline_strong()
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if this.setup_step > 0 {
                                                this.setup_step -= 1;
                                                cx.notify();
                                            }
                                        }))
                                        .child("Back"),
                                )
                                .child(
                                    div()
                                        .id("wiz-next")
                                        .px_3()
                                        .py(px(5.0))
                                        .rounded(theme::r_sm())
                                        .bg(theme::surface_active())
                                        .hover(|s| s.bg(theme::row_hover()))
                                        .cursor_pointer()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::text_primary())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if this.setup_step < 3 {
                                                this.setup_step += 1;
                                                cx.notify();
                                            } else {
                                                this.active_popover = ActivePopover::None;
                                                this.set_toast("Setup saved", cx);
                                            }
                                        }))
                                        .child(if step < 3 { "Next →" } else { "Done" }),
                                ),
                        );

                Some(
                    div()
                        .id("setup-wizard-backdrop")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .bg(gpui::rgba(0x0505089e))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.active_popover = ActivePopover::None;
                                cx.notify();
                            }),
                        )
                        .child(if self.reduce_motion {
                            card.into_any_element()
                        } else {
                            card.with_animation(
                                SharedString::from(format!("pop-wiz-{epoch}")),
                                Animation::new(Duration::from_millis(140))
                                    .with_easing(ease_out_quint()),
                                |el, t| el.opacity(t).mt(px(-8.0 * (1.0 - t))),
                            )
                            .into_any_element()
                        })
                        .into_any_element(),
                )
            }
            ActivePopover::Tour => {
                let step = self.tour_step.min(4);
                let prev_step = self.tour_prev_step.min(4);
                let anim_epoch = self.tour_anim_epoch;
                let reduce_motion = self.reduce_motion;
                let vw = f32::from(viewport.width).max(720.0);
                let vh = f32::from(viewport.height).max(480.0);
                let card_w = 340.0_f32;
                let card_h = 166.0_f32;

                // Driver.js-style live spotlight cutout rects (x, y, w, h) + anchored popover (px, py)
                let stop_geom = |idx: usize| -> ((f32, f32, f32, f32), (f32, f32)) {
                    let chat_off = if self.chat_open { self.chat_width } else { 0.0 };
                    match idx {
                        // 1. Search & Command bar (top center)
                        0 => {
                            let sw = 276.0_f32;
                            let sx = ((vw - sw) * 0.5).clamp(110.0, (vw - sw - 230.0).max(110.0));
                            let sy = 6.0;
                            let sh = 34.0;
                            let cx_pos =
                                (sx + sw * 0.5 - card_w * 0.5).clamp(16.0, vw - card_w - 16.0);
                            ((sx, sy, sw, sh), (cx_pos, sy + sh + 14.0))
                        }
                        // 2. Board / Sankey switch & Attention filter (top-right toolbar)
                        1 => {
                            let sw = 286.0_f32;
                            let sx = (vw - sw - 14.0).max(chat_off + 16.0);
                            let sy = 50.0;
                            let sh = 34.0;
                            let cx_pos = (vw - card_w - 20.0).max(16.0);
                            ((sx, sy, sw, sh), (cx_pos, sy + sh + 14.0))
                        }
                        // 3. CVs, Profile, Tidy & Settings icons (top-right header)
                        2 => {
                            let sw = 188.0_f32;
                            let sx = (vw - sw - 12.0).max(16.0);
                            let sy = 6.0;
                            let sh = 34.0;
                            let cx_pos = (vw - card_w - 20.0).max(16.0);
                            ((sx, sy, sw, sh), (cx_pos, sy + sh + 14.0))
                        }
                        // 4. Quick Ask / AI Chat rail (bottom-left)
                        3 => {
                            let sx = if self.chat_open { 10.0 } else { 14.0 };
                            let sw = if self.chat_open {
                                (self.chat_width - 20.0).clamp(160.0, 320.0)
                            } else {
                                46.0
                            };
                            let sh = 42.0;
                            let sy = (vh - sh - 10.0).max(80.0);
                            let cx_pos = (sx + sw + 14.0).min(vw - card_w - 16.0);
                            let cy_pos = (sy + sh - card_h).clamp(56.0, vh - card_h - 16.0);
                            ((sx, sy, sw, sh), (cx_pos, cy_pos))
                        }
                        // 5. Daily cap, 1-click Refresh & Health (bottom-right dock)
                        _ => {
                            let sw = 216.0_f32;
                            let sh = 38.0;
                            let sx = (vw - sw - 12.0).max(16.0);
                            let sy = (vh - sh - 8.0).max(80.0);
                            let cx_pos = (vw - card_w - 20.0).max(16.0);
                            let cy_pos = (sy - card_h - 14.0).max(56.0);
                            ((sx, sy, sw, sh), (cx_pos, cy_pos))
                        }
                    }
                };

                let stops: [(&str, &str); 5] = [
                    (
                        "Search & jump anywhere",
                        "Filter all roles instantly across active and archived columns, or press ⌘K for quick commands.",
                    ),
                    (
                        "Board & Sankey Flow",
                        "Switch between Kanban and the interactive funnel, or filter straight to roles needing attention.",
                    ),
                    (
                        "CVs, Profile, Tidy & Settings",
                        "Toggle child CV tracks and cover letters, review quiet threads, or tune your delegation safety mode.",
                    ),
                    (
                        "Ask your agent anytime",
                        "Open Quick Ask or press ⌘\\ to chat with Hermes, Claude Code, Codex, or Pi over your local ledger.",
                    ),
                    (
                        "Daily cap & instant sync",
                        "Track your daily submission budget, inspect workspace health, and refresh live from disk with one click.",
                    ),
                ];

                let (title, desc) = stops[step];
                let ((x0, y0, w0, h0), (px0, py0)) = stop_geom(prev_step);
                let ((x1, y1, w1, h1), (px1, py1)) = stop_geom(step);
                let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
                let scrim_col = gpui::rgba(0x050508a8);
                let dur = Duration::from_millis(if reduce_motion || prev_step == step {
                    1
                } else {
                    260
                });

                let popover_card =
                    div()
                        .id("tour-spotlight-card")
                        .occlude()
                        .w(px(card_w))
                        .rounded(theme::r_lg())
                        .bg(theme::popover_glass())
                        .border_1()
                        .border_color(theme::hairline_strong())
                        .shadow_lg()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme::text_primary())
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .id("tour-skip")
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .hover(|s| s.text_color(theme::text_primary()))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.active_popover = ActivePopover::None;
                                            cx.notify();
                                        }))
                                        .child("Skip"),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .line_height(px(18.0))
                                .text_color(theme::text_secondary())
                                .child(desc),
                        )
                        .child(
                            div()
                                .pt_2()
                                .border_t_1()
                                .border_color(theme::hairline())
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().flex().items_center().gap(px(5.0)).children(
                                    (0..5).map(|i| {
                                        div()
                                            .w(if i == step { px(16.0) } else { px(6.0) })
                                            .h(px(6.0))
                                            .rounded(px(99.0))
                                            .bg(if i == step {
                                                theme::lavender()
                                            } else {
                                                theme::surface_active()
                                            })
                                    }),
                                ))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .when(step > 0, |r| {
                                            r.child(
                                                div()
                                                    .id("tour-back")
                                                    .px_2()
                                                    .py(px(4.0))
                                                    .rounded(theme::r_sm())
                                                    .hover(|s| s.bg(theme::row_hover()))
                                                    .cursor_pointer()
                                                    .text_xs()
                                                    .text_color(theme::text_secondary())
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        if this.tour_step > 0 {
                                                            this.tour_prev_step = this.tour_step;
                                                            this.tour_step -= 1;
                                                            this.tour_anim_epoch = this
                                                                .tour_anim_epoch
                                                                .wrapping_add(1);
                                                            cx.notify();
                                                        }
                                                    }))
                                                    .child("Back"),
                                            )
                                        })
                                        .child(
                                            div()
                                                .id("tour-next")
                                                .px_3()
                                                .py(px(4.0))
                                                .rounded(theme::r_sm())
                                                .bg(theme::surface_active())
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme::text_primary())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    if this.tour_step < 4 {
                                                        this.tour_prev_step = this.tour_step;
                                                        this.tour_step += 1;
                                                        this.tour_anim_epoch =
                                                            this.tour_anim_epoch.wrapping_add(1);
                                                        cx.notify();
                                                    } else {
                                                        this.active_popover = ActivePopover::None;
                                                        cx.notify();
                                                    }
                                                }))
                                                .child(if step < 4 { "Next" } else { "Done" }),
                                        ),
                                ),
                        );

                // Smoothly morph all 4 cutout scrim walls + spotlight ring + popover (ZERO opacity flash!)
                Some(
                    div()
                        .id("tour-backdrop")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.active_popover = ActivePopover::None;
                                cx.notify();
                            }),
                        )
                        // Top scrim
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .w(px(vw))
                                .bg(scrim_col)
                                .with_animation(
                                    SharedString::from(format!("tour-top-{anim_epoch}")),
                                    Animation::new(dur).with_easing(ease_out_quint()),
                                    move |el, t| el.h(px(lerp(y0, y1, t).max(0.0))),
                                ),
                        )
                        // Bottom scrim
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .w(px(vw))
                                .bg(scrim_col)
                                .with_animation(
                                    SharedString::from(format!("tour-bot-{anim_epoch}")),
                                    Animation::new(dur).with_easing(ease_out_quint()),
                                    move |el, t| {
                                        let top = (lerp(y0, y1, t) + lerp(h0, h1, t)).min(vh);
                                        el.top(px(top)).h(px((vh - top).max(0.0)))
                                    },
                                ),
                        )
                        // Left scrim
                        .child(div().absolute().left_0().bg(scrim_col).with_animation(
                            SharedString::from(format!("tour-left-{anim_epoch}")),
                            Animation::new(dur).with_easing(ease_out_quint()),
                            move |el, t| {
                                el.top(px(lerp(y0, y1, t)))
                                    .h(px(lerp(h0, h1, t)))
                                    .w(px(lerp(x0, x1, t).max(0.0)))
                            },
                        ))
                        // Right scrim
                        .child(div().absolute().bg(scrim_col).with_animation(
                            SharedString::from(format!("tour-right-{anim_epoch}")),
                            Animation::new(dur).with_easing(ease_out_quint()),
                            move |el, t| {
                                let lx = (lerp(x0, x1, t) + lerp(w0, w1, t)).min(vw);
                                el.top(px(lerp(y0, y1, t)))
                                    .h(px(lerp(h0, h1, t)))
                                    .left(px(lx))
                                    .w(px((vw - lx).max(0.0)))
                            },
                        ))
                        // Crisp spotlight ring framing the live cutout
                        .child(
                            div()
                                .absolute()
                                .rounded(theme::r_md())
                                .border_1()
                                .border_color(theme::lavender())
                                .with_animation(
                                    SharedString::from(format!("tour-ring-{anim_epoch}")),
                                    Animation::new(dur).with_easing(ease_out_quint()),
                                    move |el, t| {
                                        el.left(px(lerp(x0, x1, t)))
                                            .top(px(lerp(y0, y1, t)))
                                            .w(px(lerp(w0, w1, t)))
                                            .h(px(lerp(h0, h1, t)))
                                    },
                                ),
                        )
                        // Smoothly gliding minimal popover card
                        .child(div().absolute().child(popover_card).with_animation(
                            SharedString::from(format!("tour-card-{anim_epoch}")),
                            Animation::new(dur).with_easing(ease_out_quint()),
                            move |el, t| el.left(px(lerp(px0, px1, t))).top(px(lerp(py0, py1, t))),
                        ))
                        .into_any_element(),
                )
            }
            ActivePopover::Budget => {
                let b = &self.snapshot.daily_budget;
                let right_off = self.bottom_popover_right_offset();
                let epoch = self.cmd_anim_epoch;
                let card = popover_shell(true, 330.0, right_off)
                    .id("budget-popover-card")
                    .occlude()
                    .on_hover(cx.listener(|this, hovered, _, cx| {
                        this.on_dock_panel_hover(*hovered, cx);
                    }))
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
                    ));
                Some(if self.reduce_motion {
                    card.into_any_element()
                } else {
                    card.with_animation(
                        SharedString::from(format!("pop-budget-{epoch}")),
                        Animation::new(Duration::from_millis(130)).with_easing(ease_out_quint()),
                        |el, t| el.opacity(t).mb(px(-6.0 * (1.0 - t))),
                    )
                    .into_any_element()
                })
            }
            ActivePopover::Health => {
                let right_off = self.bottom_popover_right_offset();
                let epoch = self.cmd_anim_epoch;
                let unresolved = self.snapshot.unresolved_attempts;
                let rt_ready = self.runtime.is_ready();
                let is_healthy = unresolved == 0 && rt_ready;
                let ws_loc = format!(
                    "{} ({})",
                    self.client.target.label(),
                    if self.client.target.is_remote() {
                        "Remote"
                    } else {
                        "Local"
                    }
                );
                let upcoming_n = self.snapshot.upcoming_count;
                let total_roles = self.snapshot.total_records;
                let card = popover_shell(true, 330.0, right_off)
                    .id("health-popover-card")
                    .occlude()
                    .on_hover(cx.listener(|this, hovered, _, cx| {
                        this.on_dock_panel_hover(*hovered, cx);
                    }))
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .pb_2()
                            .border_b_1()
                            .border_color(theme::hairline())
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme::text_primary())
                                            .child("Workspace"),
                                    )
                                    .child(
                                        div()
                                            .px(px(6.0))
                                            .py(px(1.5))
                                            .rounded(theme::r_xs())
                                            .bg(if is_healthy {
                                                gpui::rgba(0x34d3991c)
                                            } else {
                                                theme::amber_wash()
                                            })
                                            .text_size(px(10.0))
                                            .text_color(if is_healthy {
                                                theme::emerald()
                                            } else {
                                                theme::amber_text()
                                            })
                                            .child(if is_healthy {
                                                "Healthy"
                                            } else {
                                                "Needs review"
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .id("pop-close-health")
                                    .px_1()
                                    .rounded(theme::r_xs())
                                    .hover(|s| {
                                        s.bg(theme::row_hover()).text_color(theme::text_primary())
                                    })
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.active_popover = ActivePopover::None;
                                        this.popover_pinned = false;
                                        cx.notify();
                                    }))
                                    .child("✕"),
                            ),
                    )
                    .child(kv_row("Location", ws_loc))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .text_xs()
                            .child(div().text_color(theme::text_muted()).child("Agent"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .w(px(6.0))
                                            .h(px(6.0))
                                            .rounded(px(99.0))
                                            .bg(if rt_ready {
                                                theme::emerald()
                                            } else {
                                                theme::amber_accent()
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_color(if rt_ready {
                                                theme::text_primary()
                                            } else {
                                                theme::amber_text()
                                            })
                                            .child(self.runtime.label()),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .text_xs()
                            .child(div().text_color(theme::text_muted()).child("Pipeline"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_color(theme::text_primary())
                                            .child(format!("{total_roles} roles")),
                                    )
                                    .when(upcoming_n > 0, |row| {
                                        row.child(
                                            div()
                                                .id("health-focus-upcoming")
                                                .px(px(6.0))
                                                .py(px(2.0))
                                                .rounded(theme::r_xs())
                                                .bg(theme::surface_subtle())
                                                .hover(|s| {
                                                    s.bg(theme::row_hover())
                                                        .text_color(theme::text_primary())
                                                })
                                                .cursor_pointer()
                                                .text_color(theme::text_secondary())
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.active_popover = ActivePopover::None;
                                                    this.popover_pinned = false;
                                                    this.flow_view = false;
                                                    this.only_attention = true;
                                                    this.attention_toggled = true;
                                                    this.attention_anim_epoch =
                                                        this.attention_anim_epoch.wrapping_add(1);
                                                    cx.notify();
                                                }))
                                                .child(format!("{upcoming_n} upcoming →")),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .text_xs()
                            .child(
                                div()
                                    .text_color(theme::text_muted())
                                    .child("Unverified submits"),
                            )
                            .child(if unresolved > 0 {
                                div()
                                    .id("health-review-unverified")
                                    .px(px(6.0))
                                    .py(px(2.0))
                                    .rounded(theme::r_xs())
                                    .bg(theme::amber_wash())
                                    .hover(|s| s.bg(theme::row_hover()))
                                    .cursor_pointer()
                                    .text_color(theme::amber_text())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.active_popover = ActivePopover::None;
                                        this.popover_pinned = false;
                                        if !this.chat_open {
                                            this.chat_open = true;
                                            this.chat_anim_epoch =
                                                this.chat_anim_epoch.wrapping_add(1);
                                        }
                                        this.submit_prompt(
                                            "Summarize our unverified submissions and suggest the highest-priority ones to check first.".into(),
                                            false,
                                            cx,
                                        );
                                    }))
                                    .child(format!("{unresolved} Review →"))
                                    .into_any_element()
                            } else {
                                div()
                                    .text_color(theme::emerald())
                                    .child("All clear")
                                    .into_any_element()
                            }),
                    );
                Some(if self.reduce_motion {
                    card.into_any_element()
                } else {
                    card.with_animation(
                        SharedString::from(format!("pop-health-{epoch}")),
                        Animation::new(Duration::from_millis(130)).with_easing(ease_out_quint()),
                        |el, t| el.opacity(t).mb(px(-6.0 * (1.0 - t))),
                    )
                    .into_any_element()
                })
            }
        }
    }
}

impl Render for JobHarnessApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus_handle);
                    let mut dirty = false;
                    if matches!(
                        this.active_popover,
                        ActivePopover::Budget | ActivePopover::Health
                    ) {
                        this.active_popover = ActivePopover::None;
                        this.popover_pinned = false;
                        dirty = true;
                    }
                    if this.editing_config_key.is_some() {
                        this.cancel_inline_config(cx);
                    }
                    if matches!(
                        this.active_input,
                        ActiveInput::Search
                            | ActiveInput::QuickAsk
                            | ActiveInput::SankeyFilter
                            | ActiveInput::ModelField
                    ) {
                        this.active_input = ActiveInput::None;
                        dirty = true;
                    }
                    if dirty {
                        cx.notify();
                    }
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
                    .when(self.chat_open && self.chat_fullscreen, |d| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .overflow_hidden()
                                .child(self.render_chat_rail(cx)),
                        )
                    })
                    .when(self.chat_open && !self.chat_fullscreen, |d| {
                        let epoch = self.chat_anim_epoch;
                        let target_w = self.chat_width;
                        let dragging = matches!(self.resizing_rail, Some((ResizeRail::Chat, _, _)));
                        let rail = div()
                            .h_full()
                            .flex_shrink_0()
                            .overflow_hidden()
                            .child(self.render_chat_rail(cx));
                        // Once resized, stay static width so releasing drag NEVER replays open animation!
                        let rail_el = if self.reduce_motion || dragging || self.chat_resized {
                            rail.w(px(target_w)).into_any_element()
                        } else {
                            rail.with_animation(
                                SharedString::from(format!("chat-slide-{epoch}")),
                                Animation::new(Duration::from_millis(160))
                                    .with_easing(ease_out_quint()),
                                move |el, t| el.w(px(target_w * t)).opacity(t),
                            )
                            .into_any_element()
                        };
                        let handle = div()
                            .id("resize-chat-rail")
                            .w(px(7.0))
                            .ml(px(-3.0))
                            .mr(px(-4.0))
                            .h_full()
                            .flex_shrink_0()
                            .cursor_col_resize()
                            .flex()
                            .justify_center()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.chat_resized = true;
                                    if ev.click_count >= 2 {
                                        this.chat_width = 300.0;
                                        this.resizing_rail = None;
                                    } else {
                                        let sx: f32 = ev.position.x.into();
                                        this.resizing_rail =
                                            Some((ResizeRail::Chat, sx, this.chat_width));
                                    }
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .w(px(2.0))
                                    .h_full()
                                    .bg(if dragging {
                                        theme::lavender()
                                    } else {
                                        gpui::rgba(0x00000000)
                                    })
                                    .hover(|s| s.bg(theme::lavender_border())),
                            );
                        d.child(rail_el).child(handle)
                    })
                    .when(!(self.chat_open && self.chat_fullscreen), |d| {
                        d.child(self.render_board(cx))
                    })
                    .when_some(self.render_right_drawer(cx), |d, drawer| {
                        let epoch = self.drawer_anim_epoch;
                        let target_w = self.drawer_width;
                        let dragging =
                            matches!(self.resizing_rail, Some((ResizeRail::RightDrawer, _, _)));
                        let handle = div()
                            .id("resize-right-drawer")
                            .w(px(7.0))
                            .ml(px(-4.0))
                            .mr(px(-3.0))
                            .h_full()
                            .flex_shrink_0()
                            .cursor_col_resize()
                            .flex()
                            .justify_center()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.drawer_resized = true;
                                    if ev.click_count >= 2 {
                                        this.drawer_width = 360.0;
                                        this.resizing_rail = None;
                                    } else {
                                        let sx: f32 = ev.position.x.into();
                                        this.resizing_rail =
                                            Some((ResizeRail::RightDrawer, sx, this.drawer_width));
                                    }
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .w(px(2.0))
                                    .h_full()
                                    .bg(if dragging {
                                        theme::lavender()
                                    } else {
                                        gpui::rgba(0x00000000)
                                    })
                                    .hover(|s| s.bg(theme::lavender_border())),
                            );
                        let box_el = div()
                            .h_full()
                            .flex_shrink_0()
                            .overflow_hidden()
                            .child(drawer);
                        let drawer_el = if self.reduce_motion || dragging || self.drawer_resized {
                            box_el.w(px(target_w)).into_any_element()
                        } else {
                            box_el
                                .with_animation(
                                    SharedString::from(format!("drawer-slide-{epoch}")),
                                    Animation::new(Duration::from_millis(160))
                                        .with_easing(ease_out_quint()),
                                    move |el, t| el.w(px(target_w * t)).opacity(t),
                                )
                                .into_any_element()
                        };
                        d.child(handle).child(drawer_el)
                    }),
            )
            // Transparent full-window capture shield while scrubbing a numeric config (Figma/Blender pattern)
            .when(self.scrub_config.is_some(), |d| {
                d.child(
                    div()
                        .id("config-scrub-shield")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .cursor_row_resize()
                        .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                            let Some((key, start_y, start_val, cur_val, did_drag)) =
                                this.scrub_config
                            else {
                                return;
                            };
                            let Some((step, min_v, max_v)) = numeric_scrub_spec(key) else {
                                return;
                            };
                            let cy: f32 = ev.position.y.into();
                            let dy = start_y - cy; // drag up = increase, drag down = decrease
                            if !did_drag && dy.abs() < 4.0 {
                                return;
                            }
                            let mult = if ev.modifiers.shift { 5 } else { 1 };
                            let steps = (dy / 6.0).round() as i64;
                            let next = (start_val + steps * step * mult).clamp(min_v, max_v);
                            this.scrub_config = Some((key, start_y, start_val, next, true));
                            if next != cur_val {
                                match key {
                                    "prep_reminder_minutes" => {
                                        this.snapshot.permissions.brief_lead_time_minutes = next;
                                    }
                                    "applied_ghost_days" => {
                                        this.snapshot.profile_settings.applied_ghost_days =
                                            next.max(1) as u64;
                                    }
                                    "min_base_salary" => {
                                        this.snapshot.profile_settings.min_base_salary =
                                            next.max(0) as u64;
                                    }
                                    "target_base_salary" => {
                                        this.snapshot.profile_settings.target_base_salary =
                                            next.max(0) as u64;
                                    }
                                    _ => {}
                                }
                                cx.notify();
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _ev: &MouseUpEvent, _, cx| {
                                if let Some((key, _, start_val, cur_val, did_drag)) =
                                    this.scrub_config.take()
                                {
                                    if did_drag {
                                        if cur_val != start_val {
                                            this.editing_config_key = Some(key);
                                            this.config_draft = cur_val.to_string();
                                            this.commit_inline_config(cx);
                                        } else {
                                            cx.notify();
                                        }
                                    } else {
                                        // Clean click (<4px drag) opens borderless inline typing!
                                        this.start_inline_config(key, start_val.to_string(), cx);
                                    }
                                }
                            }),
                        ),
                )
            })
            // Transparent full-window capture shield while dragging a side-rail divider (Zed GPUI pattern)
            .when(self.resizing_rail.is_some(), |d| {
                d.child(
                    div()
                        .id("rail-resize-shield")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .cursor_col_resize()
                        .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                            let Some((rail, start_x, start_w)) = this.resizing_rail else {
                                return;
                            };
                            let mx: f32 = ev.position.x.into();
                            let dx = mx - start_x;
                            let win_w: f32 = window.viewport_size().width.into();
                            match rail {
                                ResizeRail::Chat => {
                                    let max_w = (win_w * 0.46).clamp(300.0, 560.0);
                                    let raw = (start_w + dx).clamp(260.0, max_w);
                                    // Gentle magnetic detent at default 300px only — silky 1:1 elsewhere
                                    let next = snap_width(raw, &[300.0], 6.0);
                                    if (this.chat_width - next).abs() > 0.25 {
                                        this.chat_width = next;
                                        cx.notify();
                                    }
                                }
                                ResizeRail::RightDrawer => {
                                    let max_w = (win_w * 0.48).clamp(340.0, 620.0);
                                    let raw = (start_w - dx).clamp(300.0, max_w);
                                    let next = snap_width(raw, &[360.0], 6.0);
                                    if (this.drawer_width - next).abs() > 0.25 {
                                        this.drawer_width = next;
                                        cx.notify();
                                    }
                                }
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _ev: &MouseUpEvent, _, cx| {
                                this.resizing_rail = None;
                                cx.notify();
                            }),
                        ),
                )
            })
            .when_some(self.expanded_reader.clone(), |d, (title, body)| {
                let local_open = self
                    .client
                    .target
                    .local_root()
                    .map(|r| r.join(&title))
                    .filter(|p| p.is_file());
                let local_reveal = local_open.clone();
                let copy_reader_body = body.clone();
                let parsed_json: Option<serde_json::Value> = if title.ends_with(".json")
                    || body.trim_start().starts_with('{')
                    || body.trim_start().starts_with('[')
                {
                    serde_json::from_str(&body).ok()
                } else {
                    None
                };
                let is_json_doc = parsed_json.is_some();
                let sections = if is_json_doc {
                    Vec::new()
                } else {
                    parse_brief_sections(&body)
                };
                let sec_count = sections.len();
                let active_sec = self.brief_section.filter(|&i| i < sec_count);
                let visible_md = match active_sec {
                    Some(i) => sections[i].body.clone(),
                    None => body.clone(),
                };
                let sec_epoch = self.brief_section_epoch;
                let has_rail = sec_count >= 2;
                let word_count = body.split_whitespace().count();
                let read_mins = ((word_count as f32) / 220.0).round().max(1.0) as usize;
                let modal_w = if has_rail { 960.0 } else { 760.0 };
                let modal_h = if has_rail { 680.0 } else { 620.0 };
                let modal_card = div()
                    .id("md-reader-modal")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .w(px(modal_w))
                    .h(px(modal_h))
                    .p_6()
                    .rounded(theme::r_lg())
                    .bg(theme::popover_glass())
                    .border_1()
                    .border_color(theme::hairline_strong())
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .pb_3()
                            .border_b_1()
                            .border_color(theme::hairline())
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child({
                                        let has_file = local_reveal.is_some();
                                        div()
                                            .id("reader-title-reveal")
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_sm()
                                            .text_color(theme::text_primary())
                                            .when(has_file, |d| {
                                                let p = local_reveal.unwrap();
                                                d.underline()
                                                    .cursor_pointer()
                                                    .hover(|s| s.text_color(theme::lavender()))
                                                    .on_click(cx.listener(move |_, _, _, _| {
                                                        reveal_in_file_manager(&p);
                                                    }))
                                            })
                                            .child(truncate_label(&title, 56))
                                    })
                                    .child({
                                        let copied = self.is_copied_flash("reader-copy-all");
                                        div()
                                            .id("reader-copy-all")
                                            .p(px(4.0))
                                            .rounded(theme::r_xs())
                                            .hover(|s| s.bg(theme::row_hover()))
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    copy_reader_body.clone(),
                                                ));
                                                this.flash_copied("reader-copy-all", cx);
                                                this.set_toast("Copied text", cx);
                                            }))
                                            .child(
                                                svg()
                                                    .path(if copied {
                                                        "icons/check.svg"
                                                    } else {
                                                        "icons/copy.svg"
                                                    })
                                                    .size(px(13.5))
                                                    .text_color(if copied {
                                                        theme::emerald()
                                                    } else {
                                                        theme::text_muted()
                                                    }),
                                            )
                                    }),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .when(is_json_doc, |r| {
                                        let raw_on = self.reader_raw_json;
                                        r.child(
                                            div()
                                                .p(px(2.0))
                                                .rounded(theme::r_sm())
                                                .bg(theme::window_glass())
                                                .border_1()
                                                .border_color(theme::hairline())
                                                .flex()
                                                .items_center()
                                                .gap(px(2.0))
                                                .child(
                                                    div()
                                                        .id("reader-json-fmt")
                                                        .px_2()
                                                        .py(px(2.0))
                                                        .rounded(theme::r_xs())
                                                        .when(!raw_on, |d| {
                                                            d.bg(theme::surface_active())
                                                        })
                                                        .cursor_pointer()
                                                        .text_size(px(11.0))
                                                        .text_color(if !raw_on {
                                                            theme::text_primary()
                                                        } else {
                                                            theme::text_muted()
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.reader_raw_json = false;
                                                            cx.notify();
                                                        }))
                                                        .child("Readable"),
                                                )
                                                .child(
                                                    div()
                                                        .id("reader-json-raw")
                                                        .px_2()
                                                        .py(px(2.0))
                                                        .rounded(theme::r_xs())
                                                        .when(raw_on, |d| {
                                                            d.bg(theme::surface_active())
                                                        })
                                                        .cursor_pointer()
                                                        .text_size(px(11.0))
                                                        .text_color(if raw_on {
                                                            theme::text_primary()
                                                        } else {
                                                            theme::text_muted()
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.reader_raw_json = true;
                                                            cx.notify();
                                                        }))
                                                        .child("Raw JSON"),
                                                ),
                                        )
                                    })
                                    .when_some(local_open, |r, p| {
                                        r.child(
                                            div()
                                                .id("reader-open-ext")
                                                .text_xs()
                                                .text_color(theme::lavender())
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |_, _, _, _| {
                                                    open_in_system(&p);
                                                }))
                                                .child("Open in editor ↗"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id("reader-close")
                                            .px(px(6.0))
                                            .py(px(2.0))
                                            .rounded(theme::r_xs())
                                            .hover(|s| s.bg(theme::row_hover()))
                                            .text_xs()
                                            .text_color(theme::text_muted())
                                            .cursor_pointer()
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.expanded_reader = None;
                                                cx.notify();
                                            }))
                                            .child("✕"),
                                    ),
                            ),
                    )
                    // Editorial 2-column body: sticky left Contents chapter rail (owenabel.com StoryLayout pattern) + optimal-measure reading pane
                    .child({
                        let reader_sel = self.active_text_selection();
                        let reader_content: AnyElement = if let Some(json_val) = &parsed_json {
                            if self.reader_raw_json {
                                let pretty = serde_json::to_string_pretty(json_val)
                                    .unwrap_or_else(|_| body.clone());
                                let code_id = SharedString::from("reader-json-raw-code");
                                let sr = reader_sel.as_ref().and_then(|s| {
                                    s.range_for_block(code_id.as_ref(), 0, pretty.len())
                                });
                                let blocks_rc =
                                    Rc::new(std::cell::RefCell::new(vec![pretty.clone()]));
                                markdown::render_syntax_code(
                                    code_id.clone(),
                                    code_id,
                                    0,
                                    blocks_rc,
                                    &pretty,
                                    "json",
                                    true,
                                    sr,
                                    self.md_select_handler(cx),
                                )
                            } else {
                                markdown::render_json_structured(
                                    "reader-json-fmt",
                                    json_val,
                                    reader_sel.as_ref(),
                                    self.md_link_handler(cx),
                                    self.md_select_handler(cx),
                                )
                            }
                        } else if title.ends_with(".json") {
                            let code_id = SharedString::from("reader-json-fallback");
                            let sr = reader_sel
                                .as_ref()
                                .and_then(|s| s.range_for_block(code_id.as_ref(), 0, body.len()));
                            let blocks_rc = Rc::new(std::cell::RefCell::new(vec![body.clone()]));
                            markdown::render_syntax_code(
                                code_id.clone(),
                                code_id,
                                0,
                                blocks_rc,
                                &body,
                                "json",
                                true,
                                sr,
                                self.md_select_handler(cx),
                            )
                        } else {
                            markdown::render_markdown(
                                "reader-md",
                                &visible_md,
                                true,
                                reader_sel.as_ref(),
                                self.md_link_handler(cx),
                                self.md_select_handler(cx),
                            )
                            .into_any_element()
                        };
                        let body_view = div()
                            .id("md-reader-scroll")
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .pr_2()
                            .overflow_y_scroll()
                            .child(div().max_w(px(660.0)).w_full().child(reader_content));
                        let animated_body = if self.reduce_motion {
                            body_view.into_any_element()
                        } else {
                            body_view
                                .with_animation(
                                    SharedString::from(format!("rbody-{sec_epoch}")),
                                    Animation::new(Duration::from_millis(120))
                                        .with_easing(ease_out_quint()),
                                    |el, t| el.opacity(0.4 + 0.6 * t),
                                )
                                .into_any_element()
                        };
                        if !has_rail {
                            animated_body
                        } else {
                            let chapter_rail = div()
                                .w(px(208.0))
                                .flex_shrink_0()
                                .h_full()
                                .pr_4()
                                .border_r_1()
                                .border_color(theme::hairline())
                                .flex()
                                .flex_col()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .pb_2()
                                                .text_size(px(10.5))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme::text_muted())
                                                .child("CONTENTS"),
                                        )
                                        .child(
                                            div()
                                                .id("rsec-all")
                                                .px_2()
                                                .py(px(5.0))
                                                .rounded(theme::r_sm())
                                                .border_l_2()
                                                .border_color(if active_sec.is_none() {
                                                    theme::lavender()
                                                } else {
                                                    gpui::rgba(0x00000000)
                                                })
                                                .when(active_sec.is_none(), |d| {
                                                    d.bg(theme::surface_subtle())
                                                })
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .text_xs()
                                                .font_weight(if active_sec.is_none() {
                                                    FontWeight::MEDIUM
                                                } else {
                                                    FontWeight::NORMAL
                                                })
                                                .text_color(if active_sec.is_none() {
                                                    theme::text_primary()
                                                } else {
                                                    theme::text_muted()
                                                })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.brief_section = None;
                                                    this.brief_section_epoch =
                                                        this.brief_section_epoch.wrapping_add(1);
                                                    cx.notify();
                                                }))
                                                .child("Full document"),
                                        )
                                        .children(sections.iter().enumerate().map(|(idx, sec)| {
                                            let on = active_sec == Some(idx);
                                            div()
                                                .id(SharedString::from(format!("rsec-{idx}")))
                                                .px_2()
                                                .py(px(5.0))
                                                .rounded(theme::r_sm())
                                                .border_l_2()
                                                .border_color(if on {
                                                    theme::lavender()
                                                } else {
                                                    gpui::rgba(0x00000000)
                                                })
                                                .when(on, |d| d.bg(theme::surface_subtle()))
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .text_xs()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.brief_section = Some(idx);
                                                    this.brief_section_epoch =
                                                        this.brief_section_epoch.wrapping_add(1);
                                                    cx.notify();
                                                }))
                                                .child(
                                                    div()
                                                        .text_size(px(10.5))
                                                        .text_color(if on {
                                                            theme::lavender()
                                                        } else {
                                                            theme::text_muted()
                                                        })
                                                        .child(format!("{}", idx + 1)),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .font_weight(if on {
                                                            FontWeight::MEDIUM
                                                        } else {
                                                            FontWeight::NORMAL
                                                        })
                                                        .text_color(if on {
                                                            theme::text_primary()
                                                        } else {
                                                            theme::text_secondary()
                                                        })
                                                        .child(sec.label.clone()),
                                                )
                                        })),
                                )
                                .child(
                                    div()
                                        .pt_3()
                                        .border_t_1()
                                        .border_color(theme::hairline())
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_size(px(11.0))
                                                .text_color(theme::text_muted())
                                                .child(format!("{read_mins} min read")),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(2.0))
                                                .text_xs()
                                                .text_color(theme::text_secondary())
                                                .child(
                                                    div()
                                                        .id("rsec-prev")
                                                        .w(px(22.0))
                                                        .h(px(22.0))
                                                        .rounded(theme::r_xs())
                                                        .hover(|s| {
                                                            s.bg(theme::row_hover())
                                                                .text_color(theme::text_primary())
                                                        })
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .cursor_pointer()
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.brief_section = match this
                                                                    .brief_section
                                                                {
                                                                    None | Some(0) => Some(
                                                                        sec_count.saturating_sub(1),
                                                                    ),
                                                                    Some(i) => Some(i - 1),
                                                                };
                                                                this.brief_section_epoch = this
                                                                    .brief_section_epoch
                                                                    .wrapping_add(1);
                                                                cx.notify();
                                                            },
                                                        ))
                                                        .child("‹"),
                                                )
                                                .child(
                                                    div()
                                                        .id("rsec-next")
                                                        .w(px(22.0))
                                                        .h(px(22.0))
                                                        .rounded(theme::r_xs())
                                                        .hover(|s| {
                                                            s.bg(theme::row_hover())
                                                                .text_color(theme::text_primary())
                                                        })
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .cursor_pointer()
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.brief_section = match this
                                                                    .brief_section
                                                                {
                                                                    None => Some(0),
                                                                    Some(i)
                                                                        if i + 1 < sec_count =>
                                                                    {
                                                                        Some(i + 1)
                                                                    }
                                                                    Some(_) => None,
                                                                };
                                                                this.brief_section_epoch = this
                                                                    .brief_section_epoch
                                                                    .wrapping_add(1);
                                                                cx.notify();
                                                            },
                                                        ))
                                                        .child("›"),
                                                ),
                                        ),
                                );

                            div()
                                .flex_1()
                                .min_h_0()
                                .min_w_0()
                                .flex()
                                .gap_6()
                                .child(chapter_rail)
                                .child(animated_body)
                                .into_any_element()
                        }
                    });

                let scrim = div()
                    .id("md-reader-backdrop")
                    .occlude()
                    .on_scroll_wheel(|_, _, cx| {
                        cx.stop_propagation();
                    })
                    .absolute()
                    .inset_0()
                    .bg(gpui::rgba(0x06060bcc))
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_6()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.expanded_reader = None;
                            cx.notify();
                        }),
                    )
                    .child(if self.reduce_motion {
                        modal_card.into_any_element()
                    } else {
                        modal_card
                            .with_animation(
                                "reader-pop",
                                Animation::new(Duration::from_millis(160))
                                    .with_easing(ease_out_quint()),
                                |el, t| el.opacity(t).mt(px(10.0 * (1.0 - t))),
                            )
                            .into_any_element()
                    });
                d.child(if self.reduce_motion {
                    scrim.into_any_element()
                } else {
                    scrim
                        .with_animation(
                            "reader-scrim",
                            Animation::new(Duration::from_millis(140))
                                .with_easing(ease_out_quint()),
                            |el, t| el.opacity(t),
                        )
                        .into_any_element()
                })
            })
            .when(self.needs_first_run, |d| {
                d.child(
                    div()
                        .id("first-run-launchpad")
                        .occlude()
                        .absolute()
                        .inset_0()
                        .bg(theme::window_glass())
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w(px(440.0))
                                .p_6()
                                .rounded(theme::r_lg())
                                .bg(theme::panel_glass())
                                .border_1()
                                .border_color(theme::hairline_strong())
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(6.0))
                                        .child(
                                            div()
                                                .text_base()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme::text_primary())
                                                .child("Welcome to Job Harness"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .line_height(px(19.0))
                                                .text_color(theme::text_secondary())
                                                .child("Keep applications, submitted CVs, and interview briefs in one private folder on your Mac — usable by any AI agent."),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id("first-run-sample")
                                                .w_full()
                                                .px_4()
                                                .py(px(10.0))
                                                .rounded(theme::r_md())
                                                .bg(theme::white_solid())
                                                .hover(|s| s.opacity(0.92))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.launch_sample_workspace(cx);
                                                }))
                                                .child(
                                                    div()
                                                        .flex()
                                                        .flex_col()
                                                        .gap(px(1.0))
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .font_weight(FontWeight::SEMIBOLD)
                                                                .text_color(theme::ink_dark())
                                                                .child("Try with sample data"),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(11.0))
                                                                .text_color(gpui::rgba(0x3f3f46ff))
                                                                .child("Explore 10 realistic roles, submitted CVs & interview briefs"),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(theme::ink_dark())
                                                        .child("→"),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("first-run-create")
                                                .w_full()
                                                .px_4()
                                                .py(px(10.0))
                                                .rounded(theme::r_md())
                                                .bg(theme::surface_subtle())
                                                .border_1()
                                                .border_color(theme::hairline())
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.create_workspace_dialog(cx);
                                                }))
                                                .child(
                                                    div()
                                                        .flex()
                                                        .flex_col()
                                                        .gap(px(1.0))
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .font_weight(FontWeight::MEDIUM)
                                                                .text_color(theme::text_primary())
                                                                .child("Create my workspace"),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(11.0))
                                                                .text_color(theme::text_muted())
                                                                .child("Start a fresh private folder on disk with guided setup"),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme::text_muted())
                                                        .child("→"),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("first-run-open")
                                                .w_full()
                                                .px_4()
                                                .py(px(8.0))
                                                .rounded(theme::r_md())
                                                .hover(|s| s.bg(theme::row_hover()))
                                                .cursor_pointer()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.open_workspace_dialog(cx);
                                                }))
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme::text_secondary())
                                                        .child("Open existing workspace folder…"),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(theme::text_muted())
                                                        .child("↗"),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .pt_2()
                                        .border_t_1()
                                        .border_color(theme::hairline())
                                        .text_size(px(11.0))
                                        .text_color(theme::text_muted())
                                        .child("100% local Markdown & SQLite. Nothing leaves your machine unless you connect an agent."),
                                ),
                        ),
                )
            })
            .when_some(self.render_popover(window.viewport_size(), cx), |d, pop| {
                d.child(pop)
            })
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

/// Truncates human-readable text at the end (`Prefix…`) so the beginning is never chopped off.
fn truncate_end(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let head: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

/// Truncates file names in the middle (`cambridge-ap…41638.md`) so both slug and extension stay legible.
fn numeric_scrub_spec(key: &str) -> Option<(i64, i64, i64)> {
    match key {
        "prep_reminder_minutes" => Some((5, 5, 1440)),
        "applied_ghost_days" => Some((1, 1, 365)),
        "min_base_salary" | "target_base_salary" => Some((1000, 0, 500_000)),
        _ => None,
    }
}

fn snap_width(raw: f32, stops: &[f32], threshold: f32) -> f32 {
    for &s in stops {
        if (raw - s).abs() <= threshold {
            return s;
        }
    }
    raw
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars || max_chars < 6 {
        return text.to_owned();
    }
    let keep_end = (max_chars / 3).clamp(4, 10);
    let keep_start = max_chars.saturating_sub(keep_end + 1);
    let head: String = text.chars().take(keep_start).collect();
    let tail: String = text.chars().skip(count.saturating_sub(keep_end)).collect();
    format!("{head}…{tail}")
}

struct BriefSection {
    label: String,
    body: String,
}

/// Splits a brief into clean navigable layers/parts:
/// - If 2+ top-level `# ` layers exist (e.g. Company Brief Layer 1 / Round Brief Layer 2 / Debrief),
///   slices by `# ` so global company context stays cleanly separated from stage briefs.
/// - Otherwise if 2+ `## ` sections exist, slices by `## ` for clean step-through reading.
fn parse_brief_sections(text: &str) -> Vec<BriefSection> {
    let h1_count = text
        .lines()
        .filter(|l| l.trim_start().starts_with("# ") && !l.trim_start().starts_with("## "))
        .count();
    let split_prefix = if h1_count >= 2 { "# " } else { "## " };

    let mut sections: Vec<BriefSection> = Vec::new();
    let mut current_label: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim_start();
        let is_split = if split_prefix == "# " {
            trimmed.starts_with("# ") && !trimmed.starts_with("## ")
        } else {
            trimmed.starts_with("## ") && !trimmed.starts_with("### ")
        };
        if is_split {
            if let Some(label) = current_label.take() {
                sections.push(BriefSection {
                    label,
                    body: current_lines.join("\n"),
                });
            }
            let raw_title = trimmed.trim_start_matches(split_prefix).trim();
            current_label = Some(short_section_pill(raw_title));
            current_lines = vec![line];
        } else if current_label.is_some() {
            current_lines.push(line);
        }
    }
    if let Some(label) = current_label {
        sections.push(BriefSection {
            label,
            body: current_lines.join("\n"),
        });
    }
    // Fold ad-hoc `## ` headings (> 4) into at most 4 predictable canonical buckets
    if split_prefix == "## " && sections.len() > 4 {
        let mut buckets: [(&'static str, Vec<String>); 4] = [
            ("Summary & fit", Vec::new()),
            ("Prep & stories", Vec::new()),
            ("Questions & risks", Vec::new()),
            ("Timeline", Vec::new()),
        ];
        for sec in sections {
            let low = sec.label.to_ascii_lowercase();
            let ix = if low.contains("update")
                || low.contains("log")
                || low.contains("timeline")
                || low.contains("history")
                || low.contains("debrief")
                || low.contains("email")
                || low.contains("attempt")
                || low.contains("overnight")
                || low.contains("note")
            {
                3
            } else if low.contains("question")
                || low.contains("ask")
                || low.contains("risk")
                || low.contains("gap")
                || low.contains("concern")
                || low.contains("objection")
                || low.contains("flag")
            {
                2
            } else if low.contains("prep")
                || low.contains("stor")
                || low.contains("opening")
                || low.contains("pitch")
                || low.contains("demo")
                || low.contains("angle")
                || low.contains("talk")
                || low.contains("hook")
                || low.contains("interviewer")
                || low.contains("architect")
            {
                1
            } else {
                0
            };
            buckets[ix].1.push(sec.body);
        }
        return buckets
            .into_iter()
            .filter(|(_, parts)| !parts.is_empty())
            .map(|(label, parts)| BriefSection {
                label: label.to_owned(),
                body: parts.join("\n\n"),
            })
            .collect();
    }
    sections
}

/// Extracts an explicit quoted confirmation phrase from an assistant message (e.g. `say "yes, clean it up"`).
fn extract_suggested_reply_phrase(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    for trigger in ["say ", "reply ", "type ", "respond with "] {
        if let Some(pos) = lower.rfind(trigger) {
            let rest = text[pos + trigger.len()..].trim_start();
            let (open, close) = if rest.starts_with('“') {
                ('“', '”')
            } else if rest.starts_with('"') {
                ('"', '"')
            } else if rest.starts_with('`') {
                ('`', '`')
            } else {
                continue;
            };
            let after_open = rest.strip_prefix(open)?;
            if let Some(end_ix) = after_open.find(close) {
                let phrase = after_open[..end_ix].trim();
                if !phrase.is_empty() && phrase.len() <= 72 && !phrase.contains('\n') {
                    return Some(phrase.to_owned());
                }
            }
        }
    }
    None
}

fn short_section_pill(heading: &str) -> String {
    if let Some((left, right)) = heading.split_once('—') {
        let l = left.trim();
        let r = right.trim();
        if l.eq_ignore_ascii_case("Recorded update") && !r.is_empty() {
            return format!("Update {}", truncate_end(r, 12));
        }
    }
    let head = heading
        .split('—')
        .next()
        .unwrap_or(heading)
        .split('(')
        .next()
        .unwrap_or(heading)
        .trim();
    truncate_end(head, 18)
}

fn format_tokens(total: u64) -> String {
    if total >= 1_000 {
        format!("{:.1}k tok", total as f64 / 1_000.0)
    } else {
        format!("{total} tok")
    }
}

/// Standardized DRY clip-rect column slide (Zeron motion pattern: fixed inner width,
/// animating outer clip box so zero text re-wraps mid-flight).
fn clip_column_slide(
    inner: impl IntoElement + 'static,
    name: &'static str,
    width: f32,
    gap: f32,
    hidden: bool,
    animated: bool,
    epoch: u64,
) -> AnyElement {
    if !animated {
        if hidden {
            return div().w(px(0.0)).into_any_element();
        }
        return div()
            .mr(px(gap))
            .flex_shrink_0()
            .h_full()
            .child(inner)
            .into_any_element();
    }
    if hidden {
        div()
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .child(inner)
            .with_animation(
                SharedString::from(format!("col-hide-{name}-{epoch}")),
                Animation::new(Duration::from_millis(200)).with_easing(ease_out_quint()),
                move |el, t| {
                    let inv = 1.0 - t;
                    el.w(px(width * inv)).mr(px(gap * inv)).opacity(inv)
                },
            )
            .into_any_element()
    } else {
        div()
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .child(inner)
            .with_animation(
                SharedString::from(format!("col-show-{name}-{epoch}")),
                Animation::new(Duration::from_millis(200)).with_easing(ease_out_quint()),
                move |el, t| el.w(px(width * t)).mr(px(gap * t)).opacity(t),
            )
            .into_any_element()
    }
}

fn title_case_stage(stage: &str) -> String {
    match stage {
        "saved" => "Saved".into(),
        "prepared" => "Prepared".into(),
        "applied" => "Applied".into(),
        "introduction" => "Introduction".into(),
        "screening" => "Screening".into(),
        "technical" => "Technical".into(),
        "final" => "Final".into(),
        "offer" => "Offer".into(),
        "closed" => "Archived".into(),
        "needs-review" => "Needs review".into(),
        other => other.to_owned(),
    }
}

fn popover_shell(bottom: bool, width: f32, right_offset: f32) -> gpui::Div {
    let base = div()
        .absolute()
        .right(px(right_offset))
        .w(px(width))
        .p_3()
        .rounded(theme::r_lg())
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
                                .rounded(theme::r_xs())
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
                        .rounded(theme::r_xs())
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
                        .rounded(theme::r_xs())
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

fn tool_step_icon(name: &str, detail: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    let d = detail.to_ascii_lowercase();
    if n.contains("think") || n.contains("reason") {
        "icons/brain.svg"
    } else if n.contains("read")
        || n.contains("view")
        || n.contains("bootstrap")
        || n.contains("inspect")
        || d.contains("query_batch")
    {
        "icons/eye.svg"
    } else if n.contains("edit")
        || n.contains("write")
        || n.contains("patch")
        || n.contains("update")
        || n.contains("move")
        || d.contains("execute_batch")
    {
        "icons/edit.svg"
    } else if n.contains("search") || n.contains("grep") || n.contains("find") || n.contains("web")
    {
        "icons/search.svg"
    } else {
        "icons/terminal.svg"
    }
}

fn crisp_caret(id_tag: &'static str, height: f32, reduce_motion: bool) -> AnyElement {
    let bar = div()
        .w(px(1.5))
        .h(px(height))
        .rounded(px(0.5))
        .bg(theme::text_primary());
    if reduce_motion {
        bar.into_any_element()
    } else {
        bar.with_animation(
            SharedString::from(format!("caret-blink-{id_tag}")),
            Animation::new(Duration::from_millis(1060)).repeat(),
            |el, t| el.opacity(if t < 0.55 { 1.0 } else { 0.0 }),
        )
        .into_any_element()
    }
}

/// Left-aligned wrapping inline editor for long profile fields (Target roles, Location, Avoid list, emails).
#[allow(dead_code)]
fn inline_editable_text_left(
    text: &str,
    placeholder: &str,
    all_selected: bool,
    reduce_motion: bool,
    id_tag: &'static str,
) -> impl IntoElement {
    let has_text = !text.is_empty();
    if !has_text {
        return div()
            .relative()
            .w_full()
            .flex()
            .items_center()
            .text_xs()
            .line_height(px(18.0))
            .child(crisp_caret(id_tag, 14.0, reduce_motion))
            .child(
                div()
                    .pl(px(3.0))
                    .text_color(theme::text_muted())
                    .child(placeholder.to_owned()),
            )
            .into_any_element();
    }
    div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .flex_wrap()
        .text_xs()
        .line_height(px(18.0))
        .child(
            div()
                .min_w_0()
                .when(all_selected, |d| {
                    d.px(px(2.0))
                        .rounded(theme::r_xs())
                        .bg(theme::lavender_wash())
                })
                .text_color(theme::text_primary())
                .child(text.to_owned()),
        )
        .when(!all_selected, |d| {
            d.child(
                div()
                    .ml(px(1.0))
                    .child(crisp_caret(id_tag, 14.0, reduce_motion)),
            )
        })
        .into_any_element()
}

/// Seamless borderless wrapping inline text editor (zero chrome, expands smoothly to show full value).
fn inline_editable_text(
    text: &str,
    placeholder: &str,
    all_selected: bool,
    reduce_motion: bool,
    id_tag: &'static str,
) -> impl IntoElement {
    let has_text = !text.is_empty();
    if !has_text {
        return div()
            .relative()
            .w_full()
            .flex()
            .items_center()
            .justify_end()
            .text_xs()
            .line_height(px(18.0))
            .child(crisp_caret(id_tag, 14.0, reduce_motion))
            .child(
                div()
                    .pl(px(2.0))
                    .text_color(theme::text_muted())
                    .child(placeholder.to_owned()),
            )
            .into_any_element();
    }
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_end()
        .flex_wrap()
        .text_xs()
        .line_height(px(18.0))
        .child(
            div()
                .when(all_selected, |d| {
                    d.px(px(2.0))
                        .rounded(theme::r_xs())
                        .bg(theme::lavender_wash())
                })
                .text_color(theme::text_primary())
                .child(text.to_owned()),
        )
        .when(!all_selected, |d| {
            d.child(
                div()
                    .ml(px(1.0))
                    .child(crisp_caret(id_tag, 14.0, reduce_motion)),
            )
        })
        .into_any_element()
}

/// Multi-line auto-growing Chat composer text with crisp T3 Code white caret & ⌘A highlight.
fn chat_multiline_input(
    text: &str,
    placeholder: &str,
    active: bool,
    all_selected: bool,
    reduce_motion: bool,
) -> impl IntoElement {
    let has_text = !text.is_empty();
    if !has_text {
        return div()
            .relative()
            .w_full()
            .flex()
            .items_center()
            .text_size(px(12.5))
            .line_height(px(19.0))
            .when(active, |d| {
                d.child(div().absolute().left_0().top(px(2.0)).child(crisp_caret(
                    "chat-empty",
                    15.5,
                    reduce_motion,
                )))
            })
            .child(
                div()
                    .pl(px(3.0))
                    .text_color(theme::text_muted())
                    .child(placeholder.to_owned()),
            )
            .into_any_element();
    }

    let lines: Vec<&str> = text.split('\n').collect();
    let last_ix = lines.len().saturating_sub(1);
    div()
        .w_full()
        .flex()
        .flex_col()
        .text_size(px(12.5))
        .line_height(px(19.0))
        .children(lines.into_iter().enumerate().map(move |(i, line)| {
            let is_last = i == last_ix;
            div()
                .w_full()
                .flex()
                .items_center()
                .flex_wrap()
                .child(
                    div()
                        .when(all_selected, |d| {
                            d.rounded(theme::r_xs()).bg(theme::lavender_wash())
                        })
                        .text_color(theme::text_primary())
                        .child(if line.is_empty() {
                            " ".to_owned()
                        } else {
                            line.to_owned()
                        }),
                )
                .when(active && is_last && !all_selected, |d| {
                    d.child(
                        div()
                            .ml(px(1.0))
                            .child(crisp_caret("chat-end", 15.5, reduce_motion)),
                    )
                })
        }))
        .into_any_element()
}

/// Crisp T3 Code single-line input with visible bright white blinking caret (even when empty!)
/// and ZERO placeholder shift on focus.
fn input_line(
    text: &str,
    placeholder: &str,
    active: bool,
    reduce_motion: bool,
    id_tag: &'static str,
    max_tail_chars: usize,
) -> impl IntoElement {
    let has_text = !text.is_empty();
    let visible_text = if has_text {
        let count = text.chars().count();
        if count > max_tail_chars {
            let tail: String = text.chars().skip(count - max_tail_chars).collect();
            format!("…{tail}")
        } else {
            text.to_owned()
        }
    } else {
        placeholder.to_owned()
    };

    div()
        .relative()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .when(!has_text && active, |d| {
            d.child(div().absolute().left_0().top(px(1.0)).child(crisp_caret(
                id_tag,
                14.5,
                reduce_motion,
            )))
        })
        .child(
            div()
                .when(!has_text, |d| d.pl(px(3.0)))
                .text_xs()
                .text_color(if has_text {
                    theme::text_primary()
                } else {
                    theme::text_muted()
                })
                .child(visible_text),
        )
        .when(has_text && active, |d| {
            d.child(
                div()
                    .ml(px(1.0))
                    .child(crisp_caret(id_tag, 14.5, reduce_motion)),
            )
        })
}

/// Shared right-drawer header with no double-line clutter.
fn drawer_header(
    title: &str,
    subtitle: Option<String>,
    cx: &mut Context<JobHarnessApp>,
) -> impl IntoElement {
    div()
        .pb_2()
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
                        .text_sm()
                        .text_color(theme::text_primary())
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("close-drawer-{title}")))
                        .px(px(5.0))
                        .py(px(2.0))
                        .rounded(theme::r_xs())
                        .hover(|s| s.bg(theme::row_hover()))
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
        .when_some(subtitle, |d, sub| {
            d.child(div().text_xs().text_color(theme::text_muted()).child(sub))
        })
}

fn cmd_keycap(label: &'static str) -> impl IntoElement {
    div()
        .px(px(5.0))
        .py(px(1.5))
        .rounded(theme::r_xs())
        .bg(theme::surface_subtle())
        .border_1()
        .border_color(theme::hairline())
        .text_size(px(10.5))
        .text_color(theme::text_secondary())
        .child(label)
}
