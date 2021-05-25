use crate::{cache::ImageKey, utils::Timer, GlobalState};
use bytes::{Bytes, BytesMut};
use futures::stream::Stream;
use std::error::Error;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

enum BytesAgg {
    Stable(BytesMut),
    Taken,
    Poisoned,
}

impl BytesAgg {
    /// Create a new stable [`BytesAgg`] with the defined capacity
    #[inline]
    fn new(cap: usize) -> Self {
        Self::Stable(BytesMut::with_capacity(cap))
    }
    /// Adds bytes to the aggregator
    ///
    /// Returns whether they were added to the agg or if the agg doesn't exist
    #[inline]
    fn put<T: AsRef<[u8]>>(&mut self, bytes: T) -> bool {
        use bytes::BufMut;
        match self {
            Self::Stable(ref mut agg) => agg.put(bytes.as_ref()),
            _ => return false,
        }
        true
    }
    /// Total length of the aggregator, or 0 if it doesn't exist
    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Stable(ref agg) => agg.len(),
            _ => 0,
        }
    }

    /// Takes the aggregator out of the wrapper if it's still valid
    ///
    /// Will return `None` on Poisoned or Taken
    #[inline]
    fn take(&mut self) -> Option<Bytes> {
        let last = std::mem::replace(self, Self::Taken);
        match last {
            Self::Stable(x) => Some(x.freeze()),
            _ => None,
        }
    }

    /// Sets the aggregator to poisoned (possibly invalid or uncomplete bytes)
    #[inline]
    fn poison(&mut self) {
        *self = Self::Poisoned;
    }
    #[inline]
    fn is_poisoned(&self) -> bool {
        matches!(self, Self::Poisoned)
    }
}

pub(super) type UpstreamStream<E> = dyn Stream<Item = Result<Bytes, E>> + Unpin;

/// A stream to handle cache MISSes by streaming content to the user and saving it until the stream
/// it complete, then saving it to the cache database.
///
/// To break it down: This structure converts a `reqwest` [`Stream`] into an `actix_web` stream,
/// saving all data to an aggregator, then saving the aggregator to cache once the stream is
/// completely done.
pub(super) struct ChunkedUpstreamPoll<E: Error> {
    gs: Arc<GlobalState>,
    upstream: Pin<Box<UpstreamStream<E>>>,
    agg: BytesAgg,
    cache_info: Arc<(ImageKey, mime::Mime)>,
    req_start: Timer,
}

impl<E: Error> ChunkedUpstreamPoll<E> {
    pub(super) fn new(
        gs: &Arc<GlobalState>,
        key: ImageKey,
        mime_type: mime::Mime,
        stream: Box<UpstreamStream<E>>,
        size_hint: usize,
        req_start: Timer,
    ) -> Self {
        Self {
            gs: Arc::clone(gs),
            upstream: Pin::new(stream),
            agg: BytesAgg::new(size_hint),
            cache_info: Arc::new((key, mime_type)),
            req_start,
        }
    }
}

impl<E: Error + 'static> Stream for ChunkedUpstreamPoll<E> {
    type Item = Result<Bytes, actix_web::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // match upstream's stream state and return based on that
        let u = self.upstream.as_mut();
        match u.poll_next(cx) {
            // successful upstream poll
            Poll::Ready(Some(Ok(bytes))) => {
                // copy new bytes to aggregator and then return value
                self.agg.put(&bytes);
                Poll::Ready(Some(Ok(bytes)))
            }
            // unsuccessful upstream poll
            Poll::Ready(Some(Err(e))) => {
                // internal stream had a problem? return error response
                log::error!("error occurred during upstream download: {}", e);
                self.agg.poison();
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

impl<E: Error> Drop for ChunkedUpstreamPoll<E> {
    /// Schedules a tokio task to save the cache aggregator when this value is dropped
    fn drop(&mut self) {
        // take the bytes from the aggreator. if the bytes have already been taken
        // or the bytes have been poisoned (because of an error), this will ret None
        let bytes = match self.agg.take() {
            Some(b) => b,
            None => {
                log::warn!("no byte aggregator found, skipping cache save");
                // if poisoned, then mark as a failed request
                if self.agg.is_poisoned() {
                    self.gs.metrics.failed_requests_total.inc();
                }
                return;
            }
        };

        // spawn a cache save task with tokio
        let bytes_len = bytes.len() as u64;
        let gs = Arc::clone(&self.gs);
        let cache_info = Arc::clone(&self.cache_info);
        tokio::spawn(async move {
            let (key, mime) = cache_info.as_ref();

            let timer = crate::utils::Timer::start();
            gs.cache.save(key, mime.to_string(), bytes).await;
            log::debug!("cache save in {}ms", timer.elapsed());
            gs.metrics
                .cache_save_histo
                .observe(timer.elapsed_secs() as f64);
        });

        // update all metrics
        self.gs
            .metrics
            .miss_request_process_seconds
            .observe(self.req_start.elapsed_secs() as f64);
        self.gs.metrics.bytes_up.inc_by(bytes_len);
        self.gs.metrics.bytes_down.inc_by(bytes_len);
    }
}

/// An error type denoting a problem during the stream of the upstream connection.
///
/// Can be converted into an `actix_web::Error` as it implemented the `ResponseError` trait.
#[derive(Debug)]
struct UpstreamError<E: Error>(E);

impl<E: Error> std::fmt::Display for UpstreamError<E> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "upstream error: {}", self.0)
    }
}
impl<E: Error + 'static> std::error::Error for UpstreamError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
impl<E: Error> actix_web::ResponseError for UpstreamError<E> {
    fn status_code(&self) -> actix_web::http::StatusCode {
        // since this error occurs if upstream has an error, it can be considered a BAD GATEWAY
        // problem/code and not the regular INTERNAL SERVER ERROR
        actix_web::http::StatusCode::BAD_GATEWAY
    }
}
