//! Terminal input and rendering for the interactive player.
//!
//! The player works without a terminal: when stdin/stdout are redirected the
//! UI is disabled and commands can still be piped line by line.

use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::mpsc::Sender;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

/// Events produced by the input thread.
#[derive(Clone, Debug)]
pub enum UiEvent {
    /// The command prompt opened, changed, or closed.
    Prompt(Option<String>),
    /// A complete command line was submitted.
    Command(String),
    /// The user asked to quit.
    Quit,
}

/// Restores the terminal when dropped, including on error or panic unwinding.
pub struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    pub fn enable() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self { _private: () })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Owns the live status block: transport state, lyrics, and the prompt.
pub struct Ui {
    interactive: bool,
    prompt: Option<String>,
    drawn_lines: usize,
}

impl Ui {
    pub fn new(interactive: bool) -> Self {
        Self {
            interactive,
            prompt: None,
            drawn_lines: 0,
        }
    }

    pub fn set_prompt(&mut self, prompt: Option<String>) {
        self.prompt = prompt;
    }

    /// Redraws the status block: the status line, any lyric lines, and the
    /// command prompt when it is open.
    pub fn render(&mut self, status: &str, lyrics: &[String]) -> io::Result<()> {
        if !self.interactive {
            return Ok(());
        }

        let mut stdout = io::stdout();
        self.erase_block(&mut stdout)?;

        let mut lines = Vec::with_capacity(lyrics.len() + 2);
        lines.push(status.to_string());
        lines.extend(lyrics.iter().cloned());
        if let Some(buffer) = &self.prompt {
            lines.push(format!(": {buffer}"));
        }

        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                write!(stdout, "\r\n")?;
            }
            write!(stdout, "\x1b[2K{line}")?;
        }
        stdout.flush()?;
        self.drawn_lines = lines.len();
        Ok(())
    }

    /// Prints a message without leaving it glued to the live status block.
    pub fn log(&mut self, message: &str) -> io::Result<()> {
        let mut stdout = io::stdout();
        self.erase_block(&mut stdout)?;

        if self.interactive {
            let normalized = message.replace('\n', "\r\n");
            write!(stdout, "{normalized}\r\n")?;
        } else {
            writeln!(stdout, "{message}")?;
        }
        stdout.flush()
    }

    /// Clears the status block before the process returns to the shell.
    pub fn finish(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout();
        self.erase_block(&mut stdout)?;
        stdout.flush()
    }

    /// Moves back to the first line of the block and clears every line.
    fn erase_block(&mut self, stdout: &mut io::Stdout) -> io::Result<()> {
        if !self.interactive || self.drawn_lines == 0 {
            self.drawn_lines = 0;
            return Ok(());
        }

        // The cursor rests at the end of the last drawn line.
        write!(stdout, "\r\x1b[2K")?;
        for _ in 1..self.drawn_lines {
            write!(stdout, "\x1b[1A\r\x1b[2K")?;
        }
        self.drawn_lines = 0;
        Ok(())
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = self.erase_block(&mut stdout);
        let _ = stdout.flush();
    }
}

/// Starts the appropriate input reader.
///
/// A TTY gets single-key input and a prompt opened with `:`. A pipe is read as
/// one command per line, which makes scripted control possible.
pub fn spawn_input(interactive: bool, sender: Sender<UiEvent>) {
    let stdin_is_tty = io::stdin().is_terminal();

    if interactive {
        let _ = std::thread::Builder::new()
            .name("racf-input".to_string())
            .spawn(move || run_tty_input(sender));
    } else if !stdin_is_tty {
        let _ = std::thread::Builder::new()
            .name("racf-stdin".to_string())
            .spawn(move || run_piped_input(sender));
    }
}

fn run_tty_input(sender: Sender<UiEvent>) {
    let mut prompt: Option<String> = None;

    while let Ok(event) = event::read() {
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            let _ = sender.send(UiEvent::Quit);
            return;
        }

        if let Some(buffer) = prompt.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let line = prompt.take().unwrap_or_default();
                    let _ = sender.send(UiEvent::Prompt(None));
                    if !line.trim().is_empty() {
                        let _ = sender.send(UiEvent::Command(line));
                    }
                }
                KeyCode::Esc => {
                    prompt = None;
                    let _ = sender.send(UiEvent::Prompt(None));
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    let _ = sender.send(UiEvent::Prompt(Some(buffer.clone())));
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buffer.push(character);
                    let _ = sender.send(UiEvent::Prompt(Some(buffer.clone())));
                }
                _ => {}
            }
        } else {
            let shortcut = match key.code {
                KeyCode::Char(':') => {
                    prompt = Some(String::new());
                    let _ = sender.send(UiEvent::Prompt(Some(String::new())));
                    None
                }
                KeyCode::Char(' ') => Some("toggle".to_string()),
                KeyCode::Char('q' | 'Q') => {
                    let _ = sender.send(UiEvent::Quit);
                    return;
                }
                KeyCode::Left => Some("playback -5".to_string()),
                KeyCode::Right => Some("playback +5".to_string()),
                KeyCode::Up => Some("volume +5".to_string()),
                KeyCode::Down => Some("volume -5".to_string()),
                _ => None,
            };

            if let Some(command) = shortcut {
                let _ = sender.send(UiEvent::Command(command));
            }
        }
    }
}

fn run_piped_input(sender: Sender<UiEvent>) {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut line = String::new();

    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let command = line.trim();
                if !command.is_empty()
                    && sender.send(UiEvent::Command(command.to_string())).is_err()
                {
                    break;
                }
            }
        }
    }
}
