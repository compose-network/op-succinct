use std::fmt;

use owo_colors::OwoColorize;
use regex::Regex;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tracing::{Event, Level};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::{layer::SubscriberExt, EnvFilter, Registry};
use tracing_subscriber::registry::LookupSpan;

struct TokenHighlighter {
    re: Regex,
}

impl TokenHighlighter {
    fn new() -> Self {
        // Match hex strings or integers (avoid matching digits inside hex by prioritizing hex)
        let re = Regex::new(r"(?P<hex>0x[0-9a-fA-F]+)|(?P<num>\b\d+\b)").unwrap();
        Self { re }
    }

    fn apply(&self, input: &str) -> String {
        let mut out = String::with_capacity(input.len() + 32);
        let mut last = 0usize;
        for m in self.re.captures_iter(input) {
            let mat = m.get(0).unwrap();
            out.push_str(&input[last..mat.start()]);
            if m.name("hex").is_some() {
                out.push_str(&mat.as_str().bright_cyan().to_string());
            } else if m.name("num").is_some() {
                out.push_str(&mat.as_str().bright_yellow().to_string());
            }
            last = mat.end();
        }
        out.push_str(&input[last..]);
        out
    }
}

struct FieldCollector<'a> {
    items: Vec<(&'a str, String)>,
}

impl<'a> FieldCollector<'a> {
    fn new() -> Self {
        Self { items: Vec::new() }
    }
}

impl<'a> tracing::field::Visit for FieldCollector<'a> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        self.items.push((field.name(), format!("{:?}", value)));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.items.push((field.name(), value.to_string()));
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.items.push((field.name(), value.to_string()));
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.items.push((field.name(), value.to_string()));
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.items.push((field.name(), value.to_string()));
    }
}

struct ColorEventFormatter {
    highlighter: TokenHighlighter,
}

impl ColorEventFormatter {
    fn new() -> Self {
        Self { highlighter: TokenHighlighter::new() }
    }
}

impl<S, N> FormatEvent<S, N> for ColorEventFormatter
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();

        // Timestamp in dimmed gray
        let ts = OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default();
        write!(writer, "{} ", ts.truecolor(150, 150, 150))?;

        // Level with standard colors
        let level_colored = match *meta.level() {
            Level::ERROR => "ERROR".bright_red().bold().to_string(),
            Level::WARN => "WARN".bright_yellow().bold().to_string(),
            Level::INFO => "INFO".bright_green().bold().to_string(),
            Level::DEBUG => "DEBUG".bright_blue().bold().to_string(),
            Level::TRACE => "TRACE".bright_magenta().bold().to_string(),
        };
        write!(writer, "{} ", level_colored)?;

        // Target
        write!(writer, "{}", meta.target().purple())?;

        // Fields & message
        let mut visitor = FieldCollector::new();
        event.record(&mut visitor);

        // Separate `message` from other fields
        let mut message: Option<String> = None;
        let mut other = Vec::new();
        for (k, v) in visitor.items.into_iter() {
            if k == "message" {
                message = Some(v);
            } else {
                other.push((k, v));
            }
        }

        if let Some(msg) = message {
            let colored = self.highlighter.apply(&msg);
            write!(writer, ": {}", colored)?;
        }

        if !other.is_empty() {
            // space then fields as key=value, with values highlighted
            write!(writer, " ")?;
            for (i, (k, v)) in other.into_iter().enumerate() {
                if i > 0 {
                    write!(writer, ", ")?;
                }
                let v_colored = self.highlighter.apply(&v);
                write!(writer, "{}={}", k.cyan(), v_colored)?;
            }
        }

        // Location
        if let (Some(file), Some(line)) = (meta.file(), meta.line()) {
            write!(
                writer,
                "\n    {} {}:{}",
                "at".truecolor(150, 150, 150),
                file.white(),
                line.white()
            )?;
        }

        writeln!(writer)
    }
}

pub fn init_pretty_color_logs() {
    // Best-effort init; ignore subsequent calls.
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .event_format(ColorEventFormatter::new())
        .with_ansi(true)
        .with_writer(tracing_subscriber::fmt::TestWriter::default());

    let subscriber = Registry::default().with(env_filter).with(fmt_layer);
    let _ = tracing::subscriber::set_global_default(subscriber);
}
