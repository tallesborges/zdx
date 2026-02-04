//! Debug trace helpers for raw request/response capture.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_util::Stream;

static TRACE_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone)]
pub struct DebugTrace {
    id: String,
    dir: PathBuf,
}

impl DebugTrace {
    pub fn from_env(model: &str, cache_key: Option<&str>) -> Option<Self> {
        let raw = std::env::var("ZDX_DEBUG_TRACE").ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let dir = if trimmed == "1" || trimmed.eq_ignore_ascii_case("true") {
            std::env::temp_dir().join("zdx-trace")
        } else {
            PathBuf::from(trimmed)
        };

        if fs::create_dir_all(&dir).is_err() {
            return None;
        }

        let prefix = cache_key.unwrap_or(model);
        let mut safe = prefix
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>();
        if safe.len() > 32 {
            safe.truncate(32);
        }

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let counter = TRACE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("{}_{}_{}", safe, ts, counter);

        Some(Self { id, dir })
    }

    pub fn write_request(&self, body: &[u8]) {
        if let Ok(mut file) = File::create(self.request_path()) {
            let _ = file.write_all(body);
            let _ = file.flush();
        }
    }

    pub fn response_writer(&self) -> Option<BufWriter<File>> {
        File::create(self.response_path()).ok().map(BufWriter::new)
    }

    fn request_path(&self) -> PathBuf {
        self.dir.join(format!("{}_request.json", self.id))
    }

    fn response_path(&self) -> PathBuf {
        self.dir.join(format!("{}_response.sse", self.id))
    }
}

pub struct TeeStream<S> {
    inner: S,
    sink: Option<BufWriter<File>>,
}

impl<S> TeeStream<S> {
    fn new(inner: S, sink: BufWriter<File>) -> Self {
        Self {
            inner,
            sink: Some(sink),
        }
    }
}

impl<S, E> Stream for TeeStream<S>
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Unpin,
{
    type Item = std::result::Result<Bytes, E>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;

        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                let mut disable = false;
                if let Some(sink) = &mut self.sink
                    && sink.write_all(&bytes).is_err()
                {
                    disable = true;
                }
                if disable {
                    self.sink = None;
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => {
                if let Some(sink) = &mut self.sink {
                    let _ = sink.flush();
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pub enum TraceStream<S> {
    Plain(S),
    Tee(TeeStream<S>),
}

impl<S, E> Stream for TraceStream<S>
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Unpin,
{
    type Item = std::result::Result<Bytes, E>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match &mut *self {
            TraceStream::Plain(inner) => Pin::new(inner).poll_next(cx),
            TraceStream::Tee(inner) => Pin::new(inner).poll_next(cx),
        }
    }
}

pub fn wrap_stream<S, E>(trace: Option<DebugTrace>, stream: S) -> TraceStream<S>
where
    S: Stream<Item = std::result::Result<Bytes, E>> + Unpin,
{
    if let Some(trace) = trace
        && let Some(writer) = trace.response_writer()
    {
        return TraceStream::Tee(TeeStream::new(stream, writer));
    }

    TraceStream::Plain(stream)
}
