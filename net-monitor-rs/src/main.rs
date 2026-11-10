mod calculator;
mod collector;
mod ui;

use anyhow::Result;
use calculator::StateManager;
use collector::read_network_stats;
use crossterm::{
    cursor::Show,
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::panic;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Returns the shell to its normal state. Idempotent, and errors are swallowed
/// because there is no usable terminal left to report them to.
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        Show
    );
}

/// Restores the terminal on every exit path out of `main`, including the ones
/// taken by `?` and by a panic.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// The default hook writes its message while the alternate screen is still
/// active, so restore first or the panic text disappears with the screen.
fn hook_panic_restore() {
    let default = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default(info);
    }));
}

fn main() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let _guard = TerminalGuard;
    hook_panic_restore();

    // Channel for communication between Collector and UI
    let (tx, rx) = mpsc::channel();

    // Background thread for data collection and rate calculation
    thread::spawn(move || {
        let mut state = StateManager::new();

        // Establish baseline
        if let Ok(initial) = read_network_stats() {
            state.update(initial);
        }

        loop {
            // Sleep for 1 second between samples
            thread::sleep(Duration::from_secs(1));

            if let Ok(current_stats) = read_network_stats() {
                state.update(current_stats);
                // Send the latest calculated rates to the UI thread
                if tx.send(state.current_rates.clone()).is_err() {
                    break; // UI thread has closed
                }
            }
        }
    });

    let mut current_rates = Vec::new();

    // Main UI Loop
    loop {
        // Drain the channel to ensure we always render the latest data
        while let Ok(rates) = rx.try_recv() {
            current_rates = rates;
        }

        terminal.draw(|f| {
            ui::draw(f, &current_rates);
        })?;

        // Handle keyboard events (100ms poll rate for UI responsiveness)
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && (key.code == KeyCode::Char('q') || key.code == KeyCode::Esc)
        {
            break;
        }
    }

    // Dropping `_guard` restores the terminal.
    Ok(())
}
