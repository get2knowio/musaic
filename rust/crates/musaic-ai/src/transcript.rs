//! The failure-excerpt breadcrumbs: `ai._TranscriptRecorder` over a
//! provider's event stream, its digests (`_truncate`,
//! `_summarize_tool_input`, `_summarize_tool_result`, `_summarize_tool_args`)
//! and `services.worker._format_transcript`.

use std::sync::{Arc, Mutex};

use crate::provider::ProviderEvent;
use crate::pyjson::{Py, loads, py_head, py_len, py_strip, py_tail};

/// `_TRANSCRIPT_BLOCK_MAX_CHARS`.
pub const BLOCK_MAX_CHARS: usize = 600;
/// The `max_bytes` the worker passes to `_format_transcript`.
pub const EXCERPT_MAX_BYTES: usize = 16_000;

/// `_truncate(s, limit=...)`: stripped, newlines to spaces, and when longer
/// than `limit` code points the head and tail around a marker.
pub fn truncate_to(s: &str, limit: usize) -> String {
    let s = py_strip(s).replace('\n', " ");
    let n = py_len(&s);
    if n <= limit {
        return s;
    }
    let head = (limit / 2).saturating_sub(10);
    let tail = limit - head - 10;
    format!(
        "{} \u{2026} [{} ch] \u{2026} {}",
        py_head(&s, head),
        n - limit,
        py_tail(&s, tail)
    )
}

/// `_truncate(s)`.
pub fn truncate(s: &str) -> String {
    truncate_to(s, BLOCK_MAX_CHARS)
}

/// `_summarize_tool_input(tool_input)`.
pub fn summarize_tool_input(input: &Py) -> String {
    let Py::Dict(_) = input else {
        return truncate(&input.py_str());
    };
    for key in ["url", "query", "q", "command", "file_path", "path"] {
        if let Some(v) = input.get(key) {
            return format!("{key}={}", truncate(&v.py_repr()));
        }
    }
    truncate(&input.dumps_unicode())
}

/// `_summarize_tool_result(content)`.
pub fn summarize_tool_result(content: &Py) -> String {
    match content {
        Py::Str(s) => truncate(s),
        Py::List(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|e| match e {
                    Py::Dict(_) => match e.get("type") {
                        Some(Py::Str(t)) if t == "text" => match e.get("text") {
                            Some(x) if x.truthy() => x.py_str(),
                            _ => String::new(),
                        },
                        Some(t) => format!("<{}>", t.py_str()),
                        None => "<block>".to_string(),
                    },
                    other => other.py_str(),
                })
                .collect();
            let t = truncate(py_strip(&parts.join(" ")));
            if t.is_empty() { "(empty)".into() } else { t }
        }
        other => truncate(&other.py_str()),
    }
}

/// `_summarize_tool_args(arguments_preview)`: parsed as JSON when it parses
/// (it can be cut mid-stream), else the raw text truncated.
pub fn summarize_tool_args(preview: &str) -> String {
    match loads(preview) {
        Ok(v) => summarize_tool_input(&v),
        Err(_) => truncate(preview),
    }
}

/// The per-job transcript buffer (`ai.install_transcript_buffer()`): every
/// provider call of one job appends to it.
#[derive(Debug, Clone, Default)]
pub struct Buffer(Arc<Mutex<Vec<String>>>);

impl Buffer {
    pub fn new() -> Buffer {
        Buffer::default()
    }

    /// `_record_transcript(piece)`: empty pieces are dropped.
    pub fn record(&self, piece: String) {
        if !piece.is_empty() {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(piece);
        }
    }

    /// The lines recorded so far.
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// `_TranscriptRecorder`: one per provider call.
#[derive(Debug)]
pub struct Recorder {
    buffer: Buffer,
    text: Vec<String>,
    reasoning: Vec<String>,
}

impl Recorder {
    pub fn new(buffer: Buffer) -> Recorder {
        Recorder {
            buffer,
            text: Vec::new(),
            reasoning: Vec::new(),
        }
    }

    /// `feed(event)`.
    pub fn feed(&mut self, event: &ProviderEvent) {
        match event {
            ProviderEvent::Text(t) => self.text.push(t.clone()),
            ProviderEvent::Reasoning(t) => self.reasoning.push(t.clone()),
            ProviderEvent::ToolCallStart {
                tool_name,
                arguments_preview,
            } => {
                self.flush();
                self.buffer.record(format!(
                    "\u{2192} {tool_name}  {}",
                    summarize_tool_args(arguments_preview)
                ));
            }
            ProviderEvent::ToolCallResult { output, is_error } => {
                self.flush();
                let arrow = if *is_error {
                    "\u{2190} ERROR"
                } else {
                    "\u{2190}"
                };
                self.buffer
                    .record(format!("{arrow}  {}", summarize_tool_result(output)));
            }
            ProviderEvent::TurnComplete { text } => {
                self.flush();
                if !text.is_empty() {
                    self.buffer.record(format!("[final] {}", truncate(text)));
                }
            }
        }
    }

    fn flush(&mut self) {
        if !self.reasoning.is_empty() {
            let t = py_strip(&self.reasoning.concat()).to_string();
            if !t.is_empty() {
                self.buffer.record(format!("\u{1f4ad} {}", truncate(&t)));
            }
            self.reasoning.clear();
        }
        if !self.text.is_empty() {
            let t = py_strip(&self.text.concat()).to_string();
            if !t.is_empty() {
                self.buffer.record(truncate(&t));
            }
            self.text.clear();
        }
    }

    /// `finish()`: flush what no boundary flushed.
    pub fn finish(&mut self) {
        self.flush();
    }
}

/// `services.worker._format_transcript(buffer, max_bytes=...)`: non-blank
/// pieces, stripped, one per line; the UTF-8 tail of `max_bytes` with a
/// marker when longer (a character cut at the start decodes to U+FFFD, as
/// `errors="replace"`). `None` when nothing is left.
pub fn format_transcript(buffer: &[String], max_bytes: usize) -> Option<String> {
    let pieces: Vec<&str> = buffer
        .iter()
        .map(|p| py_strip(p))
        .filter(|p| !p.is_empty())
        .collect();
    if pieces.is_empty() {
        return None;
    }
    let joined = pieces.join("\n");
    let bytes = joined.as_bytes();
    if bytes.len() <= max_bytes {
        return Some(joined);
    }
    let tail = String::from_utf8_lossy(&bytes[bytes.len() - max_bytes..]).into_owned();
    let omitted = bytes.len() - max_bytes;
    Some(format!(
        "\u{2026} [{omitted} bytes truncated] \u{2026}\n\n{tail}"
    ))
}
