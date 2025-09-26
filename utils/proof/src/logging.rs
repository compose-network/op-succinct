use std::io::{self, Write};

use tracing::{event, Level};

/// Bridges SP1 guest output streams into host tracing.
pub struct GuestLogBridge {
    buffer: Vec<u8>,
    level: Level,
    target: &'static str,
}

impl GuestLogBridge {
    pub fn new(level: Level, target: &'static str) -> Self {
        Self { buffer: Vec::with_capacity(256), level, target }
    }

    fn log_line(&self, line: &str) {
        match (self.target, self.level) {
            ("sp1::stdout", Level::ERROR) => {
                event!(target: "sp1::stdout", Level::ERROR, message = %line)
            }
            ("sp1::stdout", Level::WARN) => {
                event!(target: "sp1::stdout", Level::WARN, message = %line)
            }
            ("sp1::stdout", Level::INFO) => {
                event!(target: "sp1::stdout", Level::INFO, message = %line)
            }
            ("sp1::stdout", Level::DEBUG) => {
                event!(target: "sp1::stdout", Level::DEBUG, message = %line)
            }
            ("sp1::stdout", Level::TRACE) => {
                event!(target: "sp1::stdout", Level::TRACE, message = %line)
            }
            ("sp1::stderr", Level::ERROR) => {
                event!(target: "sp1::stderr", Level::ERROR, message = %line)
            }
            ("sp1::stderr", Level::WARN) => {
                event!(target: "sp1::stderr", Level::WARN, message = %line)
            }
            ("sp1::stderr", Level::INFO) => {
                event!(target: "sp1::stderr", Level::INFO, message = %line)
            }
            ("sp1::stderr", Level::DEBUG) => {
                event!(target: "sp1::stderr", Level::DEBUG, message = %line)
            }
            ("sp1::stderr", Level::TRACE) => {
                event!(target: "sp1::stderr", Level::TRACE, message = %line)
            }
            (_, Level::ERROR) => event!(Level::ERROR, guest_stream = self.target, message = %line),
            (_, Level::WARN) => event!(Level::WARN, guest_stream = self.target, message = %line),
            (_, Level::INFO) => event!(Level::INFO, guest_stream = self.target, message = %line),
            (_, Level::DEBUG) => event!(Level::DEBUG, guest_stream = self.target, message = %line),
            (_, Level::TRACE) => event!(Level::TRACE, guest_stream = self.target, message = %line),
        }
    }

    fn drain_line(&mut self, end: usize) {
        let mut line = self.buffer.drain(..end).collect::<Vec<_>>();
        if let Some(&b'\n') = line.last() {
            line.pop();
        }
        if let Some(&b'\r') = line.last() {
            line.pop();
        }
        if line.is_empty() {
            return;
        }

        let text = String::from_utf8_lossy(&line);
        self.log_line(text.trim_end());
    }

    fn flush_remaining(&mut self) {
        if !self.buffer.is_empty() {
            let len = self.buffer.len();
            self.drain_line(len);
        }
    }
}

impl Write for GuestLogBridge {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);

        while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
            let end = pos + 1;
            self.drain_line(end);
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_remaining();
        Ok(())
    }
}

impl Drop for GuestLogBridge {
    fn drop(&mut self) {
        self.flush_remaining();
    }
}
