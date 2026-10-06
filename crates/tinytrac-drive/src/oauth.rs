//! Loopback OAuth authorization with PKCE and cancellation.

use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use tinytrac_engine::Result;

use crate::{Account, Authorization, Client, Endpoints, Secret, http};

#[derive(serde::Deserialize)]
struct About {
    user: User,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    email_address: String,
    permission_id: String,
}

const POLL: Duration = Duration::from_millis(100);

/// Signs in using a temporary IPv4 loopback listener and PKCE. Call on a worker thread.
/// The browser launcher must return promptly, without waiting for the browser to exit.
pub fn authorize(
    client: &Client,
    endpoints: &Endpoints,
    cancel: &AtomicBool,
    open_browser: impl FnOnce(&str) -> io::Result<()>,
) -> Result<Authorization> {
    authorize_with_timeout(
        client,
        endpoints,
        cancel,
        open_browser,
        Duration::from_secs(300),
    )
}

pub(crate) fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random() -> Result<String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| http::remote("Could not obtain sign-in randomness"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) fn authorize_with_timeout(
    client: &Client,
    endpoints: &Endpoints,
    cancel: &AtomicBool,
    open_browser: impl FnOnce(&str) -> io::Result<()>,
    timeout: Duration,
) -> Result<Authorization> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let redirect = format!(
        "http://127.0.0.1:{}/callback",
        listener.local_addr()?.port()
    );
    let verifier = random()?;
    let state = random()?;
    let url = http::query(
        &endpoints.auth,
        &[
            ("response_type", "code"),
            ("client_id", &client.id),
            ("redirect_uri", &redirect),
            ("scope", "https://www.googleapis.com/auth/drive.appdata"),
            ("access_type", "offline"),
            ("prompt", "consent select_account"),
            ("code_challenge", &challenge(&verifier)),
            ("code_challenge_method", "S256"),
            ("state", &state),
        ],
    );
    let deadline = Instant::now() + timeout;
    if cancel.load(Ordering::Relaxed) {
        return Err(http::remote("Google sign-in cancelled"));
    }
    open_browser(&url).map_err(|_| http::remote("Could not open the sign-in browser"))?;
    let code = wait(&listener, &state, cancel, deadline)?;
    drop(listener);
    let agent = http::agent();
    let tokens = http::token(
        &agent,
        client,
        endpoints,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect),
            ("code_verifier", &verifier),
        ],
    )?;
    let refresh_token = tokens
        .refresh_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| http::remote("Google returned no refresh token; sign in again"))?;
    let response = agent
        .get(http::query(
            &format!("{}/drive/v3/about", endpoints.drive),
            &[("fields", "user(emailAddress,permissionId)")],
        ))
        .header(
            "Authorization",
            http::bearer(&Secret::new(tokens.access_token)),
        )
        .call()
        .map_err(http::network)?;
    let about: About = http::json(&mut http::check(response)?)?;
    if about.user.permission_id.is_empty() || about.user.email_address.is_empty() {
        return Err(http::remote("Google returned no account identity"));
    }
    Ok(Authorization {
        account: Account {
            id: about.user.permission_id,
            email: about.user.email_address,
        },
        refresh_token: Secret::new(refresh_token),
    })
}

fn interrupted(cancel: &AtomicBool, deadline: Instant) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(http::remote("Google sign-in cancelled"));
    }
    if Instant::now() >= deadline {
        return Err(http::remote("Google sign-in timed out"));
    }
    Ok(())
}

fn wait(
    listener: &TcpListener,
    state: &str,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<String> {
    loop {
        interrupted(cancel, deadline)?;
        match listener.accept() {
            Ok((mut stream, _)) => {
                if let Some(request) = request(&mut stream, cancel, deadline)? {
                    let result = callback(&request, state);
                    if let Some(result) = result {
                        let page = if result.is_ok() {
                            "tinytrac is connected. You can close this tab."
                        } else {
                            "Sign-in failed. You can close this tab."
                        };
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                            page.len()
                        );
                        return result;
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(e) => return Err(e.into()),
        }
    }
}

fn request(
    stream: &mut TcpStream,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Option<String>> {
    stream.set_read_timeout(Some(POLL))?;
    stream.set_write_timeout(Some(POLL))?;
    let connection_deadline = deadline.min(Instant::now() + Duration::from_secs(2));
    let mut bytes = Vec::new();
    while bytes.len() < 8192 && Instant::now() < connection_deadline {
        interrupted(cancel, deadline)?;
        let mut buffer = [0; 512];
        let remaining = (8192 - bytes.len()).min(buffer.len());
        match stream.read(&mut buffer[..remaining]) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buffer[..n]);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    return Ok(String::from_utf8(bytes).ok());
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    Ok(None)
}

fn callback(request: &str, state: &str) -> Option<Result<String>> {
    let mut words = request.lines().next()?.split_whitespace();
    if words.next()? != "GET" {
        return None;
    }
    let target = words.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        return None;
    }
    let mut params = BTreeMap::new();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        if params.insert(key, value).is_some() {
            return Some(Err(http::remote(
                "Google sign-in callback has duplicate parameters",
            )));
        }
    }
    if params.get("state").map(std::convert::AsRef::as_ref) != Some(state) {
        return Some(Err(http::remote("Google sign-in state did not match")));
    }
    if let Some(error) = params.get("error") {
        return Some(Err(http::remote(if error == "access_denied" {
            "Google sign-in denied"
        } else {
            "Google sign-in failed"
        })));
    }
    Some(
        params
            .get("code")
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| http::remote("Google sign-in callback has no code")),
    )
}
