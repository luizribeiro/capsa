use crate::backend::ConsoleStream;
use crate::error::{Error, Result};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::timeout;

pub struct VmConsole {
    stream: Mutex<Option<ConsoleStream>>,
    buffer: Mutex<String>,
}

impl VmConsole {
    pub(crate) fn new(stream: ConsoleStream) -> Self {
        Self {
            stream: Mutex::new(Some(stream)),
            buffer: Mutex::new(String::new()),
        }
    }

    pub async fn split(self) -> Result<(ConsoleReader, ConsoleWriter)> {
        let stream = self.stream.into_inner().ok_or(Error::ConsoleNotEnabled)?;
        let (reader, writer) = tokio::io::split(stream);
        Ok((ConsoleReader { inner: reader }, ConsoleWriter { inner: writer }))
    }

    pub async fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard.as_mut().ok_or(Error::ConsoleNotEnabled)?;
        let n = stream.read(buf).await?;
        Ok(n)
    }

    pub async fn wait_for(&self, pattern: &str) -> Result<String> {
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard.as_mut().ok_or(Error::ConsoleNotEnabled)?;

        let mut buffer_guard = self.buffer.lock().await;
        let mut reader = BufReader::new(stream);
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

    pub async fn wait_for_timeout(&self, pattern: &str, duration: Duration) -> Result<String> {
        match timeout(duration, self.wait_for(pattern)).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout),
        }
    }

    pub async fn wait_for_any(&self, patterns: &[&str]) -> Result<(usize, String)> {
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard.as_mut().ok_or(Error::ConsoleNotEnabled)?;

        let mut buffer_guard = self.buffer.lock().await;
        let mut reader = BufReader::new(stream);
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

    pub async fn read_available(&self) -> Result<String> {
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard.as_mut().ok_or(Error::ConsoleNotEnabled)?;

        let mut buf = [0u8; 4096];
        let mut output = String::new();

        loop {
            match timeout(Duration::from_millis(10), stream.read(&mut buf)).await {
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

    pub async fn write(&self, data: &[u8]) -> Result<()> {
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard.as_mut().ok_or(Error::ConsoleNotEnabled)?;
        stream.write_all(data).await?;
        stream.flush().await?;
        Ok(())
    }

    pub async fn write_str(&self, s: &str) -> Result<()> {
        self.write(s.as_bytes()).await
    }

    pub async fn write_line(&self, s: &str) -> Result<()> {
        let line = format!("{}\n", s);
        self.write(line.as_bytes()).await
    }

    pub async fn send_interrupt(&self) -> Result<()> {
        self.write(&[0x03]).await
    }

    pub async fn send_eof(&self) -> Result<()> {
        self.write(&[0x04]).await
    }

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

    pub async fn run_command(&self, cmd: &str, prompt: &str) -> Result<String> {
        self.write_line(cmd).await?;
        let output = self.wait_for(prompt).await?;
        Ok(output)
    }

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
}

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
