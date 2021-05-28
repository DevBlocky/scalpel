use crate::config::AppConfig;
use crate::utils::constants as c;
use arc_swap::ArcSwap;
use lazy_static::lazy_static;
use std::sync::Arc;
use std::time::Duration;

// below are structures that represent JSON objects for passing messages to and from the server
//
// see docs for more:
// https://gitlab.com/mangadex-pub/mangadex_at_home/-/wikis/Formal-Specification-for-Custom-Clients

// https://api.mangadex.network/ping
#[derive(serde::Serialize, Debug)]
struct PingRequest {
    secret: String,
    port: u16,
    disk_space: u64,
    network_speed: u64,
    build_version: u16,
    ip_address: Option<String>,
    tls_created_at: Option<String>,
}
#[derive(serde::Deserialize, Debug)]
#[allow(dead_code)]
struct PingResponse {
    image_server: String,
    latest_build: u16,
    url: String,
    token_key: String,
    compromised: bool,
    paused: bool,
    client_id: String,
    tls: Option<TlsPayload>,
}
#[derive(Clone, serde::Deserialize)]
#[allow(dead_code)]
pub struct TlsPayload {
    pub(crate) created_at: String,
    pub(crate) private_key: String,
    pub(crate) certificate: String,
}

// custom fmt::Debug implementation so that when debug printing PingResponse it won't print data
// that wouldn't make any sense
impl std::fmt::Debug for TlsPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsPayload")
            .field("created_at", &self.created_at)
            .finish()
    }
}

// https://api.mangadex.network/stop
#[derive(serde::Serialize)]
struct StopRequest<'a> {
    secret: &'a str,
}
#[derive(serde::Deserialize)]
struct StopResponse;

#[derive(Debug)]
enum BackendError {
    InvalidSecret,

    /// Unexpected response from server: includes status and body
    UnexpectedResponse(reqwest::StatusCode, String),
}
impl std::fmt::Display for BackendError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSecret => write!(fmt, "invalid client secret provided"),
            Self::UnexpectedResponse(status, body) => write!(
                fmt,
                "unexpected HTTP response: STATUS_CODE \"{}\" BODY \"{}\"",
                status, body
            ),
        }
    }
}
impl std::error::Error for BackendError {}

#[derive(Debug)]
struct PingStore {
    tls: TlsPayload,
    token_key: String,
    upstream_url: Arc<reqwest::Url>,
}
pub struct Backend {
    config: Arc<AppConfig>,
    client: reqwest::Client,

    ping_info: ArcSwap<Option<PingStore>>,
}

lazy_static! {
    static ref BASE_URL: reqwest::Url =
        reqwest::Url::parse("https://api.mangadex.network").unwrap();
}
impl Backend {
    /// Creates a skeleton [`Backend`] that is ready to start pinging. If no ping has been
    /// completed yet, then all getters will return `None`.
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self {
            config,
            client: reqwest::Client::builder()
                // actual requests for this client should never exceed 60s,
                // unless you have the worst internet connection in history
                .timeout(Duration::from_secs(60))
                .build()
                .expect("backend http client"),

            ping_info: ArcSwap::from_pointee(None),
        }
    }

    /// Pings the backend with a created payload and returns a TLS payload (if it's new) and a
    /// token key (if it's new)
    ///
    /// This function will handle all mutability by using interior mutability that is thread safe
    /// (using [`RwLock`]s). This is the only function in the entire implementation that will lock
    /// the `RwLock`s.
    ///
    /// # Panic
    ///
    /// This function may panic if any of the `RwLock`s are considered poisoned, as this function
    /// will unwrap all `RwLock`s that it is writing to or reading from
    pub async fn ping(
        &self,
    ) -> Result<(Option<TlsPayload>, Option<String>), Box<dyn std::error::Error>> {
        // structure JSON request using configuration
        let payload = {
            // find the tls_created_at field from the last ping info
            let last_ping = self.ping_info.load();
            let tls_created_at = last_ping
                .as_ref()
                .as_ref()
                .map(|x| x.tls.created_at.clone());

            PingRequest {
                secret: self.config.client_secret.clone(),
                disk_space: self.config.cache_size_mebibytes as u64 * 1024 * 1024,
                port: self.config.external_port.unwrap_or(self.config.port),
                network_speed: self
                    .config
                    .external_max_speed
                    .map(|x| x as u64 * 1000 / 8)
                    .unwrap_or(0),
                build_version: c::SPEC,
                ip_address: self.config.external_ip.clone(),
                tls_created_at,
            }
        };
        log::debug!("sending ping payload to server: {:?}", &payload);

        // format URL and make the request to the server (handling any errors that happen in the
        // process)
        let url = reqwest::Url::options()
            .base_url(Some(&BASE_URL))
            .parse("/ping")?;
        let res = self
            .client
            .post(url)
            .json(&payload)
            .header("Content-Type", "application/json")
            .send()
            .await?;

        // obtain the body of the request
        let client_setup = match res.status() {
            // response was OK, parse JSON body
            reqwest::StatusCode::OK => res.json::<PingResponse>().await?,

            // error handling
            reqwest::StatusCode::UNAUTHORIZED => return Err(Box::new(BackendError::InvalidSecret)),
            status => {
                return Err(Box::new(BackendError::UnexpectedResponse(
                    status,
                    res.text().await.unwrap_or_else(|_| "NO BODY".to_string()),
                )))
            }
        };
        log::info!("received ping response from server: {:?}", &client_setup);

        // log some warning about compromised/paused
        if client_setup.paused {
            log::warn!("according to backend, your client is paused!");
        }
        if client_setup.compromised {
            log::warn!("according to backend, your client is compromised!");
        }

        // update internal structures based on response
        let new_token_key = self.update_from_response(&client_setup);

        // (crt, crt_new, token_key)
        Ok((client_setup.tls, new_token_key))
    }

    /// Updates the internal structures based on the OK response in [`ping`](Self::ping)
    ///
    /// Returns Some(token_key) if there is a new token key, otherwise None
    fn update_from_response(&self, res: &PingResponse) -> Option<String> {
        let last_info = self.ping_info.load();
        let last_info = Option::as_ref(&last_info);

        // find whether or not we have a new token key
        let new_token_key = match last_info {
            Some(x) => x.token_key != res.token_key,
            // if this is the initial ping, then we do have a new token key
            _ => true,
        };

        let info = PingStore {
            // either use the new TlsPayload or the one from the previous ping
            //
            // this should only panic if there is no TlsPayload on the previous ping *and* this
            // ping, which means there is a critical problem.
            tls: res
                .tls
                .as_ref()
                .or_else(|| last_info.map(|x| &x.tls))
                .map(TlsPayload::clone)
                .unwrap(),
            token_key: res.token_key.clone(),
            // NOTE: if the below fails, there's something seriously wrong
            upstream_url: Arc::new(
                reqwest::Url::parse(&res.image_server).expect("malformed base upstream url"),
            ),
        };
        self.ping_info.store(Arc::new(Some(info)));

        if new_token_key {
            Some(res.token_key.clone())
        } else {
            None
        }
    }

    /// Pings the backend API alerting to the stop of the client
    ///
    /// This function does not modify the internal state, therefore doesn't lock any of the
    /// internal structures at all.
    pub async fn stop(&self) -> Result<(), Box<dyn std::error::Error>> {
        // create payload for JSON request
        let payload = StopRequest {
            secret: &self.config.client_secret,
        };

        // format URL and make the request to the server (handling any errors that happen in the
        // process)
        let url = reqwest::Url::options()
            .base_url(Some(&BASE_URL))
            .parse("/stop")?;
        let res = self
            .client
            .post(url)
            .json(&payload)
            .header("Content-Type", "application/json")
            .send()
            .await?;

        // match the status code with different errors
        match res.status() {
            reqwest::StatusCode::OK => Ok(()),
            reqwest::StatusCode::UNAUTHORIZED => Err(Box::new(BackendError::InvalidSecret)),
            status => Err(Box::new(BackendError::UnexpectedResponse(
                status,
                res.text().await.unwrap_or_else(|_| "NO BODY".to_string()),
            ))),
        }
    }

    /// Returns the upstream url stored from the API. Returns `None` if there has been no
    /// successful ping yet, and `Some` containing the upstream URL as provided up the API.
    pub fn get_upstream(&self) -> Option<Arc<reqwest::Url>> {
        let ping_info = self.ping_info.load();
        Option::as_ref(&ping_info).map(|x| Arc::clone(&x.upstream_url))
    }
}
