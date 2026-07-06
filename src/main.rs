use std::{
    io::{self, stdout},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use lazylore::{
    app::{App, resolve_repository},
    config::Config,
    ui,
};
use ratatui::{Terminal, backend::CrosstermBackend};

#[derive(Debug, Parser)]
#[command(version, about = "A lazygit-inspired terminal UI for Epic's Lore VCS")]
struct Args {
    /// Repository path (defaults to the current directory)
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Lore CLI executable
    #[arg(long, value_name = "PATH")]
    lore_binary: Option<PathBuf>,
    /// LazyLore configuration file
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Disable filesystem watching
    #[arg(long)]
    no_watch: bool,
    /// Perform a full Lore status scan at startup
    #[arg(long)]
    scan: bool,
    /// Enable diagnostic output
    #[arg(long)]
    debug: bool,
    /// Start in offline mode: all server-touching commands are skipped and no
    /// background reconnection probes are run. Toggle at runtime with `O`.
    #[arg(long)]
    offline: bool,
}

struct TerminalGuard {
    mouse: bool,
}

impl TerminalGuard {
    fn enter(mouse: bool) -> Result<Self> {
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        if mouse {
            execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        } else {
            execute!(stdout(), EnterAlternateScreen)?;
        }
        Ok(Self { mouse })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if self.mouse {
            let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        } else {
            let _ = execute!(stdout(), LeaveAlternateScreen);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut config = Config::load(args.config.as_deref())?;
    if let Some(binary) = args.lore_binary {
        config.general.lore_binary = binary;
    }
    if args.no_watch {
        config.general.watch_files = false;
    }
    if args.scan {
        config.general.scan_on_start = true;
    }
    if args.offline {
        config.general.offline = true;
    }
    let path = args
        .path
        .unwrap_or(std::env::current_dir().context("could not determine current directory")?);
    let repository = resolve_repository(&path)?;
    let mut app = App::new(repository, config).await?;

    let _guard = TerminalGuard::enter(app.config.ui.mouse)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    show_loading_screen(&mut terminal, &mut app).await?;
    let result = run(&mut terminal, &mut app).await;
    terminal.show_cursor()?;
    result
}

/// Draw an animated loading screen while `App::load_initial` performs the
/// first, potentially slow, server-touching refresh. Without this the
/// terminal would sit blank until `lore status` returns or times out (default
/// 3s) when the Lore server is unreachable.
async fn show_loading_screen(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let banner = app.banner.clone();
    let offline_after = app.config.command_timeout();
    let start = tokio::time::Instant::now();

    let load = app.load_initial();
    tokio::pin!(load);

    let mut spinner = tokio::time::interval(std::time::Duration::from_millis(120));
    let mut frame_idx = 0usize;
    // Draw one frame immediately so there's instant feedback, before the
    // first spinner tick fires.
    terminal.draw(|frame| ui::render_loading(frame, &banner, frame_idx, false))?;
    loop {
        tokio::select! {
            _ = &mut load => break,
            _ = spinner.tick() => {
                frame_idx += 1;
                let show_offline_hint = start.elapsed() >= offline_after;
                terminal.draw(|frame| ui::render_loading(frame, &banner, frame_idx, show_offline_hint))?;
            }
        }
    }
    Ok(())
}

async fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(app.config.refresh_interval());
    // Flushing on its own short interval (rather than only on the slower
    // `tick` above) keeps the debounce in `flush_watcher` the only thing
    // standing between a settled file edit and it showing up on screen.
    let mut flush_tick = tokio::time::interval(Duration::from_millis(200));
    loop {
        terminal.draw(|frame| ui::render(frame, app))?;
        if app.should_quit {
            break;
        }
        let (stream_rx, watch_rx) = app.receivers();
        tokio::select! {
            event = events.next() => {
                if let Some(Ok(Event::Key(key))) = event
                    && key.kind == KeyEventKind::Press { app.on_key(key).await; }
            }
            message = stream_rx.recv() => {
                if let Some(message) = message {
                    let finished = app.handle_stream(message);
                    if finished { app.refresh_all(false).await; }
                }
            }
            Some(path) = watch_rx.recv() => {
                app.note_path_change(path);
                app.drain_watcher();
            }
            _ = flush_tick.tick() => {
                app.flush_watcher().await;
            }
            _ = tick.tick() => {
                app.maybe_reconnect().await;
            }
        }
    }
    Ok(())
}
