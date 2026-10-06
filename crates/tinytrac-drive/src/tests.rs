//! Loopback fixtures for authorization and Drive transport behavior.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex, atomic::AtomicBool},
    thread,
    time::Duration,
};

use serde_json::{Value, json};
use tinytrac_engine::{Error, Transport};

use crate::{Client, Drive, Endpoints, Secret, authorize, oauth};

const BATCH: &str = "batch.json";

#[derive(Debug, Clone)]
struct Request {
    method: String,
    target: String,
    headers: String,
    body: Vec<u8>,
}

impl Request {
    fn query(&self) -> BTreeMap<String, String> {
        params(self.target.split_once('?').map_or("", |(_, q)| q))
    }
}

fn params(query: &str) -> BTreeMap<String, String> {
    form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

struct Reply {
    status: u16,
    body: Vec<u8>,
    headers: String,
}

fn reply(status: u16, value: &Value) -> Reply {
    Reply {
        status,
        body: value.to_string().into_bytes(),
        headers: String::new(),
    }
}

fn token() -> Reply {
    reply(
        200,
        &json!({"access_token":"access", "expires_in":3600, "refresh_token":"refresh", "scope":"https://www.googleapis.com/auth/drive.appdata"}),
    )
}

pub(super) struct Server {
    endpoints: Endpoints,
    context: tinytrac_engine::SyncContext,
    requests: Arc<Mutex<Vec<Request>>>,
    thread: Option<thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl Server {
    fn new(mut handler: impl FnMut(&Request, &str) -> Reply + Send + 'static) -> Self {
        let context = context();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let endpoints = Endpoints {
            auth: format!("{origin}/auth"),
            token: format!("{origin}/token"),
            drive: origin.clone(),
        };
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = thread::spawn(move || {
            while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = read_request(&mut stream);
                let result = handler(&request, &origin);
                recorded.lock().unwrap().push(request);
                write!(
                    stream,
                    "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                    result.status,
                    result.body.len(),
                    result.headers
                )
                .unwrap();
                stream.write_all(&result.body).unwrap();
            }
        });
        Self {
            context,
            endpoints,
            requests,
            thread: Some(thread),
            stop,
        }
    }

    pub(super) fn drive(&self) -> Drive {
        Drive::new(
            client(),
            Secret::new("refresh".into()),
            self.context.clone(),
            self.endpoints.clone(),
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let result = self.thread.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let end = loop {
        let mut b = [0; 1];
        stream.read_exact(&mut b).unwrap();
        bytes.push(b[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
    };
    let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
    let len = headers
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(|s| s.parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; len];
    stream.read_exact(&mut body).unwrap();
    let mut words = headers.split_whitespace();
    Request {
        method: words.next().unwrap().into(),
        target: words.next().unwrap().into(),
        headers,
        body,
    }
}

fn context() -> tinytrac_engine::SyncContext {
    let mut engine = tinytrac_engine::Engine::open_in_memory().unwrap();
    engine
        .set_sync_target(Some(tinytrac_engine::SyncTarget::Remote {
            id: "account".into(),
            label: "Google".into(),
        }))
        .unwrap();
    engine.sync_context().unwrap().unwrap()
}

fn client() -> Client {
    Client {
        id: "desktop".into(),
        secret: "desktop-secret".into(),
    }
}

fn callback(url: &str, query: &str) {
    let url = url.strip_prefix("http://").unwrap();
    let (address, path) = url.split_once('/').unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    write!(
        stream,
        "GET /{path}{query} HTTP/1.1\r\nHost: {address}\r\n\r\n"
    )
    .unwrap();
    // Browser launcher must return without waiting for the listener's response.
}

#[test]
fn pkce_rfc_vector_and_secret_redaction() {
    assert_eq!(
        oauth::challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    assert!(!format!("{:?}", Secret::new("sensitive".into())).contains("sensitive"));
}

#[test]
fn authorization_url_exchange_and_account() {
    let server = Server::new(|r, _| {
        if r.target == "/token" {
            token()
        } else {
            assert!(r.target.starts_with("/drive/v3/about?"));
            reply(
                200,
                &json!({"user":{"permissionId":"account", "emailAddress":"me@example.com"}}),
            )
        }
    });
    let mut auth_params = BTreeMap::new();
    let result = authorize(
        &client(),
        &server.endpoints,
        &AtomicBool::new(false),
        |url| {
            auth_params = params(url.split_once('?').unwrap().1);
            assert_eq!(auth_params["response_type"], "code");
            assert_eq!(auth_params["client_id"], "desktop");
            assert_eq!(
                auth_params["scope"],
                "https://www.googleapis.com/auth/drive.appdata"
            );
            assert_eq!(auth_params["access_type"], "offline");
            assert_eq!(auth_params["prompt"], "consent select_account");
            assert_eq!(auth_params["code_challenge_method"], "S256");
            assert_eq!(auth_params["state"].len(), 43);
            let redirect = &auth_params["redirect_uri"];
            callback(&redirect.replace("/callback", "/favicon.ico"), "");
            callback(
                redirect,
                &format!("?state={}&code=private-code", auth_params["state"]),
            );
            Ok(())
        },
    )
    .unwrap();
    let result = result.unwrap();
    assert_eq!(result.account.id, "account");
    assert_eq!(result.account.email, "me@example.com");
    assert_eq!(result.refresh_token.expose(), "refresh");
    let requests = server.requests.lock().unwrap();
    let exchange = params(std::str::from_utf8(&requests[0].body).unwrap());
    assert_eq!(exchange["redirect_uri"], auth_params["redirect_uri"]);
    assert_eq!(exchange["client_secret"], "desktop-secret");
    assert_eq!(exchange["code"], "private-code");
    assert_eq!(exchange["code_verifier"].len(), 43);
    assert_eq!(
        oauth::challenge(&exchange["code_verifier"]),
        auth_params["code_challenge"]
    );
    assert_eq!(
        requests[1].query()["fields"],
        "user(emailAddress,permissionId)"
    );
}

#[test]
fn callback_failures_cancel_timeout_and_missing_refresh() {
    let server = Server::new(|_, _| {
        reply(
            200,
            &json!({"access_token":"access", "expires_in":3600, "scope":"https://www.googleapis.com/auth/drive.appdata"}),
        )
    });
    for (suffix, expected) in [
        ("state={state}&error=access_denied", "denied"),
        ("state={state}&code=a", "no refresh token"),
    ] {
        let result = authorize(
            &client(),
            &server.endpoints,
            &AtomicBool::new(false),
            |url| {
                let p = params(url.split_once('?').unwrap().1);
                callback(
                    &p["redirect_uri"],
                    &format!("?{}", suffix.replace("{state}", &p["state"])),
                );
                Ok(())
            },
        );
        assert!(result.unwrap_err().to_string().contains(expected));
    }
    assert!(
        authorize(
            &client(),
            &server.endpoints,
            &AtomicBool::new(true),
            |_| panic!()
        )
        .unwrap()
        .is_none()
    );
    assert!(
        oauth::authorize_with_timeout(
            &client(),
            &server.endpoints,
            &AtomicBool::new(false),
            |_| Ok(()),
            Duration::from_millis(10)
        )
        .unwrap_err()
        .to_string()
        .contains("timed out")
    );
}

#[test]
fn refresh_once_on_401_and_read_by_id() {
    let mut reads = 0;
    let mut refreshes = 0;
    let server = Server::new(move |r, _| {
        if r.target == "/token" {
            refreshes += 1;
            return reply(
                200,
                &json!({"access_token":format!("access-{refreshes}"), "expires_in":3600}),
            );
        }
        assert_eq!(r.target, "/drive/v3/files/file-id?alt=media");
        reads += 1;
        assert!(r.headers.to_ascii_lowercase().contains(&format!(
            "authorization: bearer access-{}",
            if reads == 1 { 1 } else { 2 }
        )));
        if reads == 1 {
            reply(401, &json!({}))
        } else {
            reply(200, &json!("bytes"))
        }
    });
    let mut drive = server.drive();
    assert_eq!(drive.read("file-id").unwrap(), b"\"bytes\"");
    drive.read("file-id").unwrap();
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.iter().filter(|r| r.target == "/token").count(), 2);
    assert!(
        requests[1]
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer access-1")
    );
}

#[test]
fn errors_are_safe_and_retryable_except_revoked_grants() {
    for (status, body, expected, auth) in [
        (
            400,
            json!({"error":"invalid_grant", "error_description":"SECRET"}),
            "expired or was revoked",
            true,
        ),
        (
            403,
            json!({"error":{"errors":[{"reason":"rateLimitExceeded"}], "message":"SECRET"}}),
            "busy; will retry",
            false,
        ),
        (
            403,
            json!({"error":{"errors":[{"reason":"userRateLimitExceeded"}]}}),
            "busy; will retry",
            false,
        ),
        (429, json!({}), "busy; will retry", false),
        (
            403,
            json!({"error":{"errors":[{"reason":"storageQuotaExceeded"}]}}),
            "storage quota",
            false,
        ),
        (
            403,
            json!({"error":{"errors":[{"reason":"accessNotConfigured"}]}}),
            "API is disabled",
            false,
        ),
    ] {
        let server = Server::new(move |_, _| reply(status, &body));
        let error = server.drive().read("file").unwrap_err();
        assert_eq!(matches!(error, Error::AuthRequired(_)), auth);
        assert!(error.to_string().contains(expected));
        assert!(!error.to_string().contains("SECRET"));
    }
}

#[test]
fn resumable_upload_metadata_bytes_and_final_confirmation() {
    for final_status in [200, 201, 202, 204, 308, 500] {
        let server = Server::new(move |r, origin| {
            if r.target == "/token" {
                return token();
            }
            if r.method == "POST" {
                assert_eq!(r.target, "/upload/drive/v3/files?uploadType=resumable");
                assert_eq!(
                    serde_json::from_slice::<Value>(&r.body).unwrap()["parents"],
                    json!(["appDataFolder"])
                );
                return Reply {
                    headers: format!("Location: {origin}/session\r\n"),
                    ..reply(200, &json!({}))
                };
            }
            assert_eq!(r.method, "PUT");
            assert_eq!(r.target, "/session");
            assert_eq!(r.body, [0, 1, 255]);
            reply(final_status, &json!({"id":"new"}))
        });
        let result = server.drive().publish(BATCH, &[0, 1, 255]);
        assert_eq!(result.is_ok(), matches!(final_status, 200 | 201));
        if final_status == 308 {
            assert_eq!(
                result.unwrap_err().to_string(),
                "Google upload was interrupted; will retry"
            );
        }
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            serde_json::from_slice::<Value>(&requests[1].body).unwrap()["name"],
            format!("{}/{BATCH}", server.context.device())
        );
    }
}

#[test]
fn bootstrap_changes_pagination_filtering_and_rebuild() {
    let binding = context();
    let device = binding.device().to_owned();
    let mut step = 0;
    let server = Server::new(move |r, _| {
        if r.target == "/token" {
            return token();
        }
        step += 1;
        match step {
            1 | 7 => {
                assert!(r.target.starts_with("/drive/v3/changes/startPageToken"));
                reply(200, &json!({"startPageToken":"start"}))
            }
            2 => {
                assert_eq!(r.query()["pageSize"], "1000");
                reply(
                    200,
                    &json!({"nextPageToken":"files2", "files":[{"id":"a", "name":format!("other/{BATCH}")}, {"id":"own", "name":format!("{device}/{BATCH}")}, {"id":"junk", "name":"other/no.txt"}]}),
                )
            }
            3 => {
                assert_eq!(r.query()["pageToken"], "files2");
                reply(
                    200,
                    &json!({"files":[{"id":"b", "name":format!("other/{BATCH}")}]}),
                )
            }
            4 => {
                assert_eq!(r.query()["pageToken"], "start");
                reply(
                    200,
                    &json!({"nextPageToken":"changes2", "changes":[{"fileId":"a", "removed":true}, {"fileId":"c", "file":{"id":"c", "name":format!("other/{BATCH}")}}]}),
                )
            }
            5 => {
                assert_eq!(r.query()["pageToken"], "changes2");
                reply(
                    200,
                    &json!({"newStartPageToken":"next", "changes":[{"fileId":"b", "file":{"id":"b", "name":format!("other/{BATCH}"), "trashed":true}}]}),
                )
            }
            6 => {
                assert_eq!(r.query()["pageToken"], "next");
                reply(410, &json!({}))
            }
            8 => reply(
                200,
                &json!({"files":[{"id":"rebuilt", "name":format!("other/{BATCH}")}]}),
            ),
            _ => reply(200, &json!({"newStartPageToken":"end", "changes":[]})),
        }
    });
    let mut drive = Drive::new(
        client(),
        Secret::new("refresh".into()),
        binding,
        server.endpoints.clone(),
    );
    assert_eq!(drive.list().unwrap(), ["a", "b"]);
    assert_eq!(drive.list().unwrap(), ["c"]);
    assert_eq!(drive.list().unwrap(), ["rebuilt"]);
    assert_eq!(drive.list().unwrap(), ["rebuilt"]);
}

#[test]
fn two_engines_converge_through_drive() {
    // This fixture replays every file as a change; it does not exercise incremental
    // changes end-to-end. Cursor/pagination behavior is covered separately.
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut pending = String::new();
    let server = Server::new(move |r, origin| {
        let path = r.target.split('?').next().unwrap();
        match (r.method.as_str(), path) {
            (_, "/token") => token(),
            ("POST", "/upload/drive/v3/files") => {
                pending = serde_json::from_slice::<Value>(&r.body).unwrap()["name"]
                    .as_str()
                    .unwrap()
                    .into();
                Reply {
                    headers: format!("Location: {origin}/session\r\n"),
                    ..reply(200, &json!({}))
                }
            }
            ("PUT", "/session") => {
                files.push((pending.clone(), r.body.clone()));
                reply(200, &json!({"id":files.len().to_string()}))
            }
            (_, "/drive/v3/changes/startPageToken") => reply(200, &json!({"startPageToken":"0"})),
            (_, "/drive/v3/files") => reply(
                200,
                &json!({"files":files.iter().enumerate().map(|(i,(name,_))| json!({"id":i.to_string(), "name":name})).collect::<Vec<_>>()}),
            ),
            (_, "/drive/v3/changes") => reply(
                200,
                &json!({"newStartPageToken":"0", "changes":files.iter().enumerate().map(|(i,(name,_))| json!({"fileId":i.to_string(), "file":{"id":i.to_string(), "name":name}})).collect::<Vec<_>>()}),
            ),
            _ => {
                let id: usize = path.rsplit('/').next().unwrap().parse().unwrap();
                Reply {
                    status: 200,
                    body: files[id].1.clone(),
                    headers: String::new(),
                }
            }
        }
    });
    let root = tempfile::tempdir().unwrap();
    let mut a = tinytrac_engine::Engine::open(root.path().join("a")).unwrap();
    let mut b = tinytrac_engine::Engine::open(root.path().join("b")).unwrap();
    for engine in [&mut a, &mut b] {
        engine
            .set_sync_target(Some(tinytrac_engine::SyncTarget::Remote {
                id: "account".into(),
                label: "Google".into(),
            }))
            .unwrap();
    }
    a.create_tag("A").unwrap();
    b.create_tag("B").unwrap();
    let ca = a.sync_context().unwrap().unwrap();
    let cb = b.sync_context().unwrap().unwrap();
    let mut da = Drive::new(
        client(),
        Secret::new("refresh".into()),
        ca,
        server.endpoints.clone(),
    );
    let mut db = Drive::new(
        client(),
        Secret::new("refresh".into()),
        cb,
        server.endpoints.clone(),
    );
    for _ in 0..3 {
        assert!(a.sync(&mut da).unwrap().skipped.is_empty());
        assert!(b.sync(&mut db).unwrap().skipped.is_empty());
    }
    assert_eq!(a.tags().unwrap(), b.tags().unwrap());
    assert_eq!(a.tags().unwrap().len(), 2);
}

#[test]
fn cancellation_interrupts_an_incomplete_callback_and_releases_port() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut address = String::new();
    let mut sender = None;
    let result = authorize(&client(), &Endpoints::default(), &cancel, |url| {
        let p = params(url.split_once('?').unwrap().1);
        address = p["redirect_uri"]
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .into();
        let mut stream = TcpStream::connect(&address).unwrap();
        stream.write_all(b"GET /callback?").unwrap();
        let cancel = cancel.clone();
        sender = Some(thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            thread::sleep(Duration::from_millis(150));
            drop(stream);
        }));
        Ok(())
    });
    assert!(result.unwrap().is_none());
    sender.unwrap().join().unwrap();
    assert!(TcpStream::connect(address).is_err());
}

#[test]
fn a_second_401_stops_and_expiring_tokens_refresh_early() {
    let server = Server::new(|r, _| {
        if r.target == "/token" {
            token()
        } else {
            reply(401, &json!({}))
        }
    });
    let error = server.drive().read("file").unwrap_err();
    assert!(matches!(error, Error::AuthRequired(_)));
    assert!(
        error
            .to_string()
            .contains("Google no longer accepts this sign-in; sign in again")
    );
    assert_eq!(server.requests.lock().unwrap().len(), 4);
    let server = Server::new(|r, _| {
        if r.target == "/token" {
            reply(200, &json!({"access_token":"access", "expires_in":20}))
        } else {
            reply(200, &json!({}))
        }
    });
    let mut drive = server.drive();
    drive.read("file").unwrap();
    drive.read("file").unwrap();
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.target == "/token")
            .count(),
        2
    );
}

#[test]
fn failed_change_page_keeps_previous_cursor_and_failed_reads_remain_listed() {
    let mut changes = 0;
    let server = Server::new(move |r, _| {
        let path = r.target.split('?').next().unwrap();
        match path {
            "/token" => token(),
            "/drive/v3/changes/startPageToken" => reply(200, &json!({"startPageToken":"start"})),
            "/drive/v3/files" => reply(
                200,
                &json!({"files":[{"id":"old", "name":format!("other/{BATCH}")}]}),
            ),
            "/drive/v3/changes" => {
                changes += 1;
                match changes {
                    1 => reply(
                        200,
                        &json!({"nextPageToken":"second", "changes":[{"fileId":"old", "removed":true}]}),
                    ),
                    2 => {
                        assert_eq!(r.query()["pageToken"], "second");
                        reply(500, &json!({}))
                    }
                    _ => {
                        assert_eq!(r.query()["pageToken"], "start");
                        reply(200, &json!({"newStartPageToken":"start", "changes":[]}))
                    }
                }
            }
            _ => reply(503, &json!({})),
        }
    });
    let mut drive = server.drive();
    assert_eq!(drive.list().unwrap(), ["old"]);
    assert!(drive.list().is_err());
    assert_eq!(drive.list().unwrap(), ["old"]);
    assert!(drive.read("old").is_err());
    assert_eq!(drive.list().unwrap(), ["old"]);
}

#[test]
fn oversized_callback_is_closed_before_accepting_a_valid_one() {
    let server = Server::new(|_, _| {
        reply(
            200,
            &json!({"access_token":"access", "expires_in":3600, "scope":"https://www.googleapis.com/auth/drive.appdata"}),
        )
    });
    let result = authorize(
        &client(),
        &server.endpoints,
        &AtomicBool::new(false),
        |url| {
            let p = params(url.split_once('?').unwrap().1);
            callback(
                &p["redirect_uri"],
                &format!("?padding={}", "x".repeat(9000)),
            );
            callback(
                &p["redirect_uri"],
                &format!("?state={}&code=valid", p["state"]),
            );
            Ok(())
        },
    );
    assert!(result.unwrap_err().to_string().contains("no refresh token"));
}

#[test]
fn invalid_publication_names_fail_before_http() {
    let server = Server::new(|_, _| panic!("invalid names must not make requests"));
    let mut drive = server.drive();
    for name in ["", ".json", "batch.txt", "nested/batch.json", "/batch.json"] {
        assert!(drive.publish(name, b"batch").is_err());
    }
    assert!(server.requests.lock().unwrap().is_empty());
}

#[test]
fn invalid_callbacks_return_400_then_genuine_callback_succeeds() {
    let server = Server::new(|r, _| {
        if r.target == "/token" {
            token()
        } else {
            reply(
                200,
                &json!({"user":{"permissionId":"account", "emailAddress":"me@example.com"}}),
            )
        }
    });
    let mut browser = None;
    let result = authorize(
        &client(),
        &server.endpoints,
        &AtomicBool::new(false),
        |url| {
            let p = params(url.split_once('?').unwrap().1);
            browser = Some(thread::spawn(move || {
                let address = p["redirect_uri"]
                    .strip_prefix("http://")
                    .unwrap()
                    .split('/')
                    .next()
                    .unwrap();
                for query in [
                    "state=wrong&code=probe".to_owned(),
                    "state=wrong&error=access_denied".to_owned(),
                    format!("state={}&code=a&code=b", p["state"]),
                    format!("state={}&state={}&code=a", p["state"], p["state"]),
                    format!("state={}", p["state"]),
                ] {
                    let mut stream = TcpStream::connect(address).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    write!(
                        stream,
                        "GET /callback?{query} HTTP/1.1\r\nHost: localhost\r\n\r\n"
                    )
                    .unwrap();
                    let mut response = String::new();
                    stream.read_to_string(&mut response).unwrap();
                    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
                }
                callback(
                    &p["redirect_uri"],
                    &format!("?state={}&code=genuine", p["state"]),
                );
            }));
            Ok(())
        },
    )
    .unwrap()
    .unwrap();
    browser.unwrap().join().unwrap();
    assert_eq!(result.account.id, "account");
    assert_eq!(
        params(std::str::from_utf8(&server.requests.lock().unwrap()[0].body).unwrap())["code"],
        "genuine"
    );
}

#[test]
fn authorization_requires_appdata_scope() {
    for scope in [
        None,
        Some(""),
        Some("openid"),
        Some("https://www.googleapis.com/auth/drive.appdata.extra"),
        Some("openid https://www.googleapis.com/auth/drive.appdata email"),
    ] {
        let server = Server::new(move |r, _| {
            if r.target == "/token" {
                reply(
                    200,
                    &json!({"access_token":"access", "expires_in":3600, "refresh_token":"refresh", "scope":scope}),
                )
            } else {
                reply(
                    200,
                    &json!({"user":{"permissionId":"account", "emailAddress":"me@example.com"}}),
                )
            }
        });
        let result = authorize(
            &client(),
            &server.endpoints,
            &AtomicBool::new(false),
            |url| {
                let p = params(url.split_once('?').unwrap().1);
                callback(
                    &p["redirect_uri"],
                    &format!("?state={}&code=code", p["state"]),
                );
                Ok(())
            },
        );
        if scope.is_some_and(|s| s.starts_with("openid https://")) {
            assert!(result.unwrap().is_some());
        } else {
            let error = result.unwrap_err();
            assert!(matches!(error, Error::AuthRequired(_)));
            assert!(
                error
                    .to_string()
                    .contains("app-data access was not granted")
            );
            assert_eq!(server.requests.lock().unwrap().len(), 1);
        }
    }
}

#[test]
fn permanent_token_and_permission_errors_require_action() {
    for reason in ["invalid_client", "unauthorized_client", "invalid_scope"] {
        let server = Server::new(move |_, _| reply(400, &json!({"error":reason})));
        let error = server.drive().read("file").unwrap_err();
        assert!(matches!(error, Error::AuthRequired(_)));
        assert!(error.to_string().contains("sign-in configuration"));
        assert!(!error.to_string().contains("will retry"));
    }
    for reason in [
        "insufficientPermissions",
        "insufficientFilePermissions",
        "appNotAuthorizedToFile",
        "domainPolicy",
    ] {
        let server = Server::new(move |r, _| {
            if r.target == "/token" {
                token()
            } else {
                reply(403, &json!({"error":{"errors":[{"reason":reason}]}}))
            }
        });
        let error = server.drive().read("file").unwrap_err();
        assert!(matches!(error, Error::AuthRequired(_)));
        assert!(error.to_string().contains("Google denied Drive access"));
        assert!(!error.to_string().contains("will retry"));
    }
}

#[test]
fn upload_session_401_retries_once() {
    for final_status in [200, 401] {
        let mut puts = 0;
        let server = Server::new(move |r, origin| {
            if r.target == "/token" {
                return token();
            }
            if r.method == "POST" {
                return Reply {
                    headers: format!("Location: {origin}/session\r\n"),
                    ..reply(200, &json!({}))
                };
            }
            assert_eq!(r.method, "PUT");
            assert_eq!(r.target, "/session");
            assert_eq!(r.body, b"batch");
            puts += 1;
            reply(if puts == 1 { 401 } else { final_status }, &json!({}))
        });
        let result = server.drive().publish(BATCH, b"batch");
        if final_status == 200 {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(Error::AuthRequired(_))));
        }
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.iter().filter(|r| r.method == "PUT").count(), 2);
        assert_eq!(requests.iter().filter(|r| r.target == "/token").count(), 2);
        assert_eq!(requests.len(), 5);
    }
}

// Shared with Drive's private clock/index regression tests.
pub(super) fn enumeration_server() -> Server {
    let mut enumerations = 0;
    Server::new(move |r, _| match r.target.split('?').next().unwrap() {
        "/token" => token(),
        "/drive/v3/changes/startPageToken" => reply(200, &json!({"startPageToken":"start"})),
        "/drive/v3/files" => {
            enumerations += 1;
            reply(
                200,
                &json!({"files":[{"id":format!("enumeration-{enumerations}"), "name":"peer/batch.json"}]}),
            )
        }
        "/drive/v3/changes" => reply(200, &json!({"newStartPageToken":"next", "changes":[]})),
        _ => panic!("unexpected request"),
    })
}

#[test]
fn idle_connection_expires_before_the_next_callback() {
    let server = Server::new(|_, _| token());
    let mut idle = None;
    let result = oauth::authorize_with_timeout(
        &client(),
        &server.endpoints,
        &AtomicBool::new(false),
        |url| {
            let p = params(url.split_once('?').unwrap().1);
            let address = p["redirect_uri"]
                .strip_prefix("http://")
                .unwrap()
                .split('/')
                .next()
                .unwrap();
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(b"GET /callback?").unwrap();
            idle = Some(stream);
            callback(
                &p["redirect_uri"],
                &format!("?state={}&error=access_denied", p["state"]),
            );
            Ok(())
        },
        Duration::from_millis(1500),
    );
    assert_eq!(result.unwrap_err().to_string(), "Google sign-in denied");
    let mut bytes = [0; 1];
    assert_eq!(idle.unwrap().read(&mut bytes).unwrap(), 0);
}
