//! Client side of the orchestrator service protocol, shared by the CLI and
//! the bot.

use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::protocol::{Reply, Request, socket_path};
use crate::core::workers::WorkerSnapshot;

/// Longest wait for a one-shot reply.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct OrchestratorClient {
    socket: PathBuf,
}

impl Default for OrchestratorClient {
    fn default() -> Self {
        Self::new()
    }
}

impl OrchestratorClient {
    /// A client for the service of the current `ZDX_HOME`.
    #[must_use]
    pub fn new() -> Self {
        Self::at(socket_path())
    }

    #[must_use]
    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }

    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    fn connect_error(&self, err: std::io::Error) -> anyhow::Error {
        if matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        ) {
            anyhow::anyhow!(
                "the orchestrator service is not running (no listener at {}); install it once with `zdx service install orchestrator`, then start it with `zdx service start orchestrator` (or run `zdx orchestrator serve` in the foreground)",
                self.socket.display()
            )
        } else {
            anyhow::Error::new(err).context(format!("connect to {}", self.socket.display()))
        }
    }

    /// Sends one request and waits for its reply (blocking).
    ///
    /// # Errors
    /// Returns an error when the service is not reachable or the reply is
    /// unreadable. A [`Reply::Error`] is returned as a value.
    pub fn request(&self, request: &Request) -> Result<Reply> {
        let mut stream =
            StdUnixStream::connect(&self.socket).map_err(|err| self.connect_error(err))?;
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        stream.set_write_timeout(Some(REPLY_TIMEOUT))?;
        let mut line = serde_json::to_vec(request).context("encode request")?;
        line.push(b'\n');
        stream.write_all(&line).context("send request")?;
        let mut reply = String::new();
        std::io::BufReader::new(&stream)
            .read_line(&mut reply)
            .context("read reply")?;
        decode(&reply)
    }

    /// Sends one request and waits for its reply.
    ///
    /// # Errors
    /// Same as [`Self::request`].
    pub async fn call(&self, request: &Request) -> Result<Reply> {
        let mut stream = self.open(request).await?;
        tokio::time::timeout(REPLY_TIMEOUT, stream.next())
            .await
            .context("orchestrator service did not reply in time")??
            .context("orchestrator service closed the connection without a reply")
    }

    /// Sends a request whose reply is a stream (`Submit` with a follow mode,
    /// `Subscribe`).
    ///
    /// # Errors
    /// Returns an error when the service is not reachable.
    pub async fn open(&self, request: &Request) -> Result<ReplyStream> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|err| self.connect_error(err))?;
        let (read, mut write) = stream.into_split();
        let mut line = serde_json::to_vec(request).context("encode request")?;
        line.push(b'\n');
        write.write_all(&line).await.context("send request")?;
        write.flush().await.context("send request")?;
        Ok(ReplyStream {
            lines: BufReader::new(read).lines(),
            _write: write,
        })
    }

    /// Managed workers, all or of one owner. Empty when the service is not
    /// reachable: callers show persisted lineage then.
    pub async fn workers(&self, owner_thread_id: Option<&str>) -> Vec<WorkerSnapshot> {
        let request = Request::Workers {
            owner_thread_id: owner_thread_id.map(str::to_string),
        };
        workers_of(self.call(&request).await)
    }

    /// Blocking form of [`Self::workers`].
    #[must_use]
    pub fn workers_blocking(&self, owner_thread_id: Option<&str>) -> Vec<WorkerSnapshot> {
        let request = Request::Workers {
            owner_thread_id: owner_thread_id.map(str::to_string),
        };
        workers_of(self.request(&request))
    }

    /// One managed worker; `None` when unmanaged or the service is down.
    pub async fn worker(&self, thread_id: &str) -> Option<WorkerSnapshot> {
        let request = Request::Worker {
            thread_id: thread_id.to_string(),
        };
        match self.call(&request).await {
            Ok(Reply::Worker { worker }) => worker,
            _ => None,
        }
    }
}

fn workers_of(reply: Result<Reply>) -> Vec<WorkerSnapshot> {
    match reply {
        Ok(Reply::Workers { workers }) => workers,
        Ok(other) => {
            tracing::debug!(?other, "Unexpected reply to a worker listing");
            Vec::new()
        }
        Err(err) => {
            tracing::debug!(err = %format!("{err:#}"), "Worker listing unavailable");
            Vec::new()
        }
    }
}

fn decode(line: &str) -> Result<Reply> {
    if line.trim().is_empty() {
        bail!("orchestrator service closed the connection without a reply");
    }
    serde_json::from_str(line).context("decode orchestrator reply")
}

/// Replies of one streaming connection. Dropping it closes the connection.
pub struct ReplyStream {
    lines: tokio::io::Lines<BufReader<OwnedReadHalf>>,
    _write: OwnedWriteHalf,
}

impl ReplyStream {
    /// The next reply; `None` once the service closes the connection.
    ///
    /// # Errors
    /// Returns an error when reading fails or a line does not decode.
    pub async fn next(&mut self) -> Result<Option<Reply>> {
        match self.lines.next_line().await.context("read reply")? {
            Some(line) => decode(&line).map(Some),
            None => Ok(None),
        }
    }
}

/// Turns a [`Reply::Error`] into an error.
///
/// # Errors
/// Returns the service's error message.
pub fn ok(reply: Reply) -> Result<Reply> {
    match reply {
        Reply::Error { message, .. } => bail!("{message}"),
        other => Ok(other),
    }
}
