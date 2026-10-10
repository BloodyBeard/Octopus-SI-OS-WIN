use std::io::{IsTerminal, Write};
use std::time::Instant;

const LABEL_WIDTH: usize = 11;
const ITEM_WIDTH: usize = 48;

pub struct BootReport {
    styled: bool,
    started: Instant,
    line_open: bool,
}

impl BootReport {
    pub fn new() -> Self {
        Self {
            styled: std::io::stderr().is_terminal(),
            started: Instant::now(),
            line_open: false,
        }
    }

    pub fn banner(&mut self, name: &str) {
        self.finish_line();
        if self.styled {
            eprintln!("\x1b[1m{name}\x1b[0m");
        } else {
            eprintln!("{name}");
        }
    }

    pub fn stage(&mut self, label: &str, detail: impl std::fmt::Display) {
        self.finish_line();
        if self.styled {
            eprintln!("  \x1b[2m{label:<LABEL_WIDTH$}\x1b[0m {detail}");
        } else {
            eprintln!("  {label:<LABEL_WIDTH$} {detail}");
        }
    }

    pub fn progress(&mut self, label: &str, done: usize, total: usize, item: &str) {
        if !self.styled || total == 0 {
            return;
        }
        let percent = done.saturating_mul(100) / total;
        let mut stderr = std::io::stderr().lock();
        let _ = write!(
            stderr,
            "\r\x1b[2K  \x1b[2m{label:<LABEL_WIDTH$}\x1b[0m \x1b[1m{percent:>3}%\x1b[0m {done}/{total} \x1b[2m{}\x1b[0m",
            tail(item, ITEM_WIDTH),
        );
        let _ = stderr.flush();
        self.line_open = true;
    }

    pub fn done(&mut self, verb: &str, detail: impl std::fmt::Display) {
        self.finish_line();
        let seconds = self.started.elapsed().as_secs_f64();
        if self.styled {
            eprintln!("\x1b[1;32m{verb}\x1b[0m\x1b[32m in {seconds:.1}s\x1b[0m {detail}");
        } else {
            eprintln!("{verb} in {seconds:.1}s {detail}");
        }
    }

    fn finish_line(&mut self) {
        if !self.line_open {
            return;
        }
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "\r\x1b[2K");
        let _ = stderr.flush();
        self.line_open = false;
    }
}

fn tail(value: &str, width: usize) -> &str {
    let mut start = value.len().saturating_sub(width);
    while start > 0 && !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}
