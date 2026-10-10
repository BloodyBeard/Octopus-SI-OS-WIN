use crate::color::perceptual_distance;
use octopus_terminal_detection::TerminalName;
use octopus_terminal_detection::terminal_info;
use ratatui::style::Color;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdoutColorLevel {
    TrueColor,
    Ansi256,
    Ansi16,
    Unknown,
}

pub fn stdout_color_level() -> StdoutColorLevel {
    match supports_color::on_cached(supports_color::Stream::Stdout) {
        Some(level) if level.has_16m => StdoutColorLevel::TrueColor,
        Some(level) if level.has_256 => StdoutColorLevel::Ansi256,
        Some(_) => StdoutColorLevel::Ansi16,
        None => StdoutColorLevel::Unknown,
    }
}

#[allow(clippy::disallowed_methods)]
pub fn rgb_color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

#[allow(clippy::disallowed_methods)]
pub fn indexed_color(index: u8) -> Color {
    Color::Indexed(index)
}

pub fn best_color(target: (u8, u8, u8)) -> Color {
    best_color_for_color_level(target, effective_stdout_color_level())
}

pub fn best_color_for_level(target: (u8, u8, u8), color_level: StdoutColorLevel) -> Color {
    best_color_for_color_level(target, color_level)
}

fn effective_stdout_color_level() -> StdoutColorLevel {
    stdout_color_level_for_terminal(
        stdout_color_level(),
        terminal_info().name,
        std::env::var_os("WT_SESSION").is_some(),
        std::env::var_os("FORCE_COLOR").is_some(),
    )
}

fn stdout_color_level_for_terminal(
    stdout_level: StdoutColorLevel,
    terminal_name: TerminalName,
    has_wt_session: bool,
    has_force_color_override: bool,
) -> StdoutColorLevel {
    if has_wt_session && !has_force_color_override {
        return StdoutColorLevel::TrueColor;
    }

    if stdout_level == StdoutColorLevel::Ansi16
        && terminal_name == TerminalName::WindowsTerminal
        && !has_force_color_override
    {
        StdoutColorLevel::TrueColor
    } else {
        stdout_level
    }
}

fn best_color_for_color_level(target: (u8, u8, u8), color_level: StdoutColorLevel) -> Color {
    match color_level {
        StdoutColorLevel::TrueColor => rgb_color(target),
        StdoutColorLevel::Ansi256 => xterm_fixed_colors()
            .min_by(|(_, a), (_, b)| {
                perceptual_distance(*a, target)
                    .partial_cmp(&perceptual_distance(*b, target))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map_or_else(Color::default, |(i, _)| indexed_color(i as u8)),
        StdoutColorLevel::Ansi16 | StdoutColorLevel::Unknown => Color::default(),
    }
}

pub fn requery_default_colors() {
    imp::requery_default_colors();
}

#[derive(Clone, Copy)]
pub struct DefaultColors {
    fg: (u8, u8, u8),
    bg: (u8, u8, u8),
}

pub fn default_colors() -> Option<DefaultColors> {
    imp::default_colors()
}

pub fn default_fg() -> Option<(u8, u8, u8)> {
    default_colors().map(|c| c.fg)
}

pub fn default_bg() -> Option<(u8, u8, u8)> {
    default_colors().map(|c| c.bg)
}

#[cfg(any(unix, windows))]
pub fn set_default_colors_from_startup_probe(colors: Option<crate::terminal_probe::DefaultColors>) {
    imp::set_default_colors_from_startup_probe(colors);
}

#[cfg(all(unix, not(test)))]
mod imp {
    use super::DefaultColors;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    struct Cache<T> {
        attempted: bool,
        value: Option<T>,
    }

    impl<T> Default for Cache<T> {
        fn default() -> Self {
            Self {
                attempted: false,
                value: None,
            }
        }
    }

    impl<T: Copy> Cache<T> {
        fn get_or_init_with(&mut self, mut init: impl FnMut() -> Option<T>) -> Option<T> {
            if !self.attempted {
                self.value = init();
                self.attempted = true;
            }
            self.value
        }
    }

    fn default_colors_cache() -> &'static Mutex<Cache<DefaultColors>> {
        static CACHE: OnceLock<Mutex<Cache<DefaultColors>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(Cache::default()))
    }

    pub(super) fn default_colors() -> Option<DefaultColors> {
        let cache = default_colors_cache();
        let mut cache = cache.lock().ok()?;
        cache.get_or_init_with(query_default_colors)
    }

    pub(super) fn set_default_colors_from_startup_probe(
        colors: Option<crate::terminal_probe::DefaultColors>,
    ) {
        if let Ok(mut cache) = default_colors_cache().lock() {
            cache.value = colors.map(|colors| DefaultColors {
                fg: colors.fg,
                bg: colors.bg,
            });
            cache.attempted = true;
        }
    }

    pub(super) fn requery_default_colors() {
        if let Ok(mut cache) = default_colors_cache().lock() {
            if cache.attempted && cache.value.is_none() {
                return;
            }

            cache.value = query_default_colors();
            cache.attempted = true;
        }
    }

    fn query_default_colors() -> Option<DefaultColors> {
        crate::terminal_probe::default_colors(crate::terminal_probe::DEFAULT_TIMEOUT)
            .ok()
            .flatten()
            .map(|colors| DefaultColors {
                fg: colors.fg,
                bg: colors.bg,
            })
    }
}

#[cfg(windows)]
mod imp {
    use super::DefaultColors;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    struct Cache<T> {
        attempted: bool,
        value: Option<T>,
    }

    impl<T> Default for Cache<T> {
        fn default() -> Self {
            Self {
                attempted: false,
                value: None,
            }
        }
    }

    impl<T: Copy> Cache<T> {
        fn get_or_init_with(&mut self, mut init: impl FnMut() -> Option<T>) -> Option<T> {
            if !self.attempted {
                self.value = init();
                self.attempted = true;
            }
            self.value
        }
    }

    fn default_colors_cache() -> &'static Mutex<Cache<DefaultColors>> {
        static CACHE: OnceLock<Mutex<Cache<DefaultColors>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(Cache::default()))
    }

    pub(super) fn default_colors() -> Option<DefaultColors> {
        let cache = default_colors_cache();
        let mut cache = cache.lock().ok()?;
        cache.get_or_init_with(query_default_colors)
    }

    pub(super) fn set_default_colors_from_startup_probe(
        colors: Option<crate::terminal_probe::DefaultColors>,
    ) {
        if let Ok(mut cache) = default_colors_cache().lock() {
            cache.value = colors.map(|colors| DefaultColors {
                fg: colors.fg,
                bg: colors.bg,
            });
            cache.attempted = true;
        }
    }

    pub(super) fn requery_default_colors() {}

    fn query_default_colors() -> Option<DefaultColors> {
        crate::terminal_probe::default_colors(crate::terminal_probe::DEFAULT_TIMEOUT)
            .ok()
            .flatten()
            .map(|colors| DefaultColors {
                fg: colors.fg,
                bg: colors.bg,
            })
    }
}

#[cfg(not(any(all(unix, not(test)), windows)))]
mod imp {
    use super::DefaultColors;

    pub(super) fn default_colors() -> Option<DefaultColors> {
        None
    }

    #[cfg(any(unix, windows))]
    pub(super) fn set_default_colors_from_startup_probe(
        _colors: Option<crate::terminal_probe::DefaultColors>,
    ) {
    }

    pub(super) fn requery_default_colors() {}
}

fn xterm_fixed_colors() -> impl Iterator<Item = (usize, (u8, u8, u8))> {
    XTERM_COLORS.into_iter().enumerate().skip(16)
}

pub const XTERM_COLORS: [(u8, u8, u8); 256] = [
    (0, 0, 0),
    (128, 0, 0),
    (0, 128, 0),
    (128, 128, 0),
    (0, 0, 128),
    (128, 0, 128),
    (0, 128, 128),
    (192, 192, 192),
    (128, 128, 128),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (0, 0, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
    (0, 0, 0),
    (0, 0, 95),
    (0, 0, 135),
    (0, 0, 175),
    (0, 0, 215),
    (0, 0, 255),
    (0, 95, 0),
    (0, 95, 95),
    (0, 95, 135),
    (0, 95, 175),
    (0, 95, 215),
    (0, 95, 255),
    (0, 135, 0),
    (0, 135, 95),
    (0, 135, 135),
    (0, 135, 175),
    (0, 135, 215),
    (0, 135, 255),
    (0, 175, 0),
    (0, 175, 95),
    (0, 175, 135),
    (0, 175, 175),
    (0, 175, 215),
    (0, 175, 255),
    (0, 215, 0),
    (0, 215, 95),
    (0, 215, 135),
    (0, 215, 175),
    (0, 215, 215),
    (0, 215, 255),
    (0, 255, 0),
    (0, 255, 95),
    (0, 255, 135),
    (0, 255, 175),
    (0, 255, 215),
    (0, 255, 255),
    (95, 0, 0),
    (95, 0, 95),
    (95, 0, 135),
    (95, 0, 175),
    (95, 0, 215),
    (95, 0, 255),
    (95, 95, 0),
    (95, 95, 95),
    (95, 95, 135),
    (95, 95, 175),
    (95, 95, 215),
    (95, 95, 255),
    (95, 135, 0),
    (95, 135, 95),
    (95, 135, 135),
    (95, 135, 175),
    (95, 135, 215),
    (95, 135, 255),
    (95, 175, 0),
    (95, 175, 95),
    (95, 175, 135),
    (95, 175, 175),
    (95, 175, 215),
    (95, 175, 255),
    (95, 215, 0),
    (95, 215, 95),
    (95, 215, 135),
    (95, 215, 175),
    (95, 215, 215),
    (95, 215, 255),
    (95, 255, 0),
    (95, 255, 95),
    (95, 255, 135),
    (95, 255, 175),
    (95, 255, 215),
    (95, 255, 255),
    (135, 0, 0),
    (135, 0, 95),
    (135, 0, 135),
    (135, 0, 175),
    (135, 0, 215),
    (135, 0, 255),
    (135, 95, 0),
    (135, 95, 95),
    (135, 95, 135),
    (135, 95, 175),
    (135, 95, 215),
    (135, 95, 255),
    (135, 135, 0),
    (135, 135, 95),
    (135, 135, 135),
    (135, 135, 175),
    (135, 135, 215),
    (135, 135, 255),
    (135, 175, 0),
    (135, 175, 95),
    (135, 175, 135),
    (135, 175, 175),
    (135, 175, 215),
    (135, 175, 255),
    (135, 215, 0),
    (135, 215, 95),
    (135, 215, 135),
    (135, 215, 175),
    (135, 215, 215),
    (135, 215, 255),
    (135, 255, 0),
    (135, 255, 95),
    (135, 255, 135),
    (135, 255, 175),
    (135, 255, 215),
    (135, 255, 255),
    (175, 0, 0),
    (175, 0, 95),
    (175, 0, 135),
    (175, 0, 175),
    (175, 0, 215),
    (175, 0, 255),
    (175, 95, 0),
    (175, 95, 95),
    (175, 95, 135),
    (175, 95, 175),
    (175, 95, 215),
    (175, 95, 255),
    (175, 135, 0),
    (175, 135, 95),
    (175, 135, 135),
    (175, 135, 175),
    (175, 135, 215),
    (175, 135, 255),
    (175, 175, 0),
    (175, 175, 95),
    (175, 175, 135),
    (175, 175, 175),
    (175, 175, 215),
    (175, 175, 255),
    (175, 215, 0),
    (175, 215, 95),
    (175, 215, 135),
    (175, 215, 175),
    (175, 215, 215),
    (175, 215, 255),
    (175, 255, 0),
    (175, 255, 95),
    (175, 255, 135),
    (175, 255, 175),
    (175, 255, 215),
    (175, 255, 255),
    (215, 0, 0),
    (215, 0, 95),
    (215, 0, 135),
    (215, 0, 175),
    (215, 0, 215),
    (215, 0, 255),
    (215, 95, 0),
    (215, 95, 95),
    (215, 95, 135),
    (215, 95, 175),
    (215, 95, 215),
    (215, 95, 255),
    (215, 135, 0),
    (215, 135, 95),
    (215, 135, 135),
    (215, 135, 175),
    (215, 135, 215),
    (215, 135, 255),
    (215, 175, 0),
    (215, 175, 95),
    (215, 175, 135),
    (215, 175, 175),
    (215, 175, 215),
    (215, 175, 255),
    (215, 215, 0),
    (215, 215, 95),
    (215, 215, 135),
    (215, 215, 175),
    (215, 215, 215),
    (215, 215, 255),
    (215, 255, 0),
    (215, 255, 95),
    (215, 255, 135),
    (215, 255, 175),
    (215, 255, 215),
    (215, 255, 255),
    (255, 0, 0),
    (255, 0, 95),
    (255, 0, 135),
    (255, 0, 175),
    (255, 0, 215),
    (255, 0, 255),
    (255, 95, 0),
    (255, 95, 95),
    (255, 95, 135),
    (255, 95, 175),
    (255, 95, 215),
    (255, 95, 255),
    (255, 135, 0),
    (255, 135, 95),
    (255, 135, 135),
    (255, 135, 175),
    (255, 135, 215),
    (255, 135, 255),
    (255, 175, 0),
    (255, 175, 95),
    (255, 175, 135),
    (255, 175, 175),
    (255, 175, 215),
    (255, 175, 255),
    (255, 215, 0),
    (255, 215, 95),
    (255, 215, 135),
    (255, 215, 175),
    (255, 215, 215),
    (255, 215, 255),
    (255, 255, 0),
    (255, 255, 95),
    (255, 255, 135),
    (255, 255, 175),
    (255, 255, 215),
    (255, 255, 255),
    (8, 8, 8),
    (18, 18, 18),
    (28, 28, 28),
    (38, 38, 38),
    (48, 48, 48),
    (58, 58, 58),
    (68, 68, 68),
    (78, 78, 78),
    (88, 88, 88),
    (98, 98, 98),
    (108, 108, 108),
    (118, 118, 118),
    (128, 128, 128),
    (138, 138, 138),
    (148, 148, 148),
    (158, 158, 158),
    (168, 168, 168),
    (178, 178, 178),
    (188, 188, 188),
    (198, 198, 198),
    (208, 208, 208),
    (218, 218, 218),
    (228, 228, 228),
    (238, 238, 238),
];

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn best_color_uses_truecolor_without_quantization() {
        assert_eq!(
            best_color_for_color_level((12, 34, 56), StdoutColorLevel::TrueColor),
            rgb_color((12, 34, 56))
        );
    }

    #[test]
    fn best_color_resets_for_ansi16() {
        assert_eq!(
            best_color_for_color_level((12, 34, 56), StdoutColorLevel::Ansi16),
            Color::Reset
        );
    }

    #[test]
    fn windows_terminal_wt_session_promotes_to_truecolor() {
        assert_eq!(
            stdout_color_level_for_terminal(
                StdoutColorLevel::Ansi16,
                TerminalName::Unknown,
                true,
                false,
            ),
            StdoutColorLevel::TrueColor
        );
    }

    #[test]
    fn windows_terminal_name_promotes_ansi16_to_truecolor() {
        assert_eq!(
            stdout_color_level_for_terminal(
                StdoutColorLevel::Ansi16,
                TerminalName::WindowsTerminal,
                false,
                false,
            ),
            StdoutColorLevel::TrueColor
        );
    }

    #[test]
    fn force_color_keeps_reported_stdout_level() {
        assert_eq!(
            stdout_color_level_for_terminal(
                StdoutColorLevel::Ansi16,
                TerminalName::WindowsTerminal,
                true,
                true,
            ),
            StdoutColorLevel::Ansi16
        );
    }
}
