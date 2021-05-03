use crate::config::AppConfig;
use crate::utils::constants as c;
use std::sync::{Arc, RwLock};

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
        f.debug_struct("TLSPayload")
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

pub struct Backend {
    config: Arc<AppConfig>,
    client: reqwest::Client,

    upstream_url: RwLock<Option<String>>,
    tls: RwLock<Option<TlsPayload>>,
    token_key: RwLock<Option<String>>,
}

impl Backend {
    const URL: &'static str = "https://api.mangadex.network";

    /// Creates a skeleton [`Backend`] that is ready to start pinging. If no ping has been
    /// completed yet, then all getters will return `None`.
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),

            upstream_url: RwLock::new(None),
            tls: RwLock::new(None),
            token_key: RwLock::new(None),
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
            // unlock RwLock to get the last created_at
            let tls = self.tls.read().unwrap();

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
                tls_created_at: tls.as_ref().map(|x| x.created_at.clone()),
            }
        };
        log::debug!("sending ping payload to server: {:?}", &payload);

        // format URL and make the request to the server (handling any errors that happen in the
        // process)
        let url = reqwest::Url::parse(&format!("{}/ping", Self::URL))?;
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

    /// Updates the internal structures based on the OK response in [`ping`](Self::ping) with the
    /// least reprocussions (i.e. only writing to `RwLock`s if necessary)
    ///
    /// Returns Some(token_key) if there is a new token key, otherwise None
    fn update_from_response(&self, res: &PingResponse) -> Option<String> {
        // store new tls_created_at because the server has determined we're using an outdated
        // version
        if let Some(ref tls) = res.tls {
            let mut inner = self.tls.write().unwrap();
            *inner = Some(tls.clone());
        }

        // store new upstream url
        // weird syntax because we need to drop read lock before locking for write
        let update_upstream = {
            let inner = self.upstream_url.read().unwrap();
            inner.as_ref() != Some(&res.image_server)
        };
        if update_upstream {
            let mut inner = self.upstream_url.write().unwrap();
            *inner = Some(res.image_server.clone());
        }

        // read comment above this for reasoning on weird syntax
        let update_key = {
            let inner = self.token_key.read().unwrap();
            inner.as_ref() != Some(&res.token_key)
        };
        if update_key {
            let mut inner = self.token_key.write().unwrap();
            *inner = Some(res.token_key.clone());

            // return inner token_key, signalling that this token key is a brand new one
            inner.clone()
        } else {
            // we didn't update token key, so signal that the one stored internally is up-to-date
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
        let url = reqwest::Url::parse(&format!("{}/stop", Self::URL))?;
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
    pub fn get_upstream(&self) -> Option<String> {
        let inner = self.upstream_url.read().unwrap();
        inner.clone()
    }
}
