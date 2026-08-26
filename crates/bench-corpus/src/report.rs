//! What a generation run produced, and the sink that counts it.

use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

/// Per-stream totals. Streams are named with the same ids the agent's
/// `layout.toml` uses, so a bench harness can point at the right glob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamReport {
    pub stream: String,
    pub files: u64,
    pub lines: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CorpusReport {
    pub agent: String,
    pub root: String,
    pub seed: u64,
    pub sessions: u64,
    pub tool_ratio: f64,
    pub streams: Vec<StreamReport>,
}

impl CorpusReport {
    pub fn files(&self) -> u64 {
        self.streams.iter().map(|s| s.files).sum()
    }

    pub fn lines(&self) -> u64 {
        self.streams.iter().map(|s| s.lines).sum()
    }

    pub fn bytes(&self) -> u64 {
        self.streams.iter().map(|s| s.bytes).sum()
    }

    /// The human-readable form the CLI prints.
    pub fn table(&self) -> String {
        let mut out = format!(
            "agent={} seed={} sessions={} tool-ratio={:.2}\nroot={}\n\n",
            self.agent, self.seed, self.sessions, self.tool_ratio, self.root
        );
        out.push_str(&format!(
            "{:<24} {:>10} {:>12} {:>14}\n",
            "stream", "files", "lines", "bytes"
        ));
        for stream in &self.streams {
            out.push_str(&format!(
                "{:<24} {:>10} {:>12} {:>14}\n",
                stream.stream, stream.files, stream.lines, stream.bytes
            ));
        }
        out.push_str(&format!(
            "{:<24} {:>10} {:>12} {:>14}  ({})\n",
            "TOTAL",
            self.files(),
            self.lines(),
            self.bytes(),
            human_bytes(self.bytes())
        ));
        out
    }
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A JSONL writer that counts what it wrote. Records are streamed straight
/// through — a long session is millions of lines and must never be buffered
/// as a `Vec<Value>`.
pub struct LineSink<W: Write> {
    inner: W,
    lines: u64,
    bytes: u64,
}

impl<W: Write> LineSink<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            lines: 0,
            bytes: 0,
        }
    }

    pub fn write(&mut self, value: &Value) -> io::Result<()> {
        let text = serde_json::to_string(value).map_err(io::Error::other)?;
        self.inner.write_all(text.as_bytes())?;
        self.inner.write_all(b"\n")?;
        self.lines += 1;
        self.bytes += text.len() as u64 + 1;
        Ok(())
    }

    pub fn lines(&self) -> u64 {
        self.lines
    }

    /// Flush and surrender the counts.
    pub fn finish(mut self) -> io::Result<(u64, u64)> {
        self.inner.flush()?;
        Ok((self.lines, self.bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::{CorpusReport, LineSink, StreamReport, human_bytes};
    use serde_json::json;

    #[test]
    fn sink_counts_lines_and_bytes() {
        let mut buffer = Vec::new();
        let mut sink = LineSink::new(&mut buffer);
        sink.write(&json!({"a": 1})).unwrap();
        sink.write(&json!({"b": 2})).unwrap();
        let (lines, bytes) = sink.finish().unwrap();
        assert_eq!(lines, 2);
        assert_eq!(bytes, buffer.len() as u64);
        assert_eq!(String::from_utf8(buffer).unwrap(), "{\"a\":1}\n{\"b\":2}\n");
    }

    #[test]
    fn report_totals_and_rendering() {
        let report = CorpusReport {
            agent: "claude-code".into(),
            root: "/tmp/bench".into(),
            seed: 42,
            sessions: 2,
            tool_ratio: 0.3,
            streams: vec![StreamReport {
                stream: "transcript".into(),
                files: 2,
                lines: 10,
                bytes: 2048,
            }],
        };
        assert_eq!(
            (report.files(), report.lines(), report.bytes()),
            (2, 10, 2048)
        );
        let table = report.table();
        assert!(table.contains("transcript"));
        assert!(table.contains("TOTAL"));
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(512), "512 B");
    }
}
