use crate::{cache::ImageKey, utils::Timer, GlobalState};
use bytes::{BufMut, Bytes, BytesMut};
use futures::stream::Stream;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

pub(super) type UpstreamStream = dyn Stream<Item = reqwest::Result<Bytes>> + Unpin;

/// A stream to handle cache MISSes by streaming content to the user and saving it until the stream
/// it complete, then saving it to the cache database.
///
/// To break it down: This structure converts a `reqwest` [`Stream`] into an `actix_web` stream,
/// saving all data to an aggregator, then saving the aggregator to cache once the stream is
/// completely done.
pub(super) struct ChunkedUpstreamPoll {
    gs: Arc<GlobalState>,
    upstream: Pin<Box<UpstreamStream>>,
    agg: BytesMut,
    cache_info: Arc<(ImageKey, mime::Mime)>,
    req_start: Timer,
}

impl ChunkedUpstreamPoll {
    pub(super) fn new(
        gs: &Arc<GlobalState>,
        key: ImageKey,
        mime_type: mime::Mime,
        stream: Box<UpstreamStream>,
        size_hint: usize,
        req_start: Timer,
    ) -> Self {
        Self {
            gs: Arc::clone(gs),
            upstream: Pin::new(stream),
            agg: BytesMut::with_capacity(size_hint),
            cache_info: Arc::new((key, mime_type)),
            req_start,
        }
    }
}

impl Stream for ChunkedUpstreamPoll {
    type Item = Result<Bytes, actix_web::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // match upstream's stream state and return based on that
        let u = self.upstream.as_mut();
        match u.poll_next(cx) {
            // successful upstream poll
            Poll::Ready(Some(Ok(bytes))) => {
                // copy new bytes to aggregator and then return value
                self.agg.put(bytes.as_ref());
                Poll::Ready(Some(Ok(bytes)))
            }
            // unsuccessful upstream poll
            Poll::Ready(Some(Err(e))) => {
                // internal stream had a problem? return error response
                log::warn!("error polling upstream image: {}", e);
                Poll::Ready(Some(Err(UpstreamError(e).into())))
            }

            // stream is done, log and say so
            // cache save will later be completed in Drop
            Poll::Ready(None) => {
                let len = self.agg.len();
                log::debug!("stream complete (total = {}b)", len);

                // complete saying there is no more data
                Poll::Ready(None)
            }

            // waiting for next bytes...
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for ChunkedUpstreamPoll {
    /// Schedules a tokio task to save the cache aggregator when this value is dropped
    fn drop(&mut self) {
        let bytes = std::mem::take(&mut self.agg).freeze();
        let bytes_len = bytes.len() as u64;
        let gs = Arc::clone(&self.gs);
        let cache_info = Arc::clone(&self.cache_info);

        tokio::spawn(async move {
            let (key, mime) = cache_info.as_ref();

            let timer = crate::utils::Timer::start();
            gs.cache.save(key, mime.to_string(), bytes).await;
            log::debug!("cache save in {}ms", timer.elapsed());
            gs.metrics
                .record_cache_latency("save", timer.elapsed_secs() as f64);
        });

        // update all metrics
        self.gs
            .metrics
            .record_request_duration("miss", self.req_start.elapsed_secs() as f64);
        self.gs.metrics.bytes_up.inc_by(bytes_len);
        self.gs.metrics.bytes_down.inc_by(bytes_len);
    }
}

/// An error type denoting a problem during the stream of the upstream connection.
///
/// Can be converted into an `actix_web::Error` as it implemented the `ResponseError` trait.
#[derive(Debug)]
struct UpstreamError(reqwest::Error);

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "upstream error: {}", self.0)
    }
}
impl std::error::Error for UpstreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
impl actix_web::ResponseError for UpstreamError {
    fn status_code(&self) -> actix_web::http::StatusCode {
        // since this error occurs if upstream has an error, it can be considered a BAD GATEWAY
        // problem/code and not the regular INTERNAL SERVER ERROR
        actix_web::http::StatusCode::BAD_GATEWAY
    }
}
