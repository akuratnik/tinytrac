//! Blocking Google Drive app-data transport. The caller owns browser launch and
//! credential storage; no tokens are persisted by this crate.

mod drive;
mod http;
mod oauth;

pub use drive::Drive;
pub use oauth::authorize;

/// Desktop OAuth credentials; the client secret is not confidential for installed apps.
#[derive(Clone)]
pub struct Client {
    pub id: String,
    pub secret: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// A credential with deliberately redacted debug output.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Explicit access for the caller's credential store.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// Trusted endpoint configuration. `drive` is an origin, without `/drive/v3`.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub auth: String,
    pub token: String,
    pub drive: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            drive: "https://www.googleapis.com".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Stable Drive permission id, suitable for the remote target identity.
    pub id: String,
    pub email: String,
}

#[derive(Debug)]
pub struct Authorization {
    pub account: Account,
    pub refresh_token: Secret,
}

#[cfg(test)]
mod tests;
