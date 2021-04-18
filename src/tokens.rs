use actix_web::{body::Body, http::StatusCode, BaseHttpResponse as Response, ResponseError};
use serde_json as json;
use sodiumoxide::{base64, crypto::box_};
use std::{error::Error, fmt};

const NONCE_SIZE: usize = 24;

/// Expected object representation of the JSON payload inside an MD@Home Token.
///
/// Derives from serde::Serialize for test purposes.
#[derive(Debug, serde::Deserialize)]
struct TokenPayload {
    expires: String,
    hash: String,
}

/// Every Error Kind that could happen when inside the TokenVerifier.
///
/// Most of the kinds are related to the decryption and verification of tokens, however some of
/// them apply to parsing and using PrecomputedKeys as well.
#[derive(Debug, std::cmp::PartialEq)]
pub enum TokenError {
    // generic
    InvalidBase64,
    NoKey,

    // key errors
    KeyMalformed,

    // token decrypt errors
    TokenMalformed,
    NonceMalformed,
    DecryptFailed,

    // token parse/validation errors
    InvalidPayload,
    InvalidChapterHash,
    TokenExpired,
}

impl fmt::Display for TokenError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(fmt, "TokenError::{:?}", self)
    }
}
impl Error for TokenError {}
impl ResponseError for TokenError {
    fn error_response(&self) -> Response<Body> {
        Response::build(self.status_code())
            .body(format!("error validating provided token ({})", self))
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Self::TokenExpired => StatusCode::GONE,
            _ => StatusCode::BAD_REQUEST,
        }
    }
}

/// Structure used to verify MD@Home request tokens that were boxed using the NaCl precomputed keys
/// algorithm.
///
/// Construct using the `new` or `from_str` methods, then use `verify_token` to verify the chapter
/// hash is the same and the token itself hasn't expired.
///
/// # Example
///
/// ```rust
/// const TOKEN_KEY: &str = "EXAMPLE"; // base64 encoded precomputed key (provided by API)
///
/// use crate::utils::FromBase64;
/// let mut verifier = TokenVerifier::new();
/// verifier.push_key_b64(TOKEN_KEY);
///
/// const TOKEN: &str = "EXAMPLE"; // base64 url-encoded token (provided by request path)
/// const CHAP_HASH: &str = "EXAMPLE"; // chapter hash (provided by request path)
///
/// if verifier.verify_url_token(TOKEN, CHAP_HASH).is_ok() {
///     // Token is verified
/// }
/// ```
pub struct TokenVerifier(Option<box_::PrecomputedKey>);

// functions for the Token Verifier
impl TokenVerifier {
    /// Creates a TokenVerifier using the bytes provided as a PrecomputedKey.
    pub fn new() -> Self {
        TokenVerifier(None)
    }

    /// Decodes a base64 byte array with the option to choose between the URL variant and original
    /// variant.
    fn decode_b64(b64: &str, is_url_variant: bool) -> Result<Vec<u8>, TokenError> {
        let variant = if is_url_variant {
            base64::Variant::UrlSafe
        } else {
            base64::Variant::Original
        };
        base64::decode(b64, variant).map_err(|_| TokenError::InvalidBase64)
    }

    /// Pushes a new PrecomputedKey byte array to use for decrypting Tokens
    pub fn push_key<T: AsRef<[u8]>>(&mut self, key_bytes: T) -> Result<(), TokenError> {
        self.0 = Some(Self::key_from_bytes(key_bytes)?);
        Ok(())
    }
    /// Pushes a new PrecomputedKey base64 byte array to use for decrypting Tokens
    pub fn push_key_b64(&mut self, key_str: &str) -> Result<(), TokenError> {
        self.push_key(Self::decode_b64(key_str, false)?)
    }
    /// Internal function to convert a byte array into a PrecomputedKey key
    fn key_from_bytes<T: AsRef<[u8]>>(bytes: T) -> Result<box_::PrecomputedKey, TokenError> {
        box_::PrecomputedKey::from_slice(bytes.as_ref()).ok_or(TokenError::KeyMalformed)
    }

    /// Takes a string of bytes as a token, decrypts it using the stored PrecomputedKey, then
    /// verifies that it matches the chapter hash and is not expired.
    ///
    /// This method can result in a multitude of different errors, however it only results in
    /// `Ok(())` if the result itself can be successfully decrypted, parsed, matches the chapter hash
    /// and is not expired.
    pub fn verify_token<T: AsRef<[u8]>>(
        &self,
        token: T,
        chap_hash: &str,
    ) -> Result<(), TokenError> {
        // extract nonce and cipher, then decypt
        let payload = {
            let (nonce, cipher) = Self::token_to_cipher(token.as_ref())?;
            self.decrypt_token(&nonce, &cipher)?
        };

        // convert bytes to string, then deserialize into json
        let payload = String::from_utf8(payload)
            .ok()
            .and_then(|s| json::from_str::<TokenPayload>(&s).ok());

        // verify contents inside the payload if the payload itself is valid
        if let Some(payload) = payload {
            // verify the chapter hash
            if payload.hash != chap_hash {
                return Err(TokenError::InvalidChapterHash);
            }
            // verify the token isn't expired
            let date = chrono::DateTime::parse_from_rfc3339(&payload.expires).map_err(|e| {
                log::error!("rfc3339 parse err: {}", e);
                TokenError::InvalidPayload
            })?;
            return if date > chrono::Local::now() {
                // token is not expired, so it's valid
                Ok(())
            } else {
                Err(TokenError::TokenExpired)
            };
        }
        // if deserialization failed, then there was an invalid payload
        Err(TokenError::InvalidPayload)
    }

    /// Helper method to call `verify_token` after decoding a base64 url-encoded byte array.
    ///
    /// See `verify_token` for more information.
    pub fn verify_url_token(&self, token_str: &str, chap_hash: &str) -> Result<(), TokenError> {
        // convert base64 to bytes then send to other function
        let token = Self::decode_b64(token_str, true)?;
        self.verify_token(token, chap_hash)
    }

    /// Parse ciphertext and nonce bytes from provided token bytes.
    fn token_to_cipher(token: &[u8]) -> Result<(box_::Nonce, Vec<u8>), TokenError> {
        // prevents panic on slice because of short length
        if token.len() <= NONCE_SIZE {
            return Err(TokenError::TokenMalformed);
        }

        // get byte ranges from token bytes
        // according to spec, first 24 is nonce and rest is cipher text
        let nonce_bytes = &token[0..NONCE_SIZE];
        let cipher_bytes = &token[NONCE_SIZE..];

        // convert nonce into box_::Nonce
        let nonce = box_::Nonce::from_slice(nonce_bytes).ok_or(TokenError::NonceMalformed)?;

        Ok((nonce, Vec::from(cipher_bytes)))
    }

    /// Decrypts ciphertext using the internal `PrecomputedKey`. If `Err` is present, it is
    /// always `TokenErrorKind::DecryptFailed`.
    fn decrypt_token(&self, nonce: &box_::Nonce, cipher: &[u8]) -> Result<Vec<u8>, TokenError> {
        let key = match &self.0 {
            Some(x) => x,
            None => return Err(TokenError::NoKey),
        };
        box_::open_precomputed(cipher, nonce, key).map_err(|_| TokenError::DecryptFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json as json;
    use sodiumoxide::{base64, crypto::box_};

    // arbitrary chapter hash for token payloads
    const CHAP_HASH: &str = "000000";

    /// Struct that stores a precomputed key to immitate a client and server instance. Can be used
    /// to cipher and uncipher data using precomputed shared keys for NaCl
    struct PCryptoData {
        our_p: box_::PrecomputedKey,
        their_p: box_::PrecomputedKey,
    }
    impl PCryptoData {
        /// Generates a [`PCryptoData`] instance with randomized [`PrecomputedKey`]s
        fn new() -> Self {
            sodiumoxide::init().unwrap();

            // generate public/private keys
            let our = box_::gen_keypair();
            let their = box_::gen_keypair();

            PCryptoData {
                // our precomputed uses their public
                our_p: box_::precompute(&their.0, &our.1),
                // their precomputed uses our public
                their_p: box_::precompute(&our.0, &their.1),
            }
        }

        /// Generate a valid base64 Precomputed Key and base64-url Token that can be deciphered
        /// using the [`TokenVerifier`]. Internally uses [`PCryptoData`] to generate key/token
        /// pair.
        fn key_token_pair(&self, data: &[u8]) -> (String, String) {
            // make nonce and ciphertext
            let nonce = box_::gen_nonce();
            let ciphertext = box_::seal_precomputed(data, &nonce, &self.our_p);

            // combine nonce and ciphertext together
            let token_bytes = [&nonce[..NONCE_SIZE], &ciphertext].concat();

            // return base64 encoded data
            (
                base64::encode(&self.their_p, base64::Variant::Original),
                base64::encode(&token_bytes, base64::Variant::UrlSafe),
            )
        }
    }

    fn in_one_hour() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now() + chrono::Duration::hours(1)
    }

    /// Construct a `TokenVerifier` and decrypt a valid generated token
    /// Expected Result: No Panic (from unwrap)
    #[test]
    fn valid_token_validation() {
        // create a preliminary token payload that expires in one hour
        let data = json::json!({
            "expires": in_one_hour().to_rfc3339(),
            "hash": CHAP_HASH
        })
        .to_string();

        // create a bogus token_key and token from the data
        let (token_key, token) = PCryptoData::new().key_token_pair(data.as_bytes());

        // create a new verifier and verify (unwrap so it panics if wrong)
        let mut verifier = TokenVerifier::new();
        verifier.push_key_b64(&token_key).unwrap();
        verifier.verify_url_token(&token, CHAP_HASH).unwrap();
    }

    /// Makes sure that the `TokenVerifier` ignores extra fields in the JSON payload
    /// Expected Result: No Panic (from unwrap)
    #[test]
    fn extraneous_token_fields() {
        // create a token payload that has fields that should be ignored by the TokenVerifier
        let data = json::json!({
            "expires": in_one_hour().to_rfc3339(),
            "hash": CHAP_HASH,

            // fields that should be ignored by verifier
            "str_field": "EXAMPLE",
            "i32_field": 256i32,
            "obj_field": {
                "hello": "world"
            },
            "arr_field": [128i32, 256i32]
        })
        .to_string();

        // create a bogus token_key and token from the data
        let (token_key, token) = PCryptoData::new().key_token_pair(data.as_bytes());

        // create a new verifier and verify (unwrap so it panics if wrong)
        let mut verifier = TokenVerifier::new();
        verifier.push_key_b64(&token_key).unwrap();
        verifier.verify_url_token(&token, CHAP_HASH).unwrap();
    }

    /// Construct a `TokenVerifier` with an invalid base64 key
    /// Expected Result: `TokenError::KeyMalformed`
    #[test]
    fn invalid_key_construct() {
        // random b64 bytes that should result in an invalid key
        const INVALID_KEY: &str = "74IZ1UfOKt3laiFzEtfBzg==";

        let mut verifier = TokenVerifier::new();
        // validate that the error is equal to TokenError::KeyMalformed
        match verifier.push_key_b64(INVALID_KEY) {
            Err(e) => assert!(e == TokenError::KeyMalformed),
            Ok(_) => panic!("Result was unexpectedly Ok"),
        }
    }

    /// Passes a short token (one with only a nonce, no data) to validate that there is no panic
    /// Expected Result: `TokenError::TokenMalformed`
    #[test]
    fn short_token() {
        let short_token =
            base64::encode(&box_::gen_nonce()[..NONCE_SIZE], base64::Variant::UrlSafe);

        let (token_key, _) = PCryptoData::new().key_token_pair(&[]);
        let mut verifier = TokenVerifier::new();
        verifier.push_key_b64(&token_key).unwrap();

        match verifier.verify_url_token(&short_token, CHAP_HASH) {
            Err(e) => assert!(e == TokenError::TokenMalformed),
            Ok(_) => panic!("Result was unexpectedly Ok"),
        }
    }

    // Add more tests for edge cases
}
