//! Credential-safe HTTP requests and response handling.

use std::time::Duration;

use serde::de::DeserializeOwned;
use tinytrac_engine::{Error, Result};

use crate::{Client, Endpoints, Secret};

pub(crate) type Response = ureq::http::Response<ureq::Body>;

pub(crate) fn agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        // Never forward credentials or replay a token form across redirects.
        .max_redirects(0);
    // Loopback fixtures must never inherit a developer's outbound proxy.
    #[cfg(test)]
    let config = config.proxy(None);
    config.build().into()
}

pub(crate) fn remote(message: &str) -> Error {
    Error::Remote(message.into())
}

pub(crate) fn network(_: ureq::Error) -> Error {
    remote("Google connection failed; will retry")
}

pub(crate) fn query(base: &str, pairs: &[(&str, &str)]) -> String {
    format!("{base}?{}", form(pairs))
}

pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().copied())
        .finish()
}

pub(crate) fn json<T: DeserializeOwned>(response: &mut Response) -> Result<T> {
    let bytes = response
        .body_mut()
        .with_config()
        .limit(8 * 1024 * 1024)
        .read_to_vec()
        .map_err(network)?;
    serde_json::from_slice(&bytes).map_err(|_| remote("Google returned an invalid response"))
}

pub(crate) fn check(mut response: Response) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let value: serde_json::Value = json(&mut response).unwrap_or_default();
    if value.get("error").and_then(serde_json::Value::as_str) == Some("invalid_grant") {
        return Err(Error::AuthRequired(
            "Google sign-in expired or was revoked; sign in again".into(),
        ));
    }
    let reasons = value
        .pointer("/error/errors")
        .and_then(serde_json::Value::as_array);
    let has =
        |reason: &str| reasons.is_some_and(|errors| errors.iter().any(|e| e["reason"] == reason));
    let message = if status == 429 || has("rateLimitExceeded") || has("userRateLimitExceeded") {
        "Google Drive is busy; will retry".into()
    } else if has("storageQuotaExceeded") {
        "Google Drive storage quota is full".into()
    } else if has("accessNotConfigured") || has("serviceDisabled") {
        "Google Drive API is disabled for this client".into()
    } else {
        format!("Google request failed (HTTP {status}); will retry")
    };
    Err(Error::Remote(message))
}

#[derive(serde::Deserialize)]
pub(crate) struct Tokens {
    pub access_token: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
}

pub(crate) fn token(
    agent: &ureq::Agent,
    client: &Client,
    endpoints: &Endpoints,
    pairs: &[(&str, &str)],
) -> Result<Tokens> {
    let mut pairs = pairs.to_vec();
    pairs.extend([
        ("client_id", client.id.as_str()),
        ("client_secret", client.secret.as_str()),
    ]);
    let response = agent
        .post(&endpoints.token)
        .content_type("application/x-www-form-urlencoded")
        .send(form(&pairs))
        .map_err(network)?;
    let tokens: Tokens = json(&mut check(response)?)?;
    if tokens.access_token.is_empty() {
        return Err(remote("Google returned no access token"));
    }
    Ok(tokens)
}

pub(crate) fn bearer(token: &Secret) -> String {
    format!("Bearer {}", token.expose())
}
