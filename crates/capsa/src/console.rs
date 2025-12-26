//! Console interface for interacting with VM serial console.
//!
//! See the [Console Automation guide](crate::guides::console_automation) for
//! patterns and best practices.

use capsa_core::{ConsoleStream, Error, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::timeout;

/// Global command counter for unique markers.
static CMD_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Marker for detecting exec() command completion.
///
/// Uses `X=__DONE_N__` format to avoid false matches when terminal wraps
/// command echo. The `X=` prefix ensures the pattern `\nX=...` matches only
/// the actual output, not the echoed command (which would show `"X=...`).
struct ExecMarker {
    value: String,
}

impl ExecMarker {
    fn new(cmd_id: u64) -> Self {
        Self {
            value: format!("X=__DONE_{}__", cmd_id),
        }
    }

    fn as_printf_suffix(&self) -> String {
        format!("printf '\\n{}\\n'", self.value)
    }

    fn as_pattern(&self) -> String {
        format!("\n{}", self.value)
    }
}

/// High-level interface for interacting with a VM's serial console.
///
/// Obtain via [`VmHandle::console`](crate::VmHandle::console)
/// (requires [`console_enabled`](crate::LinuxVmBuilder::console_enabled)).
///
/// ```rust,no_run
/// # async fn example(console: capsa::VmConsole) -> capsa::Result<()> {
/// console.wait_for("login:").await?;
/// console.write_line("root").await?;
/// # Ok(())
/// # }
/// ```
///
/// See the [Console Automation guide](crate::guides::console_automation) for
/// integration testing patterns.
pub struct VmConsole {
    reader: Mutex<BufReader<tokio::io::ReadHalf<ConsoleStream>>>,
    writer: Mutex<tokio::io::WriteHalf<ConsoleStream>>,
    // TODO: this buffer can grow unboundedly if wait_for() is called without a
    // timeout and the pattern never matches. Consider adding a max size limit
    // or rethinking the buffering strategy.
    buffer: Mutex<String>,
}

impl VmConsole {
    pub(crate) fn new(stream: ConsoleStream) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
        Self {
            reader: Mutex::new(BufReader::new(read_half)),
            writer: Mutex::new(write_half),
            buffer: Mutex::new(String::new()),
        }
    }

    /// Splits the console into separate reader and writer halves.
    ///
    /// This consumes the console and allows concurrent reading and writing.
    pub async fn split(self) -> Result<(ConsoleReader, ConsoleWriter)> {
        let reader = self.reader.into_inner();
        let writer = self.writer.into_inner();
        Ok((
            ConsoleReader {
                inner: reader.into_inner(),
            },
            ConsoleWriter { inner: writer },
        ))
    }

    /// Reads bytes from the console into the provided buffer.
    pub async fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let mut reader = self.reader.lock().await;
        let n = reader.read(buf).await?;
        Ok(n)
    }

    /// Waits for a pattern to appear in the console output.
    ///
    /// Returns all output up to and including the pattern.
    pub async fn wait_for(&self, pattern: &str) -> Result<String> {
        let mut reader = self.reader.lock().await;
        let mut buffer_guard = self.buffer.lock().await;
        let mut line = String::new();

        loop {
            if let Some(pos) = buffer_guard.find(pattern) {
                let end = pos + pattern.len();
                let result = buffer_guard[..end].to_string();
                buffer_guard.drain(..end);
                return Ok(result);
            }

            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Err(Error::PatternNotFound {
                    pattern: pattern.to_string(),
                });
            }
            buffer_guard.push_str(&line);
        }
    }

    /// Waits for a pattern with a timeout.
    ///
    /// Returns [`Error::Timeout`] if the pattern isn't found within the duration.
    pub async fn wait_for_timeout(&self, pattern: &str, duration: Duration) -> Result<String> {
        match timeout(duration, self.wait_for(pattern)).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout),
        }
    }

    /// Waits for any of the given patterns to appear.
    ///
    /// Returns the index of the matched pattern and the output up to it.
    pub async fn wait_for_any(&self, patterns: &[&str]) -> Result<(usize, String)> {
        let mut reader = self.reader.lock().await;
        let mut buffer_guard = self.buffer.lock().await;
        let mut line = String::new();

        loop {
            for (i, pattern) in patterns.iter().enumerate() {
                if let Some(pos) = buffer_guard.find(pattern) {
                    let end = pos + pattern.len();
                    let result = buffer_guard[..end].to_string();
                    buffer_guard.drain(..end);
                    return Ok((i, result));
                }
            }

            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Err(Error::PatternNotFound {
                    pattern: patterns.join(", "),
                });
            }
            buffer_guard.push_str(&line);
        }
    }

    /// Reads all currently available output without blocking.
    pub async fn read_available(&self) -> Result<String> {
        let mut reader = self.reader.lock().await;
        let mut buf = [0u8; 4096];
        let mut output = String::new();

        loop {
            match timeout(Duration::from_millis(10), reader.read(&mut buf)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    output.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => break,
            }
        }

        Ok(output)
    }

    /// Writes raw bytes to the console.
    pub async fn write(&self, data: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().await;
        writer.write_all(data).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Writes a string to the console.
    pub async fn write_str(&self, s: &str) -> Result<()> {
        self.write(s.as_bytes()).await
    }

    /// Writes a string followed by a newline to the console.
    pub async fn write_line(&self, s: &str) -> Result<()> {
        let line = format!("{}\n", s);
        self.write(line.as_bytes()).await
    }

    /// Sends Ctrl+C (interrupt signal) to the console.
    pub async fn send_interrupt(&self) -> Result<()> {
        self.write(&[0x03]).await
    }

    /// Sends Ctrl+D (EOF) to the console.
    pub async fn send_eof(&self) -> Result<()> {
        self.write(&[0x04]).await
    }

    /// Performs a login sequence with username and optional password.
    ///
    /// Waits for "login:", sends the username, optionally waits for "Password:"
    /// and sends the password, then waits for a shell prompt.
    pub async fn login(&self, username: &str, password: Option<&str>) -> Result<()> {
        self.wait_for("login:").await?;
        self.write_line(username).await?;

        if let Some(pwd) = password {
            self.wait_for("Password:").await?;
            self.write_line(pwd).await?;
        }

        self.wait_for_any(&["#", "$", ">"]).await?;
        Ok(())
    }

    /// Runs a command and waits for the prompt to return.
    ///
    /// Returns all output including the command echo and prompt.
    pub async fn run_command(&self, cmd: &str, prompt: &str) -> Result<String> {
        self.write_line(cmd).await?;
        let output = self.wait_for(prompt).await?;
        Ok(output)
    }

    /// Runs a command with a timeout for the prompt to return.
    pub async fn run_command_timeout(
        &self,
        cmd: &str,
        prompt: &str,
        duration: Duration,
    ) -> Result<String> {
        self.write_line(cmd).await?;
        let output = self.wait_for_timeout(prompt, duration).await?;
        Ok(output)
    }

    /// Executes a command and waits for it to complete.
    ///
    /// Uses a unique marker to reliably detect command completion, avoiding
    /// the issue where patterns match in command echoes instead of actual output.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use std::time::Duration;
    /// # async fn example(console: capsa::VmConsole) -> capsa::Result<()> {
    /// let output = console.exec("echo hello", Duration::from_secs(5)).await?;
    /// assert!(output.contains("hello"));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn exec(&self, cmd: &str, timeout_duration: Duration) -> Result<String> {
        let cmd_id = CMD_COUNTER.fetch_add(1, Ordering::Relaxed);
        let marker = ExecMarker::new(cmd_id);

        let separator = if cmd.trim_end().ends_with('&') {
            ""
        } else {
            " ;"
        };

        self.write_line(&format!(
            "{}{} {}",
            cmd,
            separator,
            marker.as_printf_suffix()
        ))
        .await?;

        self.wait_for_timeout(&marker.as_pattern(), timeout_duration)
            .await
    }
}

/// Read half of a split console, implementing [`AsyncRead`].
pub struct ConsoleReader {
    inner: tokio::io::ReadHalf<ConsoleStream>,
}

impl AsyncRead for ConsoleReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

/// Write half of a split console, implementing [`AsyncWrite`].
pub struct ConsoleWriter {
    inner: tokio::io::WriteHalf<ConsoleStream>,
}

impl AsyncWrite for ConsoleWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
