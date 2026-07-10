mod app;
mod render;

use crate::runtime::{AppCommand, CommandRequest, SharedRuntime};
use anyhow::Result;
use app::{TuiApp, UiAction};
use crossbeam_channel::Sender;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use render::draw;
use std::io;
use std::thread;
use std::time::{Duration, Instant};

const DRAW_INTERVAL: Duration = Duration::from_millis(250);

pub fn start_tui(
    runtime: SharedRuntime,
    command_tx: Sender<CommandRequest>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if let Err(err) = run_tui(runtime.clone(), command_tx) {
            runtime.log("tui", format!("TUI stopped: {err:#}"));
        }
    })
}

fn run_tui(runtime: SharedRuntime, command_tx: Sender<CommandRequest>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_tui_loop(&mut terminal, runtime, command_tx);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), Show, LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_tui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    runtime: SharedRuntime,
    command_tx: Sender<CommandRequest>,
) -> Result<()> {
    let mut app = TuiApp::default();
    let mut snapshot = runtime.snapshot();
    let mut last_draw = Instant::now() - DRAW_INTERVAL;
    loop {
        if last_draw.elapsed() >= DRAW_INTERVAL {
            snapshot = runtime.snapshot();
            app.observe(&snapshot);
            terminal.draw(|frame| draw(frame, &mut app, &snapshot))?;
            last_draw = Instant::now();
        }

        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let event = event::read()?;
        let action = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key, &snapshot),
            Event::Resize(_, _) => UiAction::None,
            _ => continue,
        };
        let should_exit = match action {
            UiAction::None => false,
            UiAction::Command(command) => {
                dispatch_command(&runtime, &command_tx, command);
                false
            }
            UiAction::Shutdown => dispatch_command(&runtime, &command_tx, AppCommand::Shutdown),
        };
        if should_exit {
            break;
        }
        last_draw = Instant::now() - DRAW_INTERVAL;
    }
    Ok(())
}

fn dispatch_command(
    runtime: &SharedRuntime,
    command_tx: &Sender<CommandRequest>,
    command: AppCommand,
) -> bool {
    let exit_after_send = matches!(command, AppCommand::Shutdown);
    let request = runtime.queue_command(command);
    let command_id = request.id;
    if command_tx.send(request).is_err() {
        runtime.fail_command(command_id, "main command channel is closed");
        false
    } else {
        exit_after_send
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runtime::{CommandPhase, RuntimeState};

    #[test]
    fn dispatch_failure_is_recorded() {
        let runtime = RuntimeState::new(Config::default());
        let (sender, receiver) = crossbeam_channel::unbounded();
        drop(receiver);
        assert!(!dispatch_command(
            &runtime,
            &sender,
            AppCommand::ReconnectRelay
        ));
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.commands.len(), 1);
        assert_eq!(snapshot.commands[0].phase, CommandPhase::Failed);
    }
}
