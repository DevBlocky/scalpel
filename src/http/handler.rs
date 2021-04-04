//! HTTP responder and handler for interacting with the dynamic image cache.
//!
//! Module will handle HIT or MISS images by calling DB. On HIT, will simply stream the image, and
//! on MISS, will download the image from upstream, save it, then stream it.

use crate::backend::Backend;
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
use bytes::Bytes;
use std::sync::Arc;
use std::time;

/// Generates an [`HttpResponse`] by querying the cache and either returning HIT data or polling
/// upstream, proxying, and saving the result on MISS.
pub(super) async fn response_from_cache(
    uid: &str, // unique-id that represents the request (used for logging)
    req: &HttpRequest,
    gs: &Arc<GlobalState>,
    (chap_hash, image, saver): (&str, &str, bool),
) -> HttpResponse {
    // attempt to load image from cache (timing response times)
    let cache_hit = {
        let timer = Timer::start();
        let cache_hit = gs.cache.load(chap_hash, image, saver).await;
        log::debug!("({}) cache lookup in {}ms", uid, timer.elapsed());
        cache_hit
    };

    if let Some(cache_hit) = cache_hit {
        // found in cache, aka HIT
        handle_cache_hit(req, cache_hit, gs.config.gzip_compress)
    } else {
        // the result was not found in cache, aka MISS
        handle_cache_miss(req, gs, (chap_hash, image, saver)).await
    }
}

/// Checks whether the browser has the current file version cached
///
/// Uses the `If-Modified-Since` header and save time for cache entry (if both exist) to find whether the
/// cached version has been modified since the browser has received the data.
///
/// `Ok(true)` equates to browser cache being valid, `Ok(false)` means that the cache has been
/// modified since the browser had originally cached it, and `Err(())` equates to a problem
/// checking the header or parsing the date
fn is_browser_cached(req: &HttpRequest, save_time: &time::SystemTime) -> Result<bool, ()> {
    use std::str::FromStr;

    // try to convert the If-Modified-Since header into SystemTime
    let mod_since: time::SystemTime = req
        .headers()
        .get(&http::header::IF_MODIFIED_SINCE)
        .ok_or(())
        .and_then(|h| h.to_str().map_err(|_| ()))
        .and_then(|h| HttpDate::from_str(h).map_err(|_| ()))?
        .into();

    // find whether the cache has been modified since the browser has cached it
    // NOTE: this must be a check between seconds because header doesn't store as much precision in
    // the modified date as the system file.
    match (
        mod_since.duration_since(time::UNIX_EPOCH),
        save_time.duration_since(time::UNIX_EPOCH),
    ) {
        (Ok(mod_since), Ok(save_time)) => Ok(mod_since.as_secs() >= save_time.as_secs()),
        _ => Ok(false),
    }
}

/// Guesses the mime type of the request based on the request path
///
/// Uses the request path and the extension of the last portion of the path to guess the mime type
/// of the request using `mime-guess`. If it can't determine the mime type, it will default to
/// image/png
fn mime_from_request(req: &HttpRequest) -> mime_guess::Mime {
    mime_guess::from_path(req.path())
        .first()
        .unwrap_or_else(|| mime_guess::mime::IMAGE_PNG)
}

/// Handles a cache HIT, returning an HttpResponse that represents that data of the cached image
///
/// Sends the bytes of the cached image to the client unless the client has already proved that
/// they have the image cached locally. Will also set gzip (if enabled by client) and provide
/// necessary headers (like `Last-Modified`)
fn handle_cache_hit(
    req: &HttpRequest,
    (image_bytes, save_time): crate::cache::ImageEntry,
    gzip: bool,
) -> HttpResponse {
    // get the MIME type from the path
    let mime = mime_from_request(req);

    // create response object with headers that should be in every response
    let mut res = HttpResponse::build(StatusCode::OK);
    res.append_header(header::LastModified(HttpDate::from(save_time)))
        .append_header(header::ContentType(mime))
        .append_header(("X-Cache", "HIT"));

    // if the image is already cached in the browser, then we can just return the associated code
    // telling the browser that it doesn't need to download anything
    match is_browser_cached(req, &save_time) {
        Ok(true) => return res.status(StatusCode::NOT_MODIFIED).finish(),
        _ => {}
    }

    // set the encoding to gzip if it is enabled by the client and the browser supports/accepts it
    if gzip {
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
    res
        .append_header(("Vary", "Accept-Encoding"))
        .body(image_bytes)
}

/// A Unit Struct that represents an error where the upstream url is unset in the backend
///
/// This error is almost certain to never be constructed as Backend sets the image server url
/// before the web server starts, however in case this logic changes in the future this is here.
#[derive(Debug)]
struct NoUpstreamError;
impl std::fmt::Display for NoUpstreamError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(fmt, "")
    }
}
impl std::error::Error for NoUpstreamError {}

/// A structure that includes bytes of the upstream response and headers that should be forwarded
/// for the proxy
struct UpstreamResponse {
    status: StatusCode,
    content_type: mime_guess::Mime,
    last_modified: HttpDate,
    bytes: Bytes,
}

/// Polls upstream for the image request for the cache MISS.
///
/// This will return the headers that need to be forwarded (required by spec) and the bytes that
/// represent the image.
async fn poll_upstream(
    req: &HttpRequest,
    backend: &Backend,
    archive_type: &str,
    chap_hash: &str,
    image: &str,
) -> Result<UpstreamResponse, Box<dyn std::error::Error>> {
    use std::str::FromStr;

    let upstream = backend.get_upstream().ok_or(NoUpstreamError)?;
    let url = reqwest::Url::parse(&format!(
        "{}/{}/{}/{}",
        &upstream, archive_type, chap_hash, image
    ))?;

    let res = reqwest::get(url).await?;
    let status = res.status();

    // get the mime type from upstream, or try to guess
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.parse::<mime_guess::Mime>().ok())
        .unwrap_or_else(|| mime_from_request(req));

    // get the last modified date from upstream, or else just use now
    let last_modified = res
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|x| x.to_str().ok())
        .and_then(|x| HttpDate::from_str(x).ok())
        .unwrap_or_else(|| HttpDate::from(time::SystemTime::now()));

    let bytes = res.bytes().await?;
    Ok(UpstreamResponse {
        status,
        bytes,
        content_type,
        last_modified,
    })
}

/// Handles a cache MISS, polling upstream for the image, saving it to the database, and streaming
/// it to the user
///
/// If polling from upstream fails, then it will automatically return 503 BAD GATEWAY to the user
/// with the error as the body. If cache fails saving, it will stream the image to the user anyways.
async fn handle_cache_miss(
    req: &HttpRequest,
    gs: &Arc<GlobalState>,
    (chap_hash, image, saver): (&str, &str, bool),
) -> HttpResponse {
    // poll upstream, finding the total time of the request
    let res = {
        let timer = Timer::start();
        let res = poll_upstream(
            req,
            &gs.backend,
            if saver { "data-saver" } else { "data" },
            chap_hash,
            image,
        )
        .await;
        log::debug!("poll upstream time: {}ms", timer.elapsed());
        res
    };
    // handle any errors that happen with res
    let res = match res {
        Ok(res) => res,
        Err(e) => {
            log::warn!("error polling upstream image: {}", e);
            return HttpResponse::BadGateway().finish();
        }
    };

    // error handling for the status, make sure it's 200 OK
    match res.status {
        StatusCode::OK => {}
        StatusCode::NOT_FOUND => return HttpResponse::NotFound().finish(),
        status => {
            return HttpResponse::BadGateway().body(format!("invalid upstream code: {}", status))
        }
    }

    // save the image to the cache
    {
        let timer = Timer::start();
        if !gs.cache.save(chap_hash, image, saver, &res.bytes).await {
            log::warn!(
                "error saving upstream to cache, hopefully there was some more info on this!"
            );
        }
        log::debug!("save to cache time: {}ms", timer.elapsed());
    }

    // proxy the image to the client
    HttpResponse::Ok()
        .append_header(header::ContentType(res.content_type))
        .append_header(header::LastModified(res.last_modified))
        .append_header(("X-Cache", "MISS"))
        .body(res.bytes)
}
