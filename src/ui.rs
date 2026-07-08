use ansi_to_tui::IntoText;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{App, Mode},
    manifest::Safety,
    model::{BranchSync, BranchTab, DiffMode, Focus},
};

const LOG_HEIGHT: u16 = 8;

const ACTIVE: Color = Color::Cyan;
const SELECTED: Color = Color::Blue;

/// Map a `BranchSync` variant to its display glyph and colour.
fn sync_glyph(sync: BranchSync) -> (&'static str, Color) {
    match sync {
        BranchSync::InSync => ("✓", Color::Green),
        BranchSync::Ahead => ("↑", Color::Yellow),
        BranchSync::Behind => ("↓", Color::Yellow),
        BranchSync::Diverged => ("↕", Color::Magenta),
        BranchSync::Differs => ("≠", Color::Yellow),
        BranchSync::Untracked => ("", Color::Reset),
    }
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);
    render_header(frame, rows[0], app);
    if area.width < 90 {
        render_narrow(frame, rows[1], app);
    } else {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(36), Constraint::Percentage(64)])
            .split(rows[1]);
        render_sidebar(frame, columns[0], app);
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(LOG_HEIGHT)])
            .split(columns[1]);
        render_main(frame, right[0], app);
        render_command_log(frame, right[1], app);
    }
    render_footer(frame, rows[2], app);
    if app.state.repository_error.is_some() && matches!(app.mode, Mode::Normal) {
        render_onboarding(frame, area, app);
    }
    match &app.mode {
        Mode::Help => render_help(frame, area),
        Mode::Palette { query, selected } => render_palette(frame, area, app, query, *selected),
        Mode::Prompt {
            title,
            value,
            secret,
            ..
        } => render_prompt(frame, area, title, value, *secret),
        Mode::Confirm {
            title,
            message,
            required,
            typed,
            ..
        } => render_confirm(frame, area, title, message, required.as_deref(), typed),
        Mode::Normal => {}
    }
}

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Standalone loading screen shown while the app performs its initial,
/// potentially slow, server-touching refresh. Deliberately takes no `&App` so
/// it can be redrawn on a timer while `app` is mutably borrowed elsewhere.
pub fn render_loading(frame: &mut Frame, banner: &str, frame_idx: usize, show_offline_hint: bool) {
    let area = frame.area();
    let mut lines: Vec<Line> = banner
        .as_bytes()
        .into_text()
        .unwrap_or_else(|_| Text::raw(banner))
        .lines;
    // Drop trailing blank lines emitted by tui-banner so the spinner sits close.
    while lines.last().is_some_and(|l| {
        l.spans
            .iter()
            .all(|s| s.content.chars().all(char::is_whitespace))
    }) {
        lines.pop();
    }
    lines.push(Line::from(""));
    let spinner = SPINNER_FRAMES[frame_idx % SPINNER_FRAMES.len()];
    lines.push(Line::from(Span::styled(
        format!("{spinner}  Connecting to Lore…"),
        Style::default().fg(ACTIVE).add_modifier(Modifier::BOLD),
    )));
    if show_offline_hint {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Server slow to respond — starting in offline mode…",
            Style::default().fg(Color::DarkGray),
        )));
    }
    let rect = centered(area, 64, 60);
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center).block(
            Block::default()
                .title(" LazyLore ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACTIVE)),
        ),
        rect,
    );
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let repo = &app.state.repository;
    let (glyph, _) = match (repo.local_ahead, repo.remote_ahead) {
        (true, true) => sync_glyph(BranchSync::Diverged),
        (true, false) => sync_glyph(BranchSync::Ahead),
        (false, true) => sync_glyph(BranchSync::Behind),
        _ => ("", Color::Reset),
    };
    let sync = if glyph.is_empty() {
        String::new()
    } else {
        format!("  {glyph}")
    };
    let stale = if repo.stale { "  [unscanned]" } else { "" };
    let busy = app
        .state
        .progress
        .as_deref()
        .map(|text| format!("  ◌ {text}"))
        .unwrap_or_default();
    let text = format!(
        " LazyLore {}  {} @{}{}{}{}",
        app.lore_version,
        empty_as(&repo.branch, "no repository"),
        repo.revision_number,
        sync,
        stale,
        busy
    );
    frame.render_widget(
        Paragraph::new(text).style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let keys = match app.focus {
        Focus::Files => {
            "2 Files  Space stage  a all  c commit  d discard  r scan  ? help  Ctrl+P commands"
        }
        Focus::Branches => {
            "3 Branches  Space switch  n new  d archive  M merge  Enter history  [ ] tabs"
        }
        Focus::Revisions if app.revision_view.is_some() => {
            "4 Revision files  ↑↓ move  Space/Enter expand  Esc back  Enter on file → Main"
        }
        Focus::Revisions => {
            "4 Revisions  Space sync  C/V cherry-pick  t revert  g reset  Enter files"
        }
        Focus::Locks => "5 Locks  Space acquire/release  r refresh",
        Focus::Main => "0 Main  Tab/←/→ cycle panes  Ctrl+Tab diff mode  PgUp/PgDn scroll",
        Focus::Repository => "1 Repository  Tab/←/→ cycle panes  p sync  P push  R refresh",
        Focus::CommandLog => "Command Log  ↑↓ move entry  ; page up  . page down  Esc back",
    };
    frame.render_widget(
        Paragraph::new(keys).style(Style::default().fg(Color::Black).bg(Color::DarkGray)),
        area,
    );
}

fn render_sidebar(frame: &mut Frame, area: Rect, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(7),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(6),
        ])
        .split(area);
    render_repository(frame, chunks[0], app);
    render_files(frame, chunks[1], app);
    render_branches(frame, chunks[2], app);
    render_revisions(frame, chunks[3], app);
    render_locks(frame, chunks[4], app);
}

fn render_narrow(frame: &mut Frame, area: Rect, app: &mut App) {
    match app.focus {
        Focus::Repository => render_repository(frame, area, app),
        Focus::Files => render_files(frame, area, app),
        Focus::Branches => render_branches(frame, area, app),
        Focus::Revisions => render_revisions(frame, area, app),
        Focus::Locks => render_locks(frame, area, app),
        Focus::Main => render_main(frame, area, app),
        Focus::CommandLog => render_command_log(frame, area, app),
    }
}

fn panel(title: &str, focused: bool) -> Block<'_> {
    let title = if focused {
        format!(" ▶ {title} ")
    } else {
        format!(" {title} ")
    };
    let title_style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(ACTIVE)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .title(Line::from(Span::styled(title, title_style)))
        .borders(Borders::ALL)
        .border_type(if focused {
            BorderType::Double
        } else {
            BorderType::Plain
        })
        .border_style(
            Style::default()
                .fg(if focused { ACTIVE } else { Color::DarkGray })
                .add_modifier(if focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )
}

fn render_repository(frame: &mut Frame, area: Rect, app: &App) {
    let repo = &app.state.repository;
    let focused = app.focus == Focus::Repository;

    // Line 1: repo directory name (bold).
    let dir_name = repo
        .root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| {
            if repo.repository.is_empty() {
                "No repository"
            } else {
                &repo.repository
            }
        });
    let line1 = Line::from(Span::styled(
        dir_name.to_owned(),
        Style::default().add_modifier(Modifier::BOLD),
    ));

    // Line 2: colored branch name + sync glyph.
    let branch_text = empty_as(&repo.branch, "no branch").to_owned();
    let (glyph, glyph_color) = match (repo.local_ahead, repo.remote_ahead) {
        (true, true) => sync_glyph(BranchSync::Diverged),
        (true, false) => sync_glyph(BranchSync::Ahead),
        (false, true) => sync_glyph(BranchSync::Behind),
        _ => sync_glyph(BranchSync::InSync),
    };
    let mut line2_spans = vec![Span::styled(
        branch_text,
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )];
    if !glyph.is_empty() {
        line2_spans.push(Span::raw(" "));
        line2_spans.push(Span::styled(glyph, Style::default().fg(glyph_color)));
    }
    let line2 = Line::from(line2_spans);

    // Line 3: @revision + colored remote state.
    let (remote_text, remote_color) = if app.offline_forced {
        ("offline (forced)", Color::DarkGray)
    } else if !repo.remote_available {
        ("offline", Color::Red)
    } else if !repo.remote_authorized {
        ("unauthorized (L to log in)", Color::Yellow)
    } else {
        ("connected", Color::Green)
    };
    let line3 = Line::from(vec![
        Span::styled(
            format!("@{}  ", repo.revision_number),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(remote_text, Style::default().fg(remote_color)),
    ]);

    frame.render_widget(
        Paragraph::new(vec![line1, line2, line3]).block(panel("1 Repository", focused)),
        area,
    );
}

fn render_files(frame: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .state
        .files
        .iter()
        .map(|file| {
            let staged = if file.staged { "S" } else { " " };
            let conflict = if file.unresolved { "!" } else { " " };
            let action = short_action(&file.action);
            let style = if file.unresolved {
                Style::default().fg(Color::Red)
            } else if file.staged {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Yellow)
            };
            ListItem::new(format!("{staged}{conflict} {action} {}", file.path)).style(style)
        })
        .collect();
    let items = if items.is_empty() {
        vec![ListItem::new("Working tree clean")]
    } else {
        items
    };
    render_list(
        frame,
        area,
        items,
        app.file_selected,
        app.focus == Focus::Files,
        panel("2 Files", app.focus == Focus::Files),
    );
}

fn render_branches(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Branches;

    // Count branches per tab for the tab strip labels.
    let local_count = app
        .state
        .branches
        .iter()
        .filter(|b| BranchTab::Local.matches(b))
        .count();
    let remote_count = app
        .state
        .branches
        .iter()
        .filter(|b| BranchTab::Remote.matches(b))
        .count();

    // Collect the visible (filtered) list first so the borrow ends before render_list.
    let visible: Vec<_> = app.visible_branches().into_iter().cloned().collect();
    // Pre-compute sync states (borrows app.state.branches, safe because visible is already cloned).
    let syncs: Vec<_> = visible.iter().map(|b| app.branch_sync(b)).collect();
    let items: Vec<ListItem> = visible
        .iter()
        .zip(syncs.iter())
        .map(|(branch, &sync)| {
            let marker = if branch.current { "*" } else { " " };
            let archived = if branch.archived { " [archived]" } else { "" };
            let name_style = if branch.current {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let (glyph, glyph_color) = sync_glyph(sync);
            let mut spans = vec![
                Span::raw(format!("{marker} ")),
                Span::styled(format!("{}{archived}", branch.name), name_style),
            ];
            if !glyph.is_empty() {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(glyph, Style::default().fg(glyph_color)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    // Build the tab strip as a right-side title.
    let active_fg = if focused {
        Color::Black
    } else {
        Color::DarkGray
    };
    let active_bg = if focused { ACTIVE } else { Color::Reset };
    let active_tab_style = Style::default()
        .fg(active_fg)
        .bg(active_bg)
        .add_modifier(Modifier::BOLD);
    let inactive_tab_style = Style::default().fg(Color::DarkGray);
    let (local_style, remote_style) = if app.branch_tab == BranchTab::Local {
        (active_tab_style, inactive_tab_style)
    } else {
        (inactive_tab_style, active_tab_style)
    };
    let tab_line = Line::from(vec![
        Span::styled(format!(" Local {local_count} "), local_style),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(format!(" Remote {remote_count} "), remote_style),
    ]);

    let block = panel("3 Branches", focused).title_bottom(tab_line);

    render_list(
        frame,
        area,
        empty_list(items, "No branches"),
        app.branch_selected,
        focused,
        block,
    );
}

fn render_revisions(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Revisions;

    if let Some(view) = &app.revision_view {
        // Tree drill-down mode
        let visible = view.visible();
        let items: Vec<ListItem> = visible
            .iter()
            .map(|entry| {
                let indent = "  ".repeat(entry.depth);
                if entry.is_dir {
                    let arrow = if view.collapsed.contains(&entry.path) {
                        "▶"
                    } else {
                        "▼"
                    };
                    ListItem::new(format!("{}{} {}", indent, arrow, entry.label)).style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    let marker = entry.marker.unwrap_or(' ');
                    let color = match marker {
                        'A' => Color::Green,
                        'D' => Color::Red,
                        'M' => Color::Yellow,
                        _ => Color::White,
                    };
                    ListItem::new(format!("{}{} {}", indent, marker, entry.label))
                        .style(Style::default().fg(color))
                }
            })
            .collect();
        let title = format!("4 Revision @{}", view.number);
        render_list(
            frame,
            area,
            empty_list(items, "No changed files"),
            view.selected,
            focused,
            panel(&title, focused),
        );
        return;
    }

    // Revision list mode (unchanged)
    let items: Vec<ListItem> = app
        .state
        .revisions
        .iter()
        .map(|revision| {
            let message = if revision.message.is_empty() {
                "(no message)"
            } else {
                &revision.message
            };
            ListItem::new(format!(
                "@{} {} {}",
                revision.number,
                short_hash(&revision.hash),
                message
            ))
        })
        .collect();
    render_list(
        frame,
        area,
        empty_list(items, "No revisions"),
        app.revision_selected,
        focused,
        panel("4 Revisions", focused),
    );
}

fn render_locks(frame: &mut Frame, area: Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .state
        .locks
        .iter()
        .map(|lock| ListItem::new(format!("🔒 {}  {}", lock.path, lock.owner)))
        .collect();
    render_list(
        frame,
        area,
        empty_list(items, "No locks"),
        app.lock_selected,
        app.focus == Focus::Locks,
        panel("5 Locks", app.focus == Focus::Locks),
    );
}

fn render_list(
    frame: &mut Frame,
    area: Rect,
    items: Vec<ListItem>,
    selected: usize,
    focused: bool,
    block: Block,
) {
    let mut state =
        ListState::default().with_selected(Some(selected.min(items.len().saturating_sub(1))));
    let highlight_style = if focused {
        Style::default()
            .bg(SELECTED)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style)
        .highlight_symbol(if focused { "▶ " } else { "  " });
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_main(frame: &mut Frame, area: Rect, app: &App) {
    let in_revision_readout = app.focus == Focus::Revisions && app.revision_view.is_none();
    let in_revision_tree = app.focus == Focus::Revisions && app.revision_view.is_some();
    let title = if in_revision_readout {
        "0 Main — Revision"
    } else if in_revision_tree {
        "0 Main — Revision diff"
    } else {
        match app.diff_mode {
            DiffMode::Working => "0 Main — Working",
            DiffMode::Staged => "0 Main — Staged",
            DiffMode::Unstaged => "0 Main — Unstaged",
        }
    };

    // Build prefix lines for the Repository focus: tui-banner logo + copyright + separator.
    let logo_lines: Vec<Line> = if app.focus == Focus::Repository {
        let mut v: Vec<Line> = app
            .banner
            .as_bytes()
            .into_text()
            .unwrap_or_else(|_| Text::raw(&app.banner))
            .lines;
        // Drop trailing blank lines emitted by tui-banner so the copyright sits close.
        while v.last().is_some_and(|l| {
            l.spans
                .iter()
                .all(|s| s.content.chars().all(char::is_whitespace))
        }) {
            v.pop();
        }
        // One blank line of top margin before the banner.
        v.insert(0, Line::from(""));
        v.push(Line::from(Span::styled(
            "Copyright 2026 Peralysis",
            Style::default().fg(Color::DarkGray),
        )));
        v.push(Line::from(""));
        v
    } else {
        vec![]
    };

    let lines: Vec<Line> = app
        .state
        .preview
        .iter()
        .map(|line| {
            let style = if in_revision_readout {
                // Readout: color the A / M / D marker lines.
                if line.starts_with("A ") {
                    Style::default().fg(Color::Green)
                } else if line.starts_with("D ") {
                    Style::default().fg(Color::Red)
                } else if line.starts_with("M ") {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                }
            } else if line.starts_with('+') && !line.starts_with("+++") {
                Style::default().fg(Color::Green)
            } else if line.starts_with('-') && !line.starts_with("---") {
                Style::default().fg(Color::Red)
            } else if line.starts_with("@@") {
                Style::default().fg(Color::Cyan)
            } else if line.starts_with("error") || line.starts_with('!') {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            Line::from(Span::styled(line.clone(), style))
        })
        .collect();
    let all_lines: Vec<Line> = logo_lines.into_iter().chain(lines).collect();
    frame.render_widget(
        Paragraph::new(all_lines)
            .scroll((app.main_scroll, 0))
            .wrap(Wrap { trim: false })
            .block(panel(title, app.focus == Focus::Main)),
        area,
    );
}

fn render_help(frame: &mut Frame, area: Rect) {
    let help = vec![
        Line::from(Span::styled(
            "Global",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("Tab / Right  Next pane             Left / Shift+Tab Previous pane"),
        Line::from("1–5 / 0      Focus pane directly   j/k or Up/Down   Move selection"),
        Line::from("p / P      Sync / push            R              Refresh tracked state"),
        Line::from("O          Toggle offline mode    ?              Help"),
        Line::from("@          Focus command log      :              Shell command"),
        Line::from("Ctrl+P     All Lore commands      q / Ctrl+C     Quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Lore notes",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("Ctrl+Tab cycles the Main pane's working/staged/unstaged diff mode."),
        Line::from("Lore stages whole files; line/hunk staging is unavailable."),
        Line::from("Lore has no stash, rebase, tags, worktrees, or Git-style remotes."),
        Line::from("The [unscanned] marker means changes made before startup may need Files → r."),
        Line::from("Destructive commands require confirmation; typed confirmation is `confirm`."),
        Line::from("Offline mode: when the server is unreachable, sync/push/lock commands are"),
        Line::from("  skipped automatically. Background probes restore connection (~30 s)."),
        Line::from(
            "  `O` toggles forced-offline mode (disables probes); `--offline` flag at start.",
        ),
        Line::from(""),
        Line::from("Esc or ? closes this window."),
    ];
    modal(
        frame,
        area,
        78,
        22,
        " Keybindings ",
        Paragraph::new(help).wrap(Wrap { trim: false }),
    );
}

fn render_command_log(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::CommandLog;
    let items: Vec<ListItem> = app
        .state
        .command_history
        .iter()
        .map(|record| {
            let marker = if record.success { "✓" } else { "✗" };
            let color = if record.success {
                Color::Green
            } else {
                Color::Red
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(color)),
                Span::raw(format!("{} ({:.2?})", record.display, record.duration)),
            ]))
        })
        .collect();
    render_list(
        frame,
        area,
        empty_list(items, "No commands yet"),
        app.command_log_selected,
        focused,
        panel("Command Log", focused),
    );
}

fn render_palette(frame: &mut Frame, area: Rect, app: &App, query: &str, selected: usize) {
    let matches = app.filtered_commands(query);
    let body = centered(area, 92, 82);
    frame.render_widget(Clear, body);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(body);
    frame.render_widget(
        Paragraph::new(format!("> {query}_")).block(panel("All Lore commands", true)),
        rows[0],
    );
    let items: Vec<ListItem> = matches
        .iter()
        .map(|spec| {
            let safety = match spec.safety {
                Safety::ReadOnly => "read",
                Safety::Mutating => "write",
                Safety::Destructive => "DANGER",
                Safety::Secret => "secret",
                Safety::LongRunning => "service",
            };
            let unavailable = if spec.available { "" } else { " [unavailable]" };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:8} ", safety),
                    Style::default().fg(if spec.safety == Safety::Destructive {
                        Color::Red
                    } else {
                        Color::DarkGray
                    }),
                ),
                Span::styled(
                    spec.path.clone(),
                    Style::default().fg(if spec.available {
                        Color::White
                    } else {
                        Color::DarkGray
                    }),
                ),
                Span::raw(unavailable),
            ]))
        })
        .collect();
    let mut state =
        ListState::default().with_selected(Some(selected.min(items.len().saturating_sub(1))));
    frame.render_stateful_widget(
        List::new(empty_list(items, "No matching commands"))
            .highlight_style(Style::default().bg(SELECTED))
            .highlight_symbol("› "),
        rows[1],
        &mut state,
    );
    let detail = matches
        .get(selected)
        .map(|spec| format!("{}  |  {}", spec.usage, spec.description))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(detail).block(Block::default().borders(Borders::TOP)),
        rows[2],
    );
}

fn render_prompt(frame: &mut Frame, area: Rect, title: &str, value: &str, secret: bool) {
    let shown = if secret {
        "•".repeat(value.chars().count())
    } else {
        value.into()
    };
    modal(
        frame,
        area,
        76,
        7,
        &format!(" {title} "),
        Paragraph::new(format!("{shown}_\n\nEnter confirms • Esc cancels"))
            .wrap(Wrap { trim: false }),
    );
}

fn render_confirm(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    message: &str,
    required: Option<&str>,
    typed: &str,
) {
    let instructions = required
        .map(|required| format!("Type `{required}` then press Enter:\n{typed}_"))
        .unwrap_or_else(|| "Press y/Enter to confirm, n/Esc to cancel.".into());
    modal(
        frame,
        area,
        72,
        9,
        &format!(" {title} "),
        Paragraph::new(format!("{message}\n\n{instructions}")).wrap(Wrap { trim: false }),
    );
}

fn render_onboarding(frame: &mut Frame, area: Rect, app: &App) {
    let error = app
        .state
        .repository_error
        .as_deref()
        .unwrap_or("No Lore repository found");
    let text = vec![
        Line::from(Span::styled(
            "No usable Lore repository",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(error.to_owned()),
        Line::from(""),
        Line::from(
            "Press Ctrl+P to create or clone a repository, authenticate, or configure a shared store.",
        ),
        Line::from(
            "Examples: repository create, repository clone, auth login, shared-store create",
        ),
        Line::from("Press L to authenticate against an existing repository's server."),
        Line::from("Press : to run a shell command, or q to quit."),
    ];
    modal(
        frame,
        area,
        78,
        14,
        " Welcome to LazyLore ",
        Paragraph::new(text)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
    );
}

fn modal<'a>(
    frame: &mut Frame,
    area: Rect,
    width: u16,
    height: u16,
    title: &str,
    widget: Paragraph<'a>,
) {
    let area = centered(area, width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        widget.block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACTIVE)),
        ),
        area,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = if width <= 100 {
        area.width.saturating_mul(width) / 100
    } else {
        width.min(area.width)
    };
    let height = if height <= 100 {
        area.height.saturating_mul(height) / 100
    } else {
        height.min(area.height)
    };
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width: width.max(1),
        height: height.max(1),
    }
}

fn short_action(action: &str) -> &str {
    match action.to_ascii_lowercase().as_str() {
        "add" => "A",
        "delete" | "remove" => "D",
        "move" => "R",
        "copy" => "C",
        "modify" => "M",
        _ => "?",
    }
}

fn short_hash(hash: &str) -> &str {
    hash.get(..hash.len().min(8)).unwrap_or(hash)
}
fn empty_as<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}
fn empty_list<'a>(items: Vec<ListItem<'a>>, message: &'a str) -> Vec<ListItem<'a>> {
    if items.is_empty() {
        vec![ListItem::new(message)]
    } else {
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn screen(width: u16, height: u16, app: &mut App) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn renders_normal_two_column_layout() {
        let mut app = App::test_fixture();
        let output = screen(120, 40, &mut app);
        assert!(output.contains("LazyLore 0.8.4"));
        assert!(output.contains("2 Files"));
        assert!(output.contains("3 Branches"));
        assert!(output.contains("4 Revisions"));
        assert!(output.contains("5 Locks"));
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("▶ 2 Files"));
        assert!(output.contains('╔'));
    }

    #[test]
    fn renders_narrow_focused_layout() {
        let mut app = App::test_fixture();
        app.focus = Focus::Branches;
        let output = screen(70, 22, &mut app);
        assert!(output.contains("3 Branches"));
        assert!(output.contains("main"));
        assert!(!output.contains("5 Locks"));
    }

    #[test]
    fn renders_command_log_pane_in_wide_layout() {
        let mut app = App::test_fixture();
        // Command log pane is always visible in the wide layout, unfocused by default.
        let output = screen(120, 40, &mut app);
        assert!(
            output.contains("Command Log"),
            "command log pane label missing"
        );
    }

    #[test]
    fn renders_command_log_pane_focused() {
        let mut app = App::test_fixture();
        app.focus = Focus::CommandLog;
        let output = screen(120, 40, &mut app);
        // Focused pane title shows the ▶ marker.
        assert!(
            output.contains("▶ Command Log"),
            "focused command log marker missing"
        );
    }

    #[test]
    fn renders_repository_pane_compact_style() {
        let mut app = App::test_fixture();
        app.focus = Focus::Repository;
        let output = screen(120, 40, &mut app);
        // Branch name in the repo pane.
        assert!(output.contains("main"), "branch name missing");
        // Sync glyph for InSync (both ahead flags false in test_fixture).
        assert!(output.contains('✓'), "sync glyph missing");
        // Remote state.
        assert!(output.contains("connected"), "remote state missing");
    }

    #[test]
    fn renders_logo_and_copyright_when_repository_focused() {
        let mut app = App::test_fixture();
        app.focus = Focus::Repository;
        // Make the Main pane visible (wide layout).
        let output = screen(120, 40, &mut app);
        assert!(
            output.contains("Copyright 2026 Peralysis"),
            "copyright line missing"
        );
    }

    #[test]
    fn renders_branch_sync_glyph_in_branch_list() {
        let mut app = App::test_fixture();
        app.focus = Focus::Branches;
        // test_fixture has main current with both ahead flags false → InSync (✓).
        let output = screen(120, 40, &mut app);
        assert!(
            output.contains('✓'),
            "in-sync glyph missing for current branch"
        );
    }
}
