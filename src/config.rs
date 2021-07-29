use crate::utils::Secret;
use serde::Deserialize;
use std::io;
use std::path::{Path, PathBuf};
use tokio::fs;

/// Global application configuration
#[derive(Deserialize, Debug)]
pub struct AppConfig {
    // basic client configuration
    pub client_secret: Secret<String>,
    pub max_grace_period: i32,
    #[serde(default)]
    pub skip_tokens: bool,
    #[serde(default)]
    pub disable_ssl: bool,

    // cache configuration
    pub cache_size_mebibytes: u32,
    pub cache_engine: String,
    #[serde(rename = "rocksdb_options")]
    pub rocks_opt: Option<RocksConfig>,
    #[serde(rename = "fs_options")]
    pub fs_opt: Option<FsConfig>,

    // webserver settings
    pub port: u16,
    pub bind_address: String,
    pub worker_threads: Option<usize>,
    pub keep_alive: usize,
    pub enforce_secure_tls: bool,
    #[serde(default)]
    pub disable_ad_headers: bool,
    #[serde(default = "opt_reject_invalid_sni")]
    pub reject_invalid_sni: bool,

    // info sent to external api
    pub external_ip: Option<String>,
    pub external_port: Option<u16>,
    pub external_max_speed: Option<u32>,
}
fn opt_reject_invalid_sni() -> bool {
    true
}

/// Configuration for RocksDB cache engine
#[derive(Deserialize, Debug)]
pub struct RocksConfig {
    pub path: String,

    // block options
    #[serde(default)]
    pub disable_bloom_filter: bool,
    pub lru_size: Option<usize>,

    // db options
    pub parallelism: Option<i32>,
    pub write_buffer_size: Option<usize>,
    pub write_rate_limit: Option<usize>,
}

/// Configuration for FileSystem cache engine
#[derive(Deserialize, Debug)]
pub struct FsConfig {
    pub path: String,
    #[serde(default = "fsce_rw_buf_sz")]
    pub rw_buffer_size: usize,
    #[serde(default = "fsce_lru_sz")]
    pub lru_size_mebibytes: usize,
}
fn fsce_rw_buf_sz() -> usize {
    16
}
fn fsce_lru_sz() -> usize {
    128
}

/// Various different errors that could happen when opening or parsing a configuration file.
#[derive(Debug)]
enum ConfigError {
    YamlParseError(serde_yaml::Error),
    JsonParseError(serde_json::Error),
    IoError(io::Error),
    UnexpectedProblem,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IoError(e) => write!(fmt, "error opening or reading file: {}", e),
            Self::YamlParseError(e) => write!(fmt, "error parsing yaml: {}", e),
            Self::JsonParseError(e) => write!(fmt, "error parsing json: {}", e),
            Self::UnexpectedProblem => write!(
                fmt,
                "unexpected problem happened when handling config parse"
            ),
        }
    }
}

impl AppConfig {
    /// Opens a file and parses into a [AppConfig](Self). Returns Some if it is successful and None
    /// if not. No success might be for various reasons (which are stderr logged).
    ///
    /// How the file is parsed is determined by the extension of the file, i.e. *.yaml will be
    /// parsed using a yaml parser, *.json will be parsed using a json parser.
    async fn read_and_parse_file(path: &Path) -> Result<Self, ConfigError> {
        // find the extension of the file we're going to open. the extension of the file determines
        // the method in which we parse, i.e. "yaml" would be parsed as a YAML configuration file.
        let ext = path.extension().and_then(|e| e.to_str());

        // match based on if we read successfully and the extension of the file
        match (fs::read_to_string(path).await, ext) {
            // successfully read a yaml file
            (Ok(content), Some("yaml")) => {
                serde_yaml::from_str(&content).map_err(ConfigError::YamlParseError)
            }
            // successfully read a json file
            (Ok(content), Some("json")) => {
                serde_json::from_str(&content).map_err(ConfigError::JsonParseError)
            }

            // there was some sort of error reading the file so log as warning and continue
            // through rest of files
            (Err(e), _) => Err(ConfigError::IoError(e)),

            // weird and unexpected case. just dump variables to console
            (a, b) => {
                log::error!(
                    "unexpected case in match statement occurred, vardump: {:?} {:?}",
                    a,
                    b
                );
                Err(ConfigError::UnexpectedProblem)
            }
        }
    }

    /// Opens files in order and tries to successfully parse them into an [AppConfig](Self). If
    /// unsuccessful, it will continue onto the next file until it runs out of options.
    ///
    /// Determines the parse algorithm based on the extension of the file path, i.e. `*.yaml` will
    /// result in yaml parsing and `*.json` will result in json parsing.
    async fn try_files(files: &[&str]) -> Option<Self> {
        // convert all files into PathBufs
        let files: Vec<PathBuf> = files.iter().map(|&f| PathBuf::from(f)).collect();

        for path in &files {
            match Self::read_and_parse_file(path).await {
                Ok(res) => return Some(res),
                Err(e) => log::warn!("{} (for file {:?})", e, path),
            }
        }
        None
    }
}

/// Asyncronously finds and parses the Application Configuration file and returns it if successful.
///
/// Currently tries `settings.yaml` and `settings.json` looking for the configuration. If
/// completely successful, it will return `Some(AppConfig)` otherwise `None`.
pub async fn init() -> Option<AppConfig> {
    // hardcoded files to check for settings
    // TODO: make it variable (through CLI parameters probably)
    const FILES: [&str; 2] = ["./settings.yaml", "./settings.json"];
    let conf = AppConfig::try_files(&FILES).await;

    // if successfully loaded, then log it cause why not
    if let Some(conf) = &conf {
        log::info!("loaded configuration: {:?}", conf);
    }
    conf
}
