use bytes::{BufMut, Bytes, BytesMut};
use futures::stream::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(super) type UpstreamStream = dyn Stream<Item = reqwest::Result<Bytes>> + Unpin;
pub(super) struct ChunkedUpstreamPoll {
    upstream: Pin<Box<UpstreamStream>>,
    agg: BytesMut,
}

impl ChunkedUpstreamPoll {
    pub(super) fn new(stream: Box<UpstreamStream>, size_hint: usize) -> Self {
        Self {
            upstream: Pin::new(stream),
            agg: BytesMut::with_capacity(size_hint),
        }
    }
}

impl Stream for ChunkedUpstreamPoll {
    type Item = Result<Bytes, actix_web::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let u = self.upstream.as_mut();

        // poll for upstream stream status
        match u.poll_next(cx) {
            // successful upstream poll
            Poll::Ready(Some(Ok(bytes))) => {
                self.agg.put(&bytes as &[u8]);
                Poll::Ready(Some(Ok(bytes)))
            }
            // unsuccessful upstream poll
            Poll::Ready(Some(Err(err))) => {
                log::warn!("an error occurred while streaming upstream result: {}", err);
                Poll::Ready(Some(Err(UpstreamError(err).into())))
            },

            // stream is done, so we should save the cache
            // TODO: actually save cache
            Poll::Ready(None) => {
                log::debug!("stream complete (total = {}b)", self.agg.len());
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// An error type denoting a problem during the stream of the upstream connection.
///
/// Can be converted into an `actix_web::Error` as it implemented the `ResponseError` trait.
#[derive(Debug)]
struct UpstreamError(reqwest::Error);

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "{}", self.0)
    }
}
impl std::error::Error for UpstreamError {}
impl actix_web::ResponseError for UpstreamError {
    fn status_code(&self) -> actix_web::http::StatusCode {
        actix_web::http::StatusCode::BAD_GATEWAY
    }
}
