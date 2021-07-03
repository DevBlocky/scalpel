//! HTTP responder and handler for interacting with the dynamic image cache.
//!
//! Module will handle HIT or MISS images by calling DB. On HIT, will simply stream the image, and
//! on MISS, will download the image from upstream, save it, then stream it.

use super::chunked::{ChunkedUpstreamPoll, UpstreamStream};
use crate::backend::Backend;
use crate::cache::ImageKey;
use crate::utils::Timer;
use crate::GlobalState;
use actix_web::{
    dev::BodyEncoding,
    http::{
        self,
        header::{self, HttpDate},
        StatusCode,
    },
    HttpRequest, HttpResponse,
};
use lazy_static::lazy_static;
use std::sync::Arc;
use std::time;

/// Generates an [`HttpResponse`] by querying the cache and either returning HIT data or polling
/// upstream, proxying, and saving the result on MISS.
pub(super) async fn response_from_cache(
    uid: &str, // unique-id that represents the request (used for logging)
    req: &HttpRequest,
    gs: &Arc<GlobalState>,
    key: ImageKey,
    req_start: Timer,
) -> HttpResponse {
    // attempt to load image from cache (timing response times)
    let cache_hit = {
        let timer = Timer::start();
        let cache_hit = gs.cache.load(&key).await;
        log::debug!("({}) cache lookup in {}", uid, timer);
        gs.metrics
            .cache_load_seconds
            .observe(timer.elapsed_secs() as f64);
        cache_hit
    };

    if let Some(cache_hit) = cache_hit {
        // found in cache, aka HIT
        let res = handle_cache_hit(uid, gs, req, cache_hit);
        // NOTE: recording metrics here because handle_cache_hit doesn't
        // contain logic for failure
        gs.metrics
            .hit_request_process_seconds
            .observe(req_start.elapsed_secs() as f64);
        gs.metrics.hit_requests_total.inc();
        res
    } else {
        // the result was not found in cache, aka MISS
        // NOTE: metrics are handled in chunked.rs
        handle_cache_miss(uid, gs, key, req_start).await
    }
}

/* CACHE HIT HANDLER LOGIC BELOW */

/// Returns whether the browser has the resource already cached locally.
///
/// This is solely based on the `If-None-Match` header the client provides and the internally
/// computed strong `ETag`. This will always return `false` if the provided `If-None-Match` is `*`.
fn is_browser_cached(req: &HttpRequest, etag: &header::EntityTag) -> bool {
    use actix_web::HttpMessage;

    match req.get_header::<header::IfNoneMatch>() {
        Some(header::IfNoneMatch::Items(ref items)) => items.iter().any(|x| etag.strong_eq(x)),
        _ => false,
    }
}

/// Handles a cache HIT, returning an HttpResponse that represents that data of the cached image
///
/// Sends the bytes of the cached image to the client unless the client has already proved that
/// they have the image cached locally. Will also set gzip (if enabled by client) and provide
/// necessary headers (like `ETag` and `Vary`)
fn handle_cache_hit(
    uid: &str,
    gs: &Arc<GlobalState>,
    req: &HttpRequest,
    image: crate::cache::ImageEntry,
) -> HttpResponse {
    // check whether the browser already has the image cached locally
    let etag = header::EntityTag::strong(image.get_checksum_hex());
    let is_client_cached = is_browser_cached(req, &etag);

    // create response object with headers that should be in every response
    let mut res = HttpResponse::build(StatusCode::OK);
    res.append_header(header::ContentType(image.get_mime()))
        .append_header(header::ETag(etag))
        .append_header(("Vary", "Accept-Encoding"))
        .append_header(("X-Cache", "HIT"));

    // if the image is already cached in the browser, then we can just return the associated code
    // telling the browser that it doesn't need to download anything
    if is_client_cached {
        log::debug!("({}) browser cache HIT", uid);
        return res.status(StatusCode::NOT_MODIFIED).finish();
    }

    // set the encoding to gzip if it is enabled by the client and the browser supports/accepts it
    if gs.config.gzip_compress {
        if let Some(accept) = req
            .headers()
            .get(&header::ACCEPT_ENCODING)
            .and_then(|h| h.to_str().ok())
        {
            if accept.contains("gzip") {
                res.encoding(http::ContentEncoding::Gzip);
            }
        }
    }

    // stream the data to the client
    let bytes = image.get_bytes();
    gs.metrics.bytes_up.inc_by(bytes.len() as u64);
    res.body(bytes)
}

/* CACHE MISS HANDLER LOGIC BELOW */

lazy_static! {
    /// Lazily loaded HTTP Client that will be used for polling upstream for images.
    static ref HTTP_CLIENT: reqwest::Client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        // if a request exceeds 5 minutes, that's big yikes
        .timeout(time::Duration::from_secs(300))
        .build()
        .expect("misconfigured lazy_static http client");
}

/// A Unit Struct that represents an error where the upstream url is unset in the backend
///
/// This error is almost certain to never be constructed as Backend sets the image server url
/// before the web server starts, however in case this logic changes in the future this is here.
#[derive(Debug)]
struct NoUpstreamError;
impl std::fmt::Display for NoUpstreamError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "no upstream URL available (how did we get here?)")
    }
}
impl std::error::Error for NoUpstreamError {}

/// A structure that includes all of the data needed to stream a response back to the client.
struct UpstreamResponse {
    stream: Box<UpstreamStream<reqwest::Error>>,
    size_hint: Option<usize>,

    status: StatusCode,
    content_type: mime::Mime,
    last_modified: HttpDate,
}

/// Starts a connection with the upstream server with the request resource.
///
/// This will return the required headers to stream back to the client as well as the stream of
/// bytes representing the body of the request.
///
/// This function will return on first byte received
async fn start_poll_upstream(
    backend: &Backend,
    key: &ImageKey,
) -> Result<UpstreamResponse, Box<dyn std::error::Error>> {
    use std::str::FromStr;

    let url = {
        let info = backend.ping_info.load();
        let upstream_url = Option::as_ref(&info)
            .map(|x| &x.upstream_url)
            .ok_or(NoUpstreamError)?;

        url::Url::options()
            .base_url(Some(&upstream_url))
            .parse(&format!(
                "/{}/{}/{}",
                key.archive_name(),
                key.chapter(),
                key.image()
            ))?
    };

    let res = HTTP_CLIENT.get(url).send().await?;
    let status = res.status();

    // get the mime type from upstream, or try to guess
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.parse::<mime::Mime>().ok())
        // if this entire process fails for whatever reason, then just assume that the image is a
        // PNG and move on with life
        .unwrap_or(mime::IMAGE_PNG);

    // get the last modified date from upstream, or else just use now
    let last_modified = res
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|x| x.to_str().ok())
        .and_then(|x| HttpDate::from_str(x).ok())
        .unwrap_or_else(|| HttpDate::from(time::SystemTime::now()));

    let size_hint = res.content_length().map(|x| x as usize);
    Ok(UpstreamResponse {
        stream: Box::new(res.bytes_stream()),
        size_hint,

        status,
        content_type,
        last_modified,
    })
}

/// Handles a cache MISS by requesting the image from the upstream and streaming the image to the
/// user using [`ChunkedUpstreamPoll`]
///
/// If polling from upstream fails, then it will automatically return 502 BAD GATEWAY to the user
/// with the error as the body.
async fn handle_cache_miss(
    uid: &str,
    gs: &Arc<GlobalState>,
    key: ImageKey,
    req_start: Timer,
) -> HttpResponse {
    // poll upstream, finding the total time of the request
    let res = {
        let timer = Timer::start();
        let res = start_poll_upstream(&gs.backend, &key).await;
        log::debug!("({}) upstream TTFB: {}", uid, timer);
        gs.metrics
            .upstream_ttfb_seconds
            .observe(timer.elapsed_secs() as f64);
        res
    };
    // handle any errors that happen with res
    let res = match res {
        Ok(res) => res,
        Err(e) => {
            log::error!("unexpected upstream error before download ({})", e);
            gs.metrics.failed_requests_total.inc();
            return HttpResponse::BadGateway().body("unexpected upstream response");
        }
    };

    // error handling for the status, make sure it's 200 OK
    match res.status {
        StatusCode::OK => {}
        StatusCode::NOT_FOUND => return HttpResponse::NotFound().finish(),
        status => {
            log::error!("unexpected upstream status ({})", status);
            gs.metrics.failed_requests_total.inc();
            return HttpResponse::BadGateway()
                .body(format!("invalid upstream status code: {}", status));
        }
    }

    // create the chunk stream
    let chunked = ChunkedUpstreamPoll::new(
        gs,
        key,
        res.content_type.clone(),
        res.stream,
        res.size_hint.unwrap_or(0),
        req_start,
    );

    // proxy the image to the client
    let mut http_res = HttpResponse::Ok();
    http_res
        .append_header(header::ContentType(res.content_type))
        .append_header(header::LastModified(res.last_modified))
        .append_header(("X-Cache", "MISS"));
    http_res.streaming(chunked)
}
