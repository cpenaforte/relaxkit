//! Terminal rendering shared by the commands. Layout is built as plain text by pure functions, so a test reads exactly
//! what prints. Color is added last, and only on a terminal that wants it. Output that a pipe or a file receives has no
//! escape codes, and a terminal that is not UTF-8 gets ASCII marks.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::exec::CommandFailed;

/// The most lines of a failed command's output an error shows. The rest are counted, not printed.
pub const OUTPUT_LINES: usize = 6;

/// How long one spinner frame stays on screen.
const FRAME_INTERVAL: Duration = Duration::from_millis(80);

/// The outcome shown as a glyph at the start of a line. Every command reads the marks the same way: `Ok` is done,
/// `Warn` is done with something to look at, or a result that could not be decided, `Fail` is a failure, and `Skip`
/// is not done: a plan in a dry run, a step that does not apply, or nothing checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Ok,
    Warn,
    Fail,
    Skip,
}

/// A text style. Each one is a single ANSI attribute or color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Bold,
    Dim,
    Red,
    Green,
    Yellow,
}

/// What a stream can show: whether it gets color, whether it can show glyphs beyond ASCII, and whether it can animate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    color: bool,
    unicode: bool,
    live: bool,
}

impl Default for Palette {
    fn default() -> Palette {
        Palette::PLAIN
    }
}

impl Palette {
    /// Unicode glyphs and no color: what tests and the library get.
    pub const PLAIN: Palette = Palette {
        color: false,
        unicode: true,
        live: false,
    };

    /// ASCII glyphs and no color: what a terminal without a UTF-8 locale gets.
    pub const ASCII: Palette = Palette {
        color: false,
        unicode: false,
        live: false,
    };

    /// The palette for a stream, from the environment. `is_terminal` says whether the stream is a terminal.
    pub fn for_stream(is_terminal: bool) -> Palette {
        let var = |key: &str| std::env::var(key).ok();
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .filter_map(|key| var(key).filter(|value| !value.is_empty()))
            .next();
        Palette::decide(
            is_terminal,
            var("NO_COLOR").as_deref(),
            var("TERM").as_deref(),
            locale.as_deref(),
        )
    }

    /// The decision itself, pure. Color needs a terminal, an unset or empty `NO_COLOR` (no-color.org) and a `TERM`
    /// that is not `dumb`. Animation needs a terminal and a `TERM` that is not `dumb`. Unicode needs a UTF-8 locale.
    pub fn decide(
        is_terminal: bool,
        no_color: Option<&str>,
        term: Option<&str>,
        locale: Option<&str>,
    ) -> Palette {
        let dumb = term == Some("dumb");
        let unicode = locale.is_some_and(|locale| {
            let locale = locale.to_ascii_lowercase();
            locale.contains("utf-8") || locale.contains("utf8")
        });
        Palette {
            color: is_terminal && !dumb && no_color.unwrap_or("").is_empty(),
            unicode,
            live: is_terminal && !dumb,
        }
    }

    /// Whether this stream may animate a line (a spinner).
    pub fn is_live(self) -> bool {
        self.live
    }

    /// The glyph for a mark, without color.
    pub fn glyph(self, mark: Mark) -> &'static str {
        match (self.unicode, mark) {
            (true, Mark::Ok) => "✓",
            (true, Mark::Warn) => "!",
            (true, Mark::Fail) => "✗",
            (true, Mark::Skip) => "·",
            (false, Mark::Ok) => "ok",
            (false, Mark::Warn) => "!!",
            (false, Mark::Fail) => "xx",
            (false, Mark::Skip) => "..",
        }
    }

    /// The text in a style, with ANSI codes only when color is on.
    pub fn paint(self, text: &str, style: Style) -> String {
        if !self.color {
            return text.to_string();
        }
        let code = match style {
            Style::Bold => "1",
            Style::Dim => "2",
            Style::Red => "31",
            Style::Green => "32",
            Style::Yellow => "33",
        };
        format!("\u{1b}[{code}m{text}\u{1b}[0m")
    }

    /// The mark's glyph in its color: green for ok, yellow for a warning, red for a failure, dim for a skip.
    pub fn mark(self, mark: Mark) -> String {
        let style = match mark {
            Mark::Ok => Style::Green,
            Mark::Warn => Style::Yellow,
            Mark::Fail => Style::Red,
            Mark::Skip => Style::Dim,
        };
        self.paint(self.glyph(mark), style)
    }

    /// The mark at the start of a step line: the glyph in its color, padded to two columns, then a space. A glyph
    /// that takes one column is padded, so the step names line up in every glyph set.
    pub fn mark_column(self, mark: Mark) -> String {
        let pad = if self.unicode { " " } else { "" };
        format!("{}{pad} ", self.mark(mark))
    }

    /// The frames of a spinner in this palette's glyph set.
    pub fn spinner_frames(self) -> &'static [&'static str] {
        if self.unicode {
            &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        } else {
            &["|", "/", "-", "\\"]
        }
    }
}

/// One table cell: text, an optional mark before it, and an optional style for the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    text: String,
    mark: Option<Mark>,
    style: Option<Style>,
}

impl Cell {
    pub fn new(text: impl Into<String>) -> Cell {
        Cell {
            text: text.into(),
            mark: None,
            style: None,
        }
    }

    pub fn styled(text: impl Into<String>, style: Style) -> Cell {
        Cell {
            text: text.into(),
            mark: None,
            style: Some(style),
        }
    }

    pub fn marked(mark: Mark, text: impl Into<String>) -> Cell {
        Cell {
            text: text.into(),
            mark: Some(mark),
            style: None,
        }
    }

    /// The characters the cell takes on screen, with no color codes.
    fn plain(&self, palette: Palette) -> String {
        match (self.mark, self.text.is_empty()) {
            (Some(mark), true) => palette.glyph(mark).to_string(),
            (Some(mark), false) => format!("{} {}", palette.glyph(mark), self.text),
            (None, _) => self.text.clone(),
        }
    }

    fn render(&self, palette: Palette) -> String {
        let text = match self.style {
            Some(style) => palette.paint(&self.text, style),
            None => self.text.clone(),
        };
        match (self.mark, self.text.is_empty()) {
            (Some(mark), true) => palette.mark(mark),
            (Some(mark), false) => format!("{} {text}", palette.mark(mark)),
            (None, _) => text,
        }
    }
}

impl From<&str> for Cell {
    fn from(text: &str) -> Cell {
        Cell::new(text)
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Cell {
        Cell::new(text)
    }
}

/// Renders `rows` under `headers` as aligned columns joined by two spaces, every line indented by `indent` spaces. A
/// column is as wide as its widest cell. The last column is not padded, so no line ends in spaces.
pub fn table(headers: &[&str], rows: &[Vec<Cell>], indent: usize, palette: Palette) -> Vec<String> {
    let columns = headers.len();
    let header_row: Vec<Cell> = headers
        .iter()
        .map(|header| Cell::styled(*header, Style::Dim))
        .collect();
    let all: Vec<&Vec<Cell>> = std::iter::once(&header_row).chain(rows).collect();
    let mut widths = vec![0; columns];
    for row in &all {
        for (index, cell) in row.iter().take(columns).enumerate() {
            widths[index] = widths[index].max(cell.plain(palette).chars().count());
        }
    }
    let pad = " ".repeat(indent);
    all.iter()
        .map(|row| {
            let mut line = pad.clone();
            for (index, cell) in row.iter().take(columns).enumerate() {
                line.push_str(&cell.render(palette));
                if index + 1 < columns {
                    let gap = widths[index] - cell.plain(palette).chars().count() + 2;
                    line.push_str(&" ".repeat(gap));
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}

/// A section title: bold on a terminal, plain otherwise.
pub fn heading(text: &str, palette: Palette) -> String {
    palette.paint(text, Style::Bold)
}

/// A result line: its mark, then the text. Every command prints what happened this way, so a line reads the same
/// whichever command printed it.
pub fn outcome(palette: Palette, mark: Mark, text: &str) -> String {
    format!("{}{text}", palette.mark_column(mark))
}

/// A result under another one, indented two columns.
pub fn detail(palette: Palette, mark: Mark, text: &str) -> String {
    format!("  {}", outcome(palette, mark, text))
}

/// Lines of `key  value` with the keys aligned. A value can take several lines; the rest sit under the first.
pub fn key_values(pairs: &[(&str, Vec<String>)], indent: usize) -> Vec<String> {
    let width = pairs
        .iter()
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    let pad = " ".repeat(indent);
    let mut lines = Vec::new();
    for (key, values) in pairs {
        for (index, value) in values.iter().enumerate() {
            let label = if index == 0 { *key } else { "" };
            lines.push(
                format!("{pad}{label:<width$}  {value}")
                    .trim_end()
                    .to_string(),
            );
        }
    }
    lines
}

/// The lines of a command's own output, cleaned for a person: blank lines, stack frames (`at f (file:1:2)`) and lines
/// that are only a brace are dropped, and trailing spaces are cut. Leading spaces stay, so a nested cause keeps its
/// indent.
pub fn clean_output(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !is_stack_frame(line.trim()))
        .filter(|line| !matches!(line.trim(), "{" | "}" | "})" | "},"))
        .map(str::to_string)
        .collect()
}

/// A stack frame: `at name (location)`, optionally followed by an opening brace of an object that a stack shows.
fn is_stack_frame(line: &str) -> bool {
    line.starts_with("at ") && line.contains('(') && (line.ends_with(')') || line.ends_with('{'))
}

/// A known cause in a failed command's output, with what it means for the person. Unknown output gets no hint.
pub fn hint_for(failure: &CommandFailed) -> Option<&'static str> {
    if failure.output.contains("git reference could not be found") {
        return Some("the git tag or branch in the command does not exist on GitHub");
    }
    None
}

/// How a command failure prints: a headline, the failed command with its output and a hint when one is known, and
/// each cause after it. Another program's output is cleaned by `clean_output` and cut to `OUTPUT_LINES`.
pub fn error_lines(err: &anyhow::Error, palette: Palette) -> Vec<String> {
    let chain: Vec<&(dyn std::error::Error + 'static)> = err.chain().collect();
    let top = chain[0];
    let top_text = top.to_string();
    let top_failed = top.downcast_ref::<CommandFailed>();
    let headline = match top_failed {
        Some(failure) => format!("command failed with exit {}", failure.code),
        None => clean_output(&top_text)
            .first()
            .map(|line| line.trim().to_string())
            .unwrap_or_default(),
    };
    let mut lines = vec![format!(
        "{} {headline}",
        palette.paint("Error:", Style::Red)
    )];
    if top_failed.is_none() {
        for line in clean_output(&top_text).iter().skip(1) {
            lines.push(format!("  {}", line.trim_start()));
        }
    }
    if let Some(failure) = chain
        .iter()
        .find_map(|cause| cause.downcast_ref::<CommandFailed>())
    {
        lines.extend(failure_lines(failure));
    }
    for cause in chain.iter().skip(1) {
        if cause.downcast_ref::<CommandFailed>().is_none() {
            let first = clean_output(&cause.to_string())
                .first()
                .map(|line| line.trim().to_string())
                .unwrap_or_default();
            lines.push(format!("  caused by: {first}"));
        }
    }
    lines
}

/// The aligned block a failed command prints under the headline.
fn failure_lines(failure: &CommandFailed) -> Vec<String> {
    let output: Vec<String> = clean_output(&failure.output)
        .iter()
        .map(|line| line.trim_start().to_string())
        .collect();
    let mut shown: Vec<String> = output.iter().take(OUTPUT_LINES).cloned().collect();
    if output.len() > OUTPUT_LINES {
        shown.push(format!("... {} more lines", output.len() - OUTPUT_LINES));
    }
    let mut pairs: Vec<(&str, Vec<String>)> = vec![("command", vec![failure.command.clone()])];
    if !shown.is_empty() {
        pairs.push(("output", shown));
    }
    if let Some(hint) = hint_for(failure) {
        pairs.push(("hint", vec![hint.to_string()]));
    }
    key_values(&pairs, 2)
}

/// Where a line goes: standard output, or standard error, where warnings and spinners go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Set while a spinner is on the screen, so a line printed meanwhile can clear it first.
static SPINNING: AtomicBool = AtomicBool::new(false);
/// One lock for every write that shares a terminal line, so a spinner frame and a printed line never interleave.
static LINE: Mutex<()> = Mutex::new(());

fn line_lock() -> MutexGuard<'static, ()> {
    LINE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Prints one line to `stream`. A spinner on screen is cleared first, so the line lands where the spinner was.
pub fn print_line(stream: Stream, line: &str) {
    let _guard = line_lock();
    if SPINNING.load(Ordering::SeqCst) {
        let _ = write!(io::stderr().lock(), "\r\u{1b}[2K");
    }
    match stream {
        Stream::Stdout => write_line(&mut io::stdout().lock(), line),
        Stream::Stderr => write_line(&mut io::stderr().lock(), line),
    }
}

/// Writes one line. A closed pipe or a full disk must not panic the process, so the error is ignored.
fn write_line(out: &mut dyn Write, line: &str) {
    let _ = writeln!(out, "{line}");
}

/// A spinner shown while a step runs. On a terminal it animates on standard error and is cleared when it ends. Anywhere
/// else it does nothing, so pipes and files get no animation codes.
pub struct Spinner {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Spinner {
    /// Starts the spinner with `label` when `palette` can animate, and returns at once.
    pub fn start(label: &str, palette: Palette) -> Spinner {
        if !palette.is_live() {
            return Spinner {
                stop: None,
                thread: None,
            };
        }
        let (stop, ticks) = mpsc::channel::<()>();
        let label = label.to_string();
        let frames = palette.spinner_frames();
        let thread = thread::spawn(move || {
            let mut frame = 0;
            loop {
                {
                    let _guard = line_lock();
                    SPINNING.store(true, Ordering::SeqCst);
                    let mut err = io::stderr().lock();
                    let _ = write!(err, "\r\u{1b}[2K{} {label}", frames[frame % frames.len()]);
                    let _ = err.flush();
                }
                frame += 1;
                match ticks.recv_timeout(FRAME_INTERVAL) {
                    Err(RecvTimeoutError::Timeout) => continue,
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            let _guard = line_lock();
            let _ = write!(io::stderr().lock(), "\r\u{1b}[2K");
            SPINNING.store(false, Ordering::SeqCst);
        });
        Spinner {
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// Ends the animation and clears its line. Output printed after this starts on a clean line.
    pub fn finish(mut self) {
        self.end();
    }

    fn end(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.end();
    }
}

/// Whether standard error is a terminal, which is where a spinner animates.
pub fn stderr_is_terminal() -> bool {
    io::stderr().is_terminal()
}

/// Whether standard output is a terminal.
pub fn stdout_is_terminal() -> bool {
    io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;
    use crate::exec::CommandFailed;

    const COLOR: Palette = Palette {
        color: true,
        unicode: true,
        live: true,
    };

    /// The text with ANSI escape sequences removed.
    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for next in chars.by_ref() {
                    if next == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// A writer whose far end has closed, as after `relaxkit ... | head`.
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn write_line_ignores_a_closed_pipe() {
        write_line(&mut ClosedPipe, "hello");
    }

    #[test]
    fn color_needs_a_terminal_a_clear_no_color_and_a_real_term() {
        let utf8 = Some("en_US.UTF-8");
        assert!(Palette::decide(true, None, Some("xterm"), utf8).color);
        assert!(Palette::decide(true, Some(""), Some("xterm"), utf8).color);
        assert!(!Palette::decide(false, None, Some("xterm"), utf8).color);
        assert!(!Palette::decide(true, Some("1"), Some("xterm"), utf8).color);
        assert!(!Palette::decide(true, None, Some("dumb"), utf8).color);
        assert!(Palette::decide(true, None, None, utf8).color);
    }

    #[test]
    fn glyphs_fall_back_to_ascii_without_a_utf8_locale() {
        assert!(Palette::decide(false, None, None, Some("en_US.UTF-8")).unicode);
        assert!(Palette::decide(false, None, None, Some("C.utf8")).unicode);
        assert!(!Palette::decide(false, None, None, Some("C")).unicode);
        assert!(!Palette::decide(false, None, None, None).unicode);
    }

    #[test]
    fn animation_needs_a_terminal_that_is_not_dumb() {
        assert!(Palette::decide(true, None, Some("xterm"), None).live);
        assert!(!Palette::decide(true, None, Some("dumb"), None).live);
        assert!(!Palette::decide(false, None, Some("xterm"), None).live);
    }

    #[test]
    fn paint_wraps_text_in_ansi_only_when_enabled() {
        assert_eq!(Palette::PLAIN.paint("ok", Style::Green), "ok");
        assert_eq!(COLOR.paint("ok", Style::Green), "\u{1b}[32mok\u{1b}[0m");
        assert_eq!(COLOR.paint("x", Style::Bold), "\u{1b}[1mx\u{1b}[0m");
        assert_eq!(COLOR.paint("x", Style::Dim), "\u{1b}[2mx\u{1b}[0m");
        assert_eq!(COLOR.paint("x", Style::Red), "\u{1b}[31mx\u{1b}[0m");
        assert_eq!(COLOR.paint("x", Style::Yellow), "\u{1b}[33mx\u{1b}[0m");
    }

    #[test]
    fn marks_have_one_glyph_each_in_each_glyph_set() {
        assert_eq!(Palette::PLAIN.glyph(Mark::Ok), "✓");
        assert_eq!(Palette::PLAIN.glyph(Mark::Warn), "!");
        assert_eq!(Palette::PLAIN.glyph(Mark::Fail), "✗");
        assert_eq!(Palette::PLAIN.glyph(Mark::Skip), "·");
        assert_eq!(Palette::ASCII.glyph(Mark::Ok), "ok");
        assert_eq!(Palette::ASCII.glyph(Mark::Warn), "!!");
        assert_eq!(Palette::ASCII.glyph(Mark::Fail), "xx");
        assert_eq!(Palette::ASCII.glyph(Mark::Skip), "..");
        assert_eq!(Palette::PLAIN.mark(Mark::Fail), "✗");
        assert_eq!(COLOR.mark(Mark::Fail), "\u{1b}[31m✗\u{1b}[0m");
    }

    #[test]
    fn a_table_pads_each_column_to_its_widest_cell_and_leaves_no_trailing_space() {
        let rows = vec![
            vec![
                Cell::new("caveman"),
                Cell::new("claude"),
                Cell::new("4.2.0"),
            ],
            vec![Cell::new("rtk"), Cell::new("opencode"), Cell::new("")],
        ];

        let lines = table(&["tool", "host", "version"], &rows, 0, Palette::PLAIN);

        assert_eq!(
            lines,
            vec![
                "tool     host      version",
                "caveman  claude    4.2.0",
                "rtk      opencode",
            ]
        );
    }

    #[test]
    fn a_table_indents_every_line() {
        let rows = vec![vec![Cell::new("a"), Cell::new("b")]];

        let lines = table(&["x", "y"], &rows, 2, Palette::PLAIN);

        assert_eq!(lines, vec!["  x  y", "  a  b"]);
    }

    #[test]
    fn color_does_not_change_the_layout() {
        let rows = vec![
            vec![
                Cell::marked(Mark::Ok, "as locked"),
                Cell::styled("4.2.0", Style::Dim),
            ],
            vec![Cell::marked(Mark::Fail, "not found"), Cell::new("-")],
        ];

        let plain = table(&["state", "version"], &rows, 1, Palette::PLAIN);
        let colored = table(&["state", "version"], &rows, 1, COLOR);

        let stripped: Vec<String> = colored.iter().map(|line| strip(line)).collect();
        assert_eq!(stripped, plain);
        assert!(colored[1].contains("\u{1b}[32m✓\u{1b}[0m"), "{colored:?}");
    }

    #[test]
    fn an_ascii_table_uses_the_ascii_glyph_in_the_width() {
        let rows = vec![vec![Cell::marked(Mark::Warn, "drift"), Cell::new("1.0")]];

        let lines = table(&["state", "version"], &rows, 0, Palette::ASCII);

        assert_eq!(lines, vec!["state     version", "!! drift  1.0"]);
    }

    #[test]
    fn a_marked_cell_shows_its_glyph_before_the_text() {
        let rows = vec![vec![Cell::marked(Mark::Warn, "drift")]];

        let lines = table(&["state"], &rows, 0, Palette::PLAIN);

        assert_eq!(lines, vec!["state", "! drift"]);
    }

    #[test]
    fn key_values_align_their_keys_and_indent_continuation_lines() {
        let pairs = vec![
            ("command", vec!["doctor".to_string()]),
            ("output", vec!["first".to_string(), "second".to_string()]),
        ];

        let lines = key_values(&pairs, 2);

        assert_eq!(
            lines,
            vec!["  command  doctor", "  output   first", "           second",]
        );
    }

    #[test]
    fn an_outcome_line_starts_with_its_mark_and_a_detail_is_indented() {
        assert_eq!(outcome(Palette::PLAIN, Mark::Ok, "done"), "✓  done");
        assert_eq!(outcome(Palette::ASCII, Mark::Warn, "careful"), "!! careful");
        assert_eq!(detail(Palette::PLAIN, Mark::Skip, "left"), "  ·  left");
        assert_eq!(strip(&outcome(COLOR, Mark::Fail, "x")), "✗  x");
    }

    #[test]
    fn a_heading_is_bold_on_a_terminal_and_plain_otherwise() {
        assert_eq!(heading("Tools", Palette::PLAIN), "Tools");
        assert_eq!(heading("Tools", COLOR), "\u{1b}[1mTools\u{1b}[0m");
    }

    #[test]
    fn clean_output_drops_blank_lines_stack_frames_and_brace_lines() {
        let text = "NpmInstallFailedError: \n    at catch (/$bunfs/root/chunk.js:2:4171)\n    at Npm.add (/x.js:1:1) {\n  [cause]: Error: The git reference could not be found\n      at Kn (/y.js:8:9324)\n}\n\n";

        let lines = clean_output(text);

        assert_eq!(
            lines,
            vec![
                "NpmInstallFailedError:",
                "  [cause]: Error: The git reference could not be found",
            ]
        );
    }

    #[test]
    fn clean_output_keeps_a_line_that_only_starts_with_at() {
        // "at" is a frame only when it is followed by a call site; a sentence about a path stays.
        assert_eq!(
            clean_output("at least one host is needed"),
            vec!["at least one host is needed"]
        );
    }

    #[test]
    fn an_error_prints_its_headline_then_its_details_and_causes_indented() {
        let err = anyhow!("the root cause").context(
            "`opencode plugin add x` failed with exit 1: NpmInstallFailedError: \n    at catch (/a.js:1:2)\n  [cause]: Error: The git reference could not be found\n      at Kn (/b.js:3:4)",
        );

        let lines = error_lines(&err, Palette::PLAIN);

        assert_eq!(
            lines,
            vec![
                "Error: `opencode plugin add x` failed with exit 1: NpmInstallFailedError:",
                "  [cause]: Error: The git reference could not be found",
                "  caused by: the root cause",
            ]
        );
    }

    #[test]
    fn a_failed_command_prints_its_command_cleaned_output_and_a_hint() {
        let failure = CommandFailed {
            command: "opencode plugin add 'x'".to_string(),
            code: 1,
            output: "NpmInstallFailedError: \n    at catch (/a.js:1:2)\n  [cause]: Error: The git reference could not be found\n      at Kn (/b.js:3:4)\n".to_string(),
        };

        let lines = error_lines(&anyhow::Error::new(failure), Palette::PLAIN);

        assert_eq!(
            lines,
            vec![
                "Error: command failed with exit 1",
                "  command  opencode plugin add 'x'",
                "  output   NpmInstallFailedError:",
                "           [cause]: Error: The git reference could not be found",
                "  hint     the git tag or branch in the command does not exist on GitHub",
            ]
        );
    }

    #[test]
    fn a_long_command_output_is_cut_and_the_rest_is_counted() {
        let output = (1..=9)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let failure = CommandFailed {
            command: "tool run".to_string(),
            code: 2,
            output,
        };

        let lines = error_lines(&anyhow::Error::new(failure), Palette::PLAIN);

        assert!(
            lines.contains(&"           line 6".to_string()),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.ends_with("line 7")),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"           ... 3 more lines".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn an_error_headline_is_red_and_bold_on_a_terminal() {
        let err = anyhow!("boom");

        let lines = error_lines(&err, COLOR);

        assert_eq!(strip(&lines[0]), "Error: boom");
        assert!(lines[0].contains("\u{1b}[31mError:\u{1b}[0m"), "{lines:?}");
    }

    #[test]
    fn the_spinner_frames_are_ascii_without_a_utf8_locale() {
        assert_eq!(Palette::ASCII.spinner_frames(), &["|", "/", "-", "\\"]);
        assert_eq!(Palette::PLAIN.spinner_frames().len(), 10);
    }

    #[test]
    fn a_spinner_on_a_palette_that_cannot_animate_does_nothing() {
        let spinner = Spinner::start("working", Palette::PLAIN);

        spinner.finish();
        assert!(!SPINNING.load(Ordering::SeqCst));
    }
}
