//! Immutable app-data batches and an incremental Drive index.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use serde::Deserialize;
use tinytrac_engine::{Result, SyncContext, Transport};

use crate::{Client, Endpoints, Secret, http};

/// One account/device binding. Reuse it across sync runs to retain the change index.
/// Names such as `device/batch.json` are labels: appDataFolder has no directories here.
#[derive(Debug)]
pub struct Drive {
    client: Client,
    refresh_token: Secret,
    context: SyncContext,
    endpoints: Endpoints,
    agent: ureq::Agent,
    access: Option<(Secret, Instant)>,
    index: BTreeMap<String, String>,
    cursor: Option<String>,
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Post,
    Put,
}

#[derive(Deserialize)]
struct File {
    id: String,
    name: String,
    #[serde(default)]
    trashed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Files {
    #[serde(default)]
    files: Vec<File>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangePage {
    #[serde(default)]
    changes: Vec<Change>,
    next_page_token: Option<String>,
    new_start_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Change {
    file_id: String,
    #[serde(default)]
    removed: bool,
    file: Option<File>,
}

impl Drive {
    pub fn new(
        client: Client,
        refresh_token: Secret,
        context: SyncContext,
        endpoints: Endpoints,
    ) -> Self {
        Self {
            client,
            refresh_token,
            context,
            endpoints,
            agent: http::agent(),
            access: None,
            index: BTreeMap::new(),
            cursor: None,
        }
    }

    fn refresh(&mut self) -> Result<String> {
        self.access = None;
        let tokens = http::token(
            &self.agent,
            &self.client,
            &self.endpoints,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", self.refresh_token.expose()),
            ],
        )?;
        let lifetime = tokens.expires_in.saturating_sub(30).min(86_400);
        let token = Secret::new(tokens.access_token);
        let auth = http::bearer(&token);
        self.access = Some((token, Instant::now() + Duration::from_secs(lifetime)));
        Ok(auth)
    }

    fn request(
        &mut self,
        method: Method,
        url: &str,
        content_type: &str,
        body: &[u8],
    ) -> Result<http::Response> {
        let auth = match &self.access {
            Some((token, expiry)) if *expiry > Instant::now() => http::bearer(token),
            _ => self.refresh()?,
        };
        let response = self.send(method, url, content_type, body, &auth)?;
        if response.status().as_u16() != 401 {
            return Ok(response);
        }
        drop(response);
        let auth = self.refresh()?;
        self.send(method, url, content_type, body, &auth)
    }

    fn send(
        &self,
        method: Method,
        url: &str,
        content_type: &str,
        body: &[u8],
        auth: &str,
    ) -> Result<http::Response> {
        match method {
            Method::Post => self
                .agent
                .post(url)
                .header("Authorization", auth)
                .content_type(content_type)
                .send(body),
            Method::Put => self
                .agent
                .put(url)
                .header("Authorization", auth)
                .content_type(content_type)
                .send(body),
            Method::Get => self.agent.get(url).header("Authorization", auth).call(),
        }
        .map_err(http::network)
    }

    fn get(&mut self, path: &str, pairs: &[(&str, &str)]) -> Result<http::Response> {
        let url = http::query(&format!("{}/drive/v3/{path}", self.endpoints.drive), pairs);
        self.request(Method::Get, &url, "", &[])
    }

    fn bootstrap(&mut self) -> Result<()> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Start {
            start_page_token: String,
        }
        let start: Start = http::json(&mut http::check(self.get("changes/startPageToken", &[])?)?)?;
        let mut index = BTreeMap::new();
        let mut page = String::new();
        let mut seen = BTreeSet::new();
        loop {
            let mut pairs = vec![
                ("spaces", "appDataFolder"),
                ("fields", "nextPageToken,files(id,name)"),
                ("pageSize", "1000"),
                ("q", "trashed = false"),
            ];
            if !page.is_empty() {
                pairs.push(("pageToken", &page));
            }
            let files: Files = http::json(&mut http::check(self.get("files", &pairs)?)?)?;
            for file in files.files {
                self.apply(&mut index, file);
            }
            match files.next_page_token {
                Some(next) => {
                    record_page(&mut seen, &next)?;
                    page = next;
                }
                None => break,
            }
        }
        if start.start_page_token.is_empty() {
            return Err(http::remote("Google returned no change token"));
        }
        // Commit only complete enumerations. On failure the old cursor is retryable.
        self.index = index;
        self.cursor = Some(start.start_page_token);
        Ok(())
    }

    fn changes(&mut self, mut page: String) -> Result<()> {
        let mut index = self.index.clone();
        let mut seen = BTreeSet::new();
        loop {
            let response = self.get("changes", &[("spaces", "appDataFolder"), ("pageToken", &page), ("pageSize", "1000"), ("fields", "nextPageToken,newStartPageToken,changes(fileId,removed,file(id,name,trashed))")])?;
            // Google can reject an obsolete/invalid page token. Rebuild once, never loop.
            if matches!(response.status().as_u16(), 400 | 410) {
                return self.bootstrap();
            }
            let changes: ChangePage = http::json(&mut http::check(response)?)?;
            for change in changes.changes {
                index.remove(&change.file_id);
                if !change.removed
                    && let Some(file) = change.file
                {
                    self.apply(&mut index, file);
                }
            }
            if let Some(next) = changes.next_page_token {
                record_page(&mut seen, &next)?;
                page = next;
            } else {
                let cursor = changes
                    .new_start_page_token
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| http::remote("Google returned no change token"))?;
                self.index = index;
                self.cursor = Some(cursor);
                return Ok(());
            }
        }
    }

    fn apply(&self, index: &mut BTreeMap<String, String>, file: File) {
        index.remove(&file.id);
        if !file.trashed
            && batch_name(&file.name).is_some_and(|device| device != self.context.device())
        {
            index.insert(file.id, file.name);
        }
    }
}

fn record_page(seen: &mut BTreeSet<String>, page: &str) -> Result<()> {
    if page.is_empty() || !seen.insert(page.into()) {
        return Err(http::remote("Google returned an invalid page token"));
    }
    Ok(())
}

// Names only identify the publishing device; the engine validates batch contents.
fn batch_name(name: &str) -> Option<&str> {
    let (device, batch) = name.split_once('/')?;
    let file = batch.strip_suffix(".json")?;
    (!device.is_empty() && !file.is_empty() && !file.contains('/')).then_some(device)
}

impl Transport for Drive {
    fn context(&self) -> &SyncContext {
        &self.context
    }

    fn publish(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        let name = format!("{}/{name}", self.context.device());
        if batch_name(&name).is_none() {
            return Err(http::remote("Invalid Drive batch name"));
        }
        let metadata =
            serde_json::to_vec(&serde_json::json!({"name": name, "parents": ["appDataFolder"]}))?;
        let url = http::query(
            &format!("{}/upload/drive/v3/files", self.endpoints.drive),
            &[("uploadType", "resumable")],
        );
        let response =
            http::check(self.request(Method::Post, &url, "application/json", &metadata)?)?;
        let location = response
            .headers()
            .get("location")
            .and_then(|s| s.to_str().ok())
            .ok_or_else(|| http::remote("Google returned no upload session"))?
            .to_owned();
        // A session is a bearer capability; only send it back to the configured origin.
        let origin = format!("{}/", self.endpoints.drive.trim_end_matches('/'));
        if !location.starts_with(&origin) {
            return Err(http::remote("Google returned an invalid upload session"));
        }
        let response = self.request(Method::Put, &location, "application/octet-stream", bytes)?;
        if !matches!(response.status().as_u16(), 200 | 201) {
            http::check(response)?;
            return Err(http::remote("Google upload was not completed; will retry"));
        }
        Ok(())
    }

    fn list(&mut self) -> Result<Vec<String>> {
        match self.cursor.clone() {
            Some(cursor) => self.changes(cursor)?,
            None => self.bootstrap()?,
        }
        Ok(self.index.keys().cloned().collect())
    }

    fn read(&mut self, key: &str) -> Result<Vec<u8>> {
        // Drive ids use URL-safe ASCII; never allow a key to alter the request path.
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(http::remote("Invalid Drive file id"));
        }
        let mut response = http::check(self.get(&format!("files/{key}"), &[("alt", "media")])?)?;
        response
            .body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_to_vec()
            .map_err(http::network)
    }
}

#[cfg(test)]
mod tests {
    use super::batch_name;

    #[test]
    fn labels_only_require_a_device_and_json_file() {
        for name in ["peer/batch.json", "peer/future-format.json", "peer/☀.json"] {
            assert_eq!(batch_name(name), Some("peer"));
        }
        for name in [
            "/batch.json",
            "peer/.json",
            "peer/batch.txt",
            "peer/nested/batch.json",
            "batch.json",
        ] {
            assert_eq!(batch_name(name), None);
        }
    }
}
