//! Shared terminal typography, tables, dates and document rendering.

use std::io::IsTerminal;

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use comfy_table::{Attribute, Cell, CellAlignment, Color, ColumnConstraint, ContentArrangement, Row, Table, Width, presets};
use owo_colors::{OwoColorize, Style as Ansi};
use regex::Regex;
use std::sync::LazyLock;

/// A text role; how it looks is decided in one place for strings and table cells alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Bold,
    Dim,
    Heading,
    Link,
    Red,
    BoldRed,
    Yellow,
    Blue,
    Cyan,
    BoldGreen,
}

impl Tone {
    fn ansi(self) -> Ansi {
        match self {
            Tone::Plain => Ansi::new(),
            Tone::Bold => Ansi::new().bold(),
            Tone::Dim => Ansi::new().dimmed(),
            Tone::Heading => Ansi::new().bold().cyan(),
            Tone::Link => Ansi::new().underline().blue(),
            Tone::Red => Ansi::new().red(),
            Tone::BoldRed => Ansi::new().bold().red(),
            Tone::Yellow => Ansi::new().yellow(),
            Tone::Blue => Ansi::new().blue(),
            Tone::Cyan => Ansi::new().cyan(),
            Tone::BoldGreen => Ansi::new().bold().green(),
        }
    }

    fn cell(self, cell: Cell) -> Cell {
        let (color, attrs): (Option<Color>, &[Attribute]) = match self {
            Tone::Plain => (None, &[]),
            Tone::Bold => (None, &[Attribute::Bold]),
            Tone::Dim => (None, &[Attribute::Dim]),
            Tone::Heading => (Some(Color::Cyan), &[Attribute::Bold]),
            Tone::Link => (Some(Color::Blue), &[Attribute::Underlined]),
            Tone::Red => (Some(Color::DarkRed), &[]),
            Tone::BoldRed => (Some(Color::DarkRed), &[Attribute::Bold]),
            Tone::Yellow => (Some(Color::DarkYellow), &[]),
            Tone::Blue => (Some(Color::DarkBlue), &[]),
            Tone::Cyan => (Some(Color::DarkCyan), &[]),
            Tone::BoldGreen => (Some(Color::DarkGreen), &[Attribute::Bold]),
        };
        let cell = attrs.iter().fold(cell, |c, a| c.add_attribute(*a));
        match color {
            Some(c) => cell.fg(c),
            None => cell,
        }
    }

    pub fn severity(text: &str) -> Tone {
        match text.trim().to_lowercase().as_str() {
            "critical" => Tone::BoldRed,
            "high" => Tone::Red,
            "medium" => Tone::Yellow,
            "low" => Tone::Blue,
            _ => Tone::Plain,
        }
    }

    pub fn status(text: &str) -> Tone {
        match text {
            "LIVE" => Tone::BoldGreen,
            "UPCOMING" => Tone::Cyan,
            "EVALUATING" | "PAUSED" => Tone::Yellow,
            _ => Tone::Dim,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub enabled: bool,
}

impl Style {
    pub fn new(enabled: bool) -> Self {
        Style { enabled }
    }

    /// Colours only on a terminal, and never with `--no-color` or `NO_COLOR`.
    pub fn for_stdout(no_color: bool) -> Self {
        Style::new(!no_color && std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
    }

    pub fn paint(&self, tone: Tone, text: &str) -> String {
        if self.enabled && !text.is_empty() && tone != Tone::Plain { text.style(tone.ansi()).to_string() } else { text.to_string() }
    }

    pub fn bold(&self, t: &str) -> String {
        self.paint(Tone::Bold, t)
    }
    pub fn dim(&self, t: &str) -> String {
        self.paint(Tone::Dim, t)
    }
    pub fn heading(&self, t: &str) -> String {
        self.paint(Tone::Heading, t)
    }
    pub fn link(&self, t: &str) -> String {
        self.paint(Tone::Link, t)
    }
    pub fn severity(&self, t: &str) -> String {
        self.paint(Tone::severity(t), t)
    }
    pub fn status(&self, t: &str) -> String {
        self.paint(Tone::status(t), t)
    }
}

// --- value formatting -------------------------------------------------------------------------

/// `1234567.8` → `1,234,568`.
pub fn thousands(value: f64) -> String {
    let rounded = format!("{:.0}", value.abs());
    let mut out = String::new();
    for (i, ch) in rounded.chars().enumerate() {
        if i > 0 && (rounded.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0.0 && rounded != "0" { format!("-{out}") } else { out }
}

pub fn thousands_int(value: i64) -> String {
    thousands(value as f64)
}

/// Python's `%.3g`: three significant digits, trailing zeros dropped.
fn g3(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let exp = value.abs().log10().floor() as i32;
    let rounded_exp = {
        let r: f64 = format!("{:.2e}", value).split('e').nth(1).and_then(|e| e.parse().ok()).unwrap_or(exp as f64);
        r as i32
    };
    if !(-4..3).contains(&rounded_exp) {
        let text = format!("{:.2e}", value);
        let (mantissa, e) = text.split_once('e').unwrap_or((&text, "0"));
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{mantissa}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    let decimals = (2 - rounded_exp).max(0) as usize;
    let text = format!("{:.*}", decimals, value);
    if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_string() } else { text }
}

/// Dollars: `$1,234`, or `$1.23M` when `compact`; `-` for nothing.
pub fn money(value: Option<f64>, compact: bool) -> String {
    let Some(v) = value.filter(|v| *v > 0.0 && v.is_finite()) else { return "-".into() };
    if compact {
        for (div, suffix) in [(1e6, "M"), (1e3, "K")] {
            if v >= div {
                return format!("${}{suffix}", g3(v / div));
            }
        }
    }
    format!("${}", thousands(v))
}

pub fn parse_date(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(&value.replace(' ', "T")) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(value, fmt) {
            return Some(Utc.from_utc_datetime(&dt));
        }
    }
    let day = NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?;
    Some(Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0)?))
}

fn parse_opt(value: Option<&str>) -> Option<DateTime<Utc>> {
    value.and_then(parse_date)
}

pub fn date(value: Option<&str>) -> String {
    parse_opt(value).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "-".into())
}

pub fn datetime_utc(value: Option<&str>) -> String {
    parse_opt(value).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_else(|| "-".into())
}

/// `in 3d` / `5h ago`.
pub fn relative(value: Option<&str>) -> String {
    let Some(dt) = parse_opt(value) else { return String::new() };
    let delta = (dt - Utc::now()).num_milliseconds() as f64 / 1000.0;
    let span = delta.abs();
    let amount = if span < 3600.0 {
        format!("{:.0}m", span / 60.0)
    } else if span < 86400.0 * 2.0 {
        format!("{:.0}h", span / 3600.0)
    } else {
        format!("{:.0}d", span / 86400.0)
    };
    if delta > 0.0 { format!("in {amount}") } else { format!("{amount} ago") }
}

pub fn duration(seconds: f64) -> String {
    let secs = seconds.max(0.0) as u64;
    format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
}

/// `a, b +3`; `-` when empty.
pub fn joined<I, S>(values: I, limit: usize) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let items: Vec<String> = values.into_iter().map(|v| v.as_ref().to_string()).filter(|v| !v.is_empty()).collect();
    if items.is_empty() {
        return "-".into();
    }
    if limit > 0 && items.len() > limit {
        return format!("{} +{}", items[..limit].join(", "), items.len() - limit);
    }
    items.join(", ")
}

static TRAILING_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]+\n").unwrap());
static BLANK_RUNS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());

/// Light cleanup of the markdown bodies platforms store; keeps them readable as text.
pub fn markdown(text: Option<&str>) -> String {
    let Some(text) = text.filter(|t| !t.trim().is_empty() && t.trim() != "_blank_") else { return String::new() };
    let text = text.replace("\r\n", "\n").replace("\\\n", "\n").replace("&nbsp;", " ");
    let text = TRAILING_SPACE.replace_all(&text, "\n");
    BLANK_RUNS.replace_all(&text, "\n\n").trim().to_string()
}

pub fn indent(text: &str, prefix: &str) -> String {
    text.lines().map(|l| if l.is_empty() { String::new() } else { format!("{prefix}{l}") }).collect::<Vec<_>>().join("\n")
}

// --- tables -----------------------------------------------------------------------------------

/// How a column's cells are coloured.
#[derive(Debug, Clone, Copy)]
pub enum Styler {
    Status,
    Severity,
}

pub fn terminal_width() -> u16 {
    if let Some(cols) = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()) {
        return cols;
    }
    terminal_size::terminal_size().map(|(w, _)| w.0).unwrap_or(160)
}

/// Aligned columns two spaces apart. Only column `flex` shrinks (to no less than 12) when the
/// terminal is too narrow; cut cells end in `…`.
pub fn table(headers: &[&str], rows: &[Vec<String>], style: &Style, flex: usize, stylers: &[(usize, Styler)]) -> String {
    let mut t = Table::new();
    t.load_style(presets::NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_width(terminal_width())
        .set_truncation_indicator("…");
    if style.enabled {
        t.enforce_styling();
    } else {
        t.force_no_tty();
    }
    let header: Vec<Cell> = headers.iter().map(|h| style_cell(Cell::new(h), Tone::Bold, style)).collect();
    t.set_header(header);
    for row in rows {
        let cells: Vec<Cell> = row
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let tone = match stylers.iter().find(|(c, _)| *c == i).map(|(_, s)| s) {
                    Some(Styler::Status) => Tone::status(text),
                    Some(Styler::Severity) => Tone::severity(text),
                    None => Tone::Plain,
                };
                style_cell(Cell::new(text).set_alignment(CellAlignment::Left), tone, style)
            })
            .collect();
        let mut r = Row::from(cells);
        r.max_height(1);
        t.add_row(r);
    }
    let last = headers.len().saturating_sub(1);
    let natural = |i: usize| rows.iter().map(|r| r.get(i).map_or(0, |c| c.chars().count())).max().unwrap_or(0);
    for (i, column) in t.column_iter_mut().enumerate() {
        column.set_padding((0, if i == last { 0 } else { 2 }));
        let constraint = if i == flex {
            let header = headers[i].chars().count();
            let floor = natural(i).max(header).min(header.max(12));
            ColumnConstraint::LowerBoundary(Width::Fixed(floor as u16 + if i == last { 0 } else { 2 }))
        } else {
            ColumnConstraint::ContentWidth
        };
        column.set_constraint(constraint);
    }
    t.lines().map(|l| l.trim_end().to_string()).collect::<Vec<_>>().join("\n")
}

fn style_cell(cell: Cell, tone: Tone, style: &Style) -> Cell {
    if style.enabled { tone.cell(cell) } else { cell }
}

/// Accumulates the text of a detail page.
pub struct Doc {
    pub style: Style,
    parts: Vec<String>,
}

impl Doc {
    pub fn new(style: Style) -> Self {
        Doc { style, parts: Vec::new() }
    }

    pub fn section(&mut self, title: &str) {
        let bar = "─".repeat((title.chars().count() + 4).min(80));
        self.parts.push(format!("\n{}\n{}", self.style.heading(&title.to_uppercase()), self.style.dim(&bar)));
    }

    pub fn sub(&mut self, title: &str) {
        self.parts.push(format!("\n{}", self.style.bold(title)));
    }

    pub fn text(&mut self, body: Option<&str>) {
        let body = markdown(body);
        if !body.is_empty() {
            self.parts.push(indent(&body, "  "));
        }
    }

    pub fn fields<K: AsRef<str>, V: AsRef<str>>(&mut self, pairs: &[(K, V)]) {
        let pairs: Vec<(&str, &str)> =
            pairs.iter().map(|(k, v)| (k.as_ref(), v.as_ref())).filter(|(_, v)| !v.is_empty() && *v != "-").collect();
        let Some(width) = pairs.iter().map(|(k, _)| k.chars().count()).max() else { return };
        let lines: Vec<String> =
            pairs.iter().map(|(k, v)| format!("  {} {v}", self.style.dim(&format!("{:<w$}", format!("{k}:"), w = width + 1)))).collect();
        self.parts.push(lines.join("\n"));
    }

    pub fn raw(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            self.parts.push(text);
        }
    }

    /// A table indented under the current heading; nothing when there are no rows.
    pub fn table(&mut self, headers: &[&str], rows: &[Vec<String>], flex: usize, stylers: &[(usize, Styler)]) {
        if !rows.is_empty() {
            let text = table(headers, rows, &self.style, flex, stylers);
            self.raw(indent(&text, "  "));
        }
    }

    pub fn render(&self) -> String {
        format!("{}\n", self.parts.join("\n").trim_matches('\n'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_formats() {
        assert_eq!(money(Some(1234567.0), false), "$1,234,567");
        assert_eq!(money(Some(1_500_000.0), true), "$1.5M");
        assert_eq!(money(Some(250_000.0), true), "$250K");
        assert_eq!(money(Some(12_345.0), true), "$12.3K");
        assert_eq!(money(Some(0.0), false), "-");
        assert_eq!(money(None, true), "-");
    }

    #[test]
    fn dates() {
        assert_eq!(date(Some("2024-03-05T10:00:00.000Z")), "2024-03-05");
        assert_eq!(date(Some("2024-03-05")), "2024-03-05");
        assert_eq!(datetime_utc(Some("2024-03-05T10:07:00+02:00")), "2024-03-05 08:07 UTC");
        assert_eq!(date(Some("nope")), "-");
        assert_eq!(duration(3723.0), "1h02m");
    }

    #[test]
    fn table_aligns_and_trims() {
        let rows = vec![vec!["a".to_string(), "LIVE".into()], vec!["long name".into(), "FINISHED".into()]];
        let out = table(&["NAME", "STATUS"], &rows, &Style::new(false), 0, &[]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "NAME       STATUS");
        assert_eq!(lines[1], "a          LIVE");
        assert_eq!(lines[2], "long name  FINISHED");
    }

    #[test]
    fn doc_fields_skip_empty() {
        let mut doc = Doc::new(Style::new(false));
        doc.fields(&[("Status", "LIVE"), ("Empty", ""), ("Dash", "-")]);
        assert_eq!(doc.render(), "  Status: LIVE\n");
        assert_eq!(markdown(Some("a  \n\n\n\nb")), "a\n\nb");
        assert_eq!(joined(["a", "b", "c"], 2), "a, b +1");
    }
}

#[cfg(test)]
mod narrow {
    use super::*;

    #[test]
    fn flex_column_truncates_to_fit() {
        unsafe { std::env::set_var("COLUMNS", "40") };
        let rows = vec![vec!["x".repeat(60), "LIVE".into(), "$1,000 USD".into()]];
        let out = table(&["PROJECT", "STATUS", "REWARD"], &rows, &Style::new(false), 0, &[]);
        unsafe { std::env::remove_var("COLUMNS") };
        let line = out.lines().nth(1).unwrap();
        assert!(line.chars().count() <= 40, "{out}");
        assert!(line.contains('…') && line.ends_with("$1,000 USD"), "{out}");
    }
}
