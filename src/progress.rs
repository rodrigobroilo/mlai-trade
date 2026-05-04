// Terminal progress indicators for long-running CLI work.
//
// Function map:
// - spinner()/bar(): create visible progress when stderr is interactive.
// - Progress::inc/set_position/set_message(): update background renderer state.
// - Drop/finish_and_clear(): stop the renderer and clear the terminal line.

use std::io::{self, IsTerminal, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, MutexGuard,
};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone)]
enum ProgressMode {
    Spinner,
    Bar { total: u64 },
}

struct ProgressState {
    mode: ProgressMode,
    label: String,
    message: String,
    position: u64,
    started_at: Instant,
}

pub struct Progress {
    enabled: bool,
    state: Arc<Mutex<ProgressState>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

// Handles spinner logic.
pub fn spinner(label: impl Into<String>) -> Progress {
    spinner_if(true, label)
}

// Handles spinner if logic.
pub fn spinner_if(show: bool, label: impl Into<String>) -> Progress {
    Progress::new(show, ProgressMode::Spinner, label.into())
}

// Handles bar logic.
pub fn bar(total: u64, label: impl Into<String>) -> Progress {
    bar_if(true, total, label)
}

// Handles bar if logic.
pub fn bar_if(show: bool, total: u64, label: impl Into<String>) -> Progress {
    Progress::new(show, ProgressMode::Bar { total }, label.into())
}

impl Progress {
    // Constructs a new instance with the provided inputs.
    fn new(show: bool, mode: ProgressMode, label: String) -> Self {
        let enabled = show
            && io::stderr().is_terminal()
            && std::env::var("MLAI_TRADE_PROGRESS")
                .map(|value| value != "0" && value.to_ascii_lowercase() != "false")
                .unwrap_or(true);
        let state = Arc::new(Mutex::new(ProgressState {
            mode,
            label,
            message: String::new(),
            position: 0,
            started_at: Instant::now(),
        }));
        let stop = Arc::new(AtomicBool::new(false));

        let handle = if enabled {
            let state_for_thread = Arc::clone(&state);
            let stop_for_thread = Arc::clone(&stop);
            Some(thread::spawn(move || {
                let frames = ["|", "/", "-", "\\"];
                let mut frame = 0usize;
                while !stop_for_thread.load(Ordering::Relaxed) {
                    let line = {
                        let state = lock_state(&state_for_thread);
                        render_line(&state, frames[frame])
                    };
                    eprint!("\r\x1b[2K{line}");
                    let _ = io::stderr().flush();
                    frame = (frame + 1) % frames.len();
                    thread::sleep(Duration::from_millis(120));
                }
            }))
        } else {
            None
        };

        Self {
            enabled,
            state,
            stop,
            handle,
        }
    }

    // Handles inc logic.
    pub fn inc(&self, delta: u64) {
        let mut state = lock_state(&self.state);
        state.position = state.position.saturating_add(delta);
    }

    // Sets position in local state.
    pub fn set_position(&self, position: u64) {
        let mut state = lock_state(&self.state);
        state.position = position;
    }

    // Sets message in local state.
    pub fn set_message(&self, message: impl Into<String>) {
        let mut state = lock_state(&self.state);
        state.message = message.into();
    }

    // Handles finish and clear logic.
    pub fn finish_and_clear(mut self) {
        self.stop_thread();
    }

    // Handles stop thread logic.
    fn stop_thread(&mut self) {
        if !self.enabled {
            return;
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        eprint!("\r\x1b[2K");
        let _ = io::stderr().flush();
    }
}

impl Drop for Progress {
    // Releases owned runtime resources when the wrapper is dropped.
    fn drop(&mut self) {
        self.stop_thread();
    }
}

// Handles lock state logic.
fn lock_state(state: &Arc<Mutex<ProgressState>>) -> MutexGuard<'_, ProgressState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// Handles render line logic.
fn render_line(state: &ProgressState, frame: &str) -> String {
    let elapsed = format_elapsed(state.started_at.elapsed());
    let detail = if state.message.is_empty() {
        String::new()
    } else {
        format!(" - {}", state.message)
    };

    match state.mode {
        ProgressMode::Spinner => {
            format!("  {frame} {}{detail} ({elapsed})", state.label)
        }
        ProgressMode::Bar { total } => {
            let total = total.max(1);
            let position = state.position.min(total);
            let percent = position.saturating_mul(100) / total;
            let width = 28usize;
            let filled = ((position as usize).saturating_mul(width) / total as usize).min(width);
            let bar = format!("{}{}", "=".repeat(filled), " ".repeat(width - filled));
            format!(
                "  {frame} {:>3}% [{}] {}/{} {}{} ({elapsed})",
                percent, bar, position, total, state.label, detail
            )
        }
    }
}

// Formats elapsed for output.
fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}
