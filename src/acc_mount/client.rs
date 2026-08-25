use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::sync::{Arc, OnceLock, Mutex};
use std::collections::HashMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio::io::AsyncWriteExt;
use xet_client::cas_client::auth::{TokenRefresher, TokenInfo, AuthError};

use crate::error::{Result, Error};
use crate::hub_api::{HubOps, TreeEntry, HeadFileInfo, BatchOp, SourceKind};

#[derive(Clone, Debug)]
pub struct UploadedFileInfo {
    pub size: u64,
    pub sha256: Option<String>,
}

pub static UPLOADED_FILE_INFOS: OnceLock<Mutex<HashMap<String, UploadedFileInfo>>> = OnceLock::new();

pub fn record_uploaded_info(hash: String, size: u64, sha256: Option<String>) {
    UPLOADED_FILE_INFOS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(hash, UploadedFileInfo { size, sha256 });
}

pub fn get_uploaded_info(hash: &str) -> Option<UploadedFileInfo> {
    UPLOADED_FILE_INFOS
        .get()
        .and_then(|m| m.lock().unwrap().get(hash).cloned())
}

#[derive(Clone, Debug)]
pub struct TokenState {
    pub raw_token: Option<String>,
    pub cas_token: Option<String>,
    pub last_known_refresh_token: Option<String>,
    pub expires_at: Option<SystemTime>,
}

pub struct AccHubClient {
    client: Client,
    hub_endpoint: String,
    token_file: Option<PathBuf>,
    token_state: Arc<RwLock<TokenState>>,
    source_kind: SourceKind,
    project_slug: String,
}

async fn parse_token_from_file(path: &Path) -> Result<String> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| Error::Xet(format!("Failed to read token file {:?}: {}", path, e)))?;
    let trimmed = content.trim();
    if trimmed.starts_with('{') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(token) = find_json_key(&val, "token") {
                return Ok(token);
            }
        }
        Err(Error::Xet(format!("Token file {:?} is JSON but no 'token' key was found", path)))
    } else {
        Ok(trimmed.to_string())
    }
}

fn find_json_key(val: &serde_json::Value, key_to_find: &str) -> Option<String> {
    match val {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get(key_to_find) {
                return Some(s.clone());
            }
            for (_k, v) in map {
                if let Some(res) = find_json_key(v, key_to_find) {
                    return Some(res);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                if let Some(res) = find_json_key(v, key_to_find) {
                    return Some(res);
                }
            }
            None
        }
        _ => None,
    }
}

async fn write_token_to_file(path: &Path, new_token: &str) -> Result<()> {
    let content = tokio::fs::read_to_string(path).await.unwrap_or_default();
    let trimmed = content.trim();
    if trimmed.starts_with('{') {
        if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if update_json_key(&mut val, "token", new_token) {
                let serialized = serde_json::to_string_pretty(&val)
                    .map_err(|e| Error::Xet(format!("Failed to serialize token JSON: {}", e)))?;
                tokio::fs::write(path, serialized)
                    .await
                    .map_err(|e| Error::Xet(format!("Failed to write token JSON to disk: {}", e)))?;
                return Ok(());
            }
        }
    }
    tokio::fs::write(path, new_token)
        .await
        .map_err(|e| Error::Xet(format!("Failed to write plain token to disk: {}", e)))?;
    Ok(())
}

fn update_json_key(val: &mut serde_json::Value, key_to_find: &str, new_value: &str) -> bool {
    match val {
        serde_json::Value::Object(map) => {
            if map.contains_key(key_to_find) {
                map.insert(key_to_find.to_string(), serde_json::Value::String(new_value.to_string()));
                return true;
            }
            for (_k, v) in map {
                if update_json_key(v, key_to_find, new_value) {
                    return true;
                }
            }
            false
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                if update_json_key(v, key_to_find, new_value) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

impl AccHubClient {
    pub async fn new(
        hub_endpoint: &str,
        token: Option<&str>,
        token_file: Option<&Path>,
        source_kind: SourceKind,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| Error::Xet(format!("Failed to build reqwest client: {e}")))?;

        let project_slug = match &source_kind {
            SourceKind::Bucket { bucket_id } => bucket_id.clone(),
            SourceKind::Repo { repo_id, .. } => repo_id.clone(),
        };

        let final_endpoint = std::env::var("ACC_ENDPOINT")
            .unwrap_or_else(|_| hub_endpoint.trim_end_matches('/').to_string());
        
        let initial_token = match std::env::var("ACC_TOKEN").ok() {
            Some(t) => Some(t),
            None => match token {
                Some(t) => Some(t.to_string()),
                None => {
                    if let Some(p) = token_file {
                        parse_token_from_file(p).await.ok()
                    } else {
                        None
                    }
                }
            }
        };

        let cas_token = initial_token.clone().map(|t| {
            format!("xet_session_prj_{}_{}", project_slug, t)
        });

        tracing::info!(
            "Initializing Accelerator Hub Client (endpoint={}, project_slug={})",
            final_endpoint,
            project_slug
        );

        let expires_at = if initial_token.as_ref().map(|t| t.starts_with("eyJ")).unwrap_or(false) {
            // JWT format: set long expiration or parse JWT. For safety, set far future
            Some(SystemTime::now() + std::time::Duration::from_secs(315360000))
        } else {
            // Refresh token format or empty: expired, will trigger active refresh on first request
            None
        };

        let token_state = Arc::new(RwLock::new(TokenState {
            raw_token: initial_token.clone(),
            cas_token,
            last_known_refresh_token: initial_token,
            expires_at,
        }));

        Ok(Self {
            client,
            hub_endpoint: final_endpoint,
            token_file: token_file.map(|p| p.to_path_buf()),
            token_state,
            source_kind,
            project_slug,
        })
    }

    pub async fn get_or_refresh_token(&self) -> Result<TokenState> {
        {
            let state = self.token_state.read().await;
            if let Some(expires) = state.expires_at {
                if expires.duration_since(SystemTime::now()).unwrap_or(std::time::Duration::ZERO).as_secs() > 600 {
                    return Ok(state.clone());
                }
            }
        }

        let mut state = self.token_state.write().await;
        if let Some(expires) = state.expires_at {
            if expires.duration_since(SystemTime::now()).unwrap_or(std::time::Duration::ZERO).as_secs() > 600 {
                return Ok(state.clone());
            }
        }

        let token_from_disk = if let Some(ref path) = self.token_file {
            parse_token_from_file(path).await.ok()
        } else {
            None
        };

        if let Some(ref disk_token) = token_from_disk {
            if Some(disk_token.clone()) != state.last_known_refresh_token {
                if disk_token.starts_with("eyJ") {
                    let cas_token = format!("xet_session_prj_{}_{}", self.project_slug, disk_token);
                    state.raw_token = Some(disk_token.clone());
                    state.cas_token = Some(cas_token);
                    state.last_known_refresh_token = Some(disk_token.clone());
                    state.expires_at = Some(SystemTime::now() + std::time::Duration::from_secs(2700));
                    return Ok(state.clone());
                } else {
                    state.last_known_refresh_token = Some(disk_token.clone());
                }
            }
        }

        let current_refresh_token = state.last_known_refresh_token.clone()
            .or_else(|| std::env::var("ACC_TOKEN").ok());

        if let Some(ref ref_token) = current_refresh_token {
            if ref_token.starts_with("eyJ") {
                let cas_token = format!("xet_session_prj_{}_{}", self.project_slug, ref_token);
                state.raw_token = Some(ref_token.clone());
                state.cas_token = Some(cas_token);
                state.last_known_refresh_token = Some(ref_token.clone());
                state.expires_at = Some(SystemTime::now() + std::time::Duration::from_secs(315360000));
                return Ok(state.clone());
            }

            let url = format!("{}/api/v1/oauth/device/access-token/", self.hub_endpoint);
            
            #[derive(Serialize)]
            struct RefreshRequest {
                refresh_token: String,
            }

            #[derive(Deserialize)]
            struct RefreshResponse {
                access_token: String,
                refresh_token: String,
            }

            tracing::info!("Rotating Accelerator Refresh Token against {}", url);
            let resp = self.client.post(&url)
                .json(&RefreshRequest { refresh_token: ref_token.clone() })
                .send()
                .await
                .map_err(|e| Error::Xet(format!("Refresh request failed: {e}")))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(Error::Xet(format!("Refresh request failed ({}): {}", status, text)));
            }

            let body: RefreshResponse = resp.json().await
                .map_err(|e| Error::Xet(format!("Failed to parse refresh JSON: {e}")))?;

            if let Some(ref path) = self.token_file {
                if let Err(e) = write_token_to_file(path, &body.refresh_token).await {
                    tracing::error!("Failed to write rotated token to disk: {:?}", e);
                }
            }

            let cas_token = format!("xet_session_prj_{}_{}", self.project_slug, body.access_token);
            state.raw_token = Some(body.access_token);
            state.cas_token = Some(cas_token);
            state.last_known_refresh_token = Some(body.refresh_token);
            state.expires_at = Some(SystemTime::now() + std::time::Duration::from_secs(2700));
            
            return Ok(state.clone());
        }

        Err(Error::Xet("No token or token_file available for authorization".to_string()))
    }
}

pub struct AccTokenRefresher {
    client: Arc<AccHubClient>,
}

impl AccTokenRefresher {
    pub fn new(client: Arc<AccHubClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TokenRefresher for AccTokenRefresher {
    async fn refresh(&self) -> std::result::Result<TokenInfo, AuthError> {
        let token_info = self.client.get_or_refresh_token().await
            .map_err(|e| AuthError::TokenRefreshFailure(e.to_string()))?;
        
        let cas_token = token_info.cas_token.ok_or_else(|| AuthError::TokenRefreshFailure("No CAS token".to_string()))?;
        let expires_epoch = token_info.expires_at
            .unwrap_or(SystemTime::now())
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok((cas_token, expires_epoch))
    }
}


#[async_trait]
impl HubOps for AccHubClient {
    async fn list_tree(&self, prefix: &str) -> Result<Vec<TreeEntry>> {
        // Absolute node path for the directory we want to list.
        // It must start with the project slug prefix.
        let absolute_path = if prefix.is_empty() {
            self.project_slug.clone()
        } else {
            format!("{}/{}", self.project_slug, prefix)
        };

        // Base64 encode the absolute path
        use base64::prelude::*;
        let b64_path = BASE64_STANDARD.encode(absolute_path.as_bytes());

        let url = format!(
            "{}/api/v1/aterm-cli/{}/node-items/{}/",
            self.hub_endpoint, self.project_slug, b64_path
        );

        let token_info = self.get_or_refresh_token().await?;
        let mut req = self.client.get(&url);
        if let Some(ref t) = token_info.raw_token {
            req = req.bearer_auth(t).header("x-authorization", t);
        }

        let resp = req.send().await.map_err(|e| Error::Xet(format!("List request failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Xet(format!("List request failed ({}): {}", status, text)));
        }

        #[derive(Deserialize)]
        struct BackendNode {
            absolute_node_name: String,
            created_at: String,
        }
        #[derive(Deserialize)]
        struct BackendObject {
            filename: String,
            created_at: String,
            file_size: Option<u64>,
            merkle_hash: Option<String>,
        }
        #[derive(Deserialize)]
        struct NodeItemsResponse {
            nodes: Vec<BackendNode>,
            objects: Vec<BackendObject>,
        }

        let body: NodeItemsResponse = resp.json().await.map_err(|e| Error::Xet(format!("Failed to parse list JSON: {e}")))?;

        let mut entries = Vec::new();

        // 1. Process directory nodes
        for node in body.nodes {
            let relative_path = if node.absolute_node_name.starts_with(&self.project_slug) {
                let stripped = node.absolute_node_name.trim_start_matches(&self.project_slug);
                stripped.trim_start_matches('/').to_string()
            } else {
                node.absolute_node_name.clone()
            };

            entries.push(TreeEntry {
                path: relative_path,
                entry_type: "directory".to_string(),
                size: None,
                xet_hash: None,
                oid: None,
                mtime: Some(node.created_at),
            });
        }

        // 2. Process file objects
        for obj in body.objects {
            let relative_path = if obj.filename.starts_with(&self.project_slug) {
                let stripped = obj.filename.trim_start_matches(&self.project_slug);
                stripped.trim_start_matches('/').to_string()
            } else {
                obj.filename.clone()
            };

            entries.push(TreeEntry {
                path: relative_path,
                entry_type: "file".to_string(),
                size: obj.file_size,
                xet_hash: obj.merkle_hash,
                oid: None,
                mtime: Some(obj.created_at),
            });
        }

        Ok(entries)
    }

    async fn head_file(&self, path: &str) -> Result<Option<HeadFileInfo>> {
        // Query the stat of the file: POST /api/v1/projects/{project_slug}/file-stat/
        // Body: {"filename": "/path"}
        let formatted_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{}", path)
        };

        let url = format!(
            "{}/api/v1/aterm-cli/{}/file-stat/",
            self.hub_endpoint, self.project_slug
        );

        #[derive(Serialize)]
        struct StatRequest {
            filename: String,
        }

        let token_info = self.get_or_refresh_token().await?;
        let mut req = self.client.post(&url).json(&StatRequest { filename: formatted_path });
        if let Some(ref t) = token_info.raw_token {
            req = req.bearer_auth(t).header("x-authorization", t);
        }

        let resp = req.send().await.map_err(|e| Error::Xet(format!("Stat request failed: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Xet(format!("Stat request failed ({}): {}", status, text)));
        }

        #[derive(Deserialize)]
        struct StatResponse {
            size: u64,
            etag: String,
            last_modified: String,
            merkle_hash: Option<String>,
        }

        let body: StatResponse = resp.json().await.map_err(|e| Error::Xet(format!("Failed to parse stat JSON: {e}")))?;

        Ok(Some(HeadFileInfo {
            xet_hash: body.merkle_hash,
            etag: Some(body.etag),
            size: Some(body.size),
            last_modified: Some(body.last_modified),
        }))
    }

    async fn batch_operations(&self, ops: &[BatchOp]) -> Result<()> {
        #[derive(Serialize)]
        struct XetObjectRegistrationItem {
            filename: String,
            merkle_hash: String,
            sha256: String,
            file_size: u64,
            content_type: Option<String>,
        }

        #[derive(Serialize)]
        struct XetBulkRegistrationModel {
            items: Vec<XetObjectRegistrationItem>,
        }

        #[derive(Serialize)]
        struct XetBulkDeleteModel {
            filenames: Vec<String>,
        }

        let mut items = Vec::new();
        let mut delete_filenames = Vec::new();

        for op in ops {
            match op {
                BatchOp::AddFile { path, xet_hash, content_type, .. } => {
                    let cached_info = get_uploaded_info(xet_hash).ok_or_else(|| {
                        Error::Xet(format!(
                            "Cache miss for xet_hash={}. Size and SHA256 must be cached before registration.",
                            xet_hash
                        ))
                    })?;

                    let file_size = cached_info.size;
                    let sha256_val = cached_info.sha256.clone().unwrap_or_else(|| xet_hash.clone());
                    tracing::debug!("Resolved size from cache for xet_hash={}: {} bytes, sha256={}", xet_hash, file_size, sha256_val);

                    let absolute_filename = if path.starts_with('/') {
                        format!("{}{}", self.project_slug, path)
                    } else {
                        format!("{}/{}", self.project_slug, path)
                    };

                    items.push(XetObjectRegistrationItem {
                        filename: absolute_filename,
                        merkle_hash: xet_hash.clone(),
                        sha256: sha256_val,
                        file_size: file_size,
                        content_type: content_type.clone(),
                    });
                }
                BatchOp::DeleteFile { path } => {
                    let absolute_filename = if path.starts_with('/') {
                        format!("{}{}", self.project_slug, path)
                    } else {
                        format!("{}/{}", self.project_slug, path)
                    };
                    delete_filenames.push(absolute_filename);
                }
            }
        }

        // Deduplicate: do not delete files that are being added in the same batch.
        // This handles cases like atomic rename where a file is registered and then
        // its previous instance is queued for deletion.
        delete_filenames.retain(|d| !items.iter().any(|i| i.filename == *d));

        if !items.is_empty() {
            let url = format!(
                "{}/api/xet-cas/v1/cas/bulk-register",
                self.hub_endpoint
            );

            let token_info = self.get_or_refresh_token().await?;
            let mut req = self.client.post(&url).json(&XetBulkRegistrationModel { items });
            if let Some(ref t) = token_info.cas_token {
                req = req.bearer_auth(t).header("x-authorization", t);
            }

            let resp = req.send().await.map_err(|e| Error::Xet(format!("Bulk register failed: {e}")))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(Error::Xet(format!("Bulk register failed ({}): {}", status, text)));
            }
        }

        if !delete_filenames.is_empty() {
            let url = format!(
                "{}/api/xet-cas/v1/cas/bulk-delete",
                self.hub_endpoint
            );

            let token_info = self.get_or_refresh_token().await?;
            let mut req = self.client.post(&url).json(&XetBulkDeleteModel { filenames: delete_filenames });
            if let Some(ref t) = token_info.cas_token {
                req = req.bearer_auth(t).header("x-authorization", t);
            }

            let resp = req.send().await.map_err(|e| Error::Xet(format!("Bulk delete failed: {e}")))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(Error::Xet(format!("Bulk delete failed ({}): {}", status, text)));
            }
        }

        Ok(())
    }

    async fn download_file_http(&self, path: &str, dest: &Path) -> Result<()> {
        let absolute_filename = if path.starts_with('/') {
            format!("{}{}", self.project_slug, path)
        } else {
            format!("{}/{}", self.project_slug, path)
        };

        let url = format!(
            "{}/api/v1/aterm-cli/{}/get-file-download-url/?filename={}",
            self.hub_endpoint, self.project_slug, absolute_filename
        );

        let token_info = self.get_or_refresh_token().await?;
        let mut req = self.client.get(&url);
        if let Some(ref t) = token_info.raw_token {
            req = req.bearer_auth(t).header("x-authorization", t);
        }

        let resp = req.send().await.map_err(|e| Error::Xet(format!("Failed to get download URL: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Xet(format!("Failed to get download URL ({}): {}", status, text)));
        }

        let download_url: String = resp.json().await.map_err(|e| Error::Xet(format!("Failed to parse download URL: {e}")))?;

        let file_resp = self.client.get(&download_url).send().await.map_err(|e| Error::Xet(format!("Download failed: {e}")))?;
        if !file_resp.status().is_success() {
            return Err(Error::Xet(format!("Download request failed: {}", file_resp.status())));
        }

        let mut file = tokio::fs::File::create(dest).await.map_err(Error::Io)?;
        let mut stream = file_resp.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Xet(format!("Stream chunk error: {e}")))?;
            file.write_all(&chunk).await.map_err(|e| Error::Io(e))?;
        }

        Ok(())
    }

    fn default_mtime(&self) -> SystemTime {
        SystemTime::now()
    }

    fn source(&self) -> &SourceKind {
        &self.source_kind
    }

    fn is_repo(&self) -> bool {
        matches!(self.source_kind, SourceKind::Repo { .. })
    }

    async fn probe_revision(&self) -> Result<String> {
        match &self.source_kind {
            SourceKind::Bucket { .. } => {
                let url = format!(
                    "{}/api/v1/aterm-cli/{}/revision-probe/?source_kind=bucket",
                    self.hub_endpoint, self.project_slug
                );

                let token_info = self.get_or_refresh_token().await?;
                let mut req = self.client.get(&url);
                if let Some(ref t) = token_info.raw_token {
                    req = req.bearer_auth(t).header("x-authorization", t);
                }

                let resp = req.send().await.map_err(|e| Error::Xet(format!("Revision probe request failed: {e}")))?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    return Err(Error::Xet(format!("Revision probe request failed ({}): {}", status, text)));
                }

                #[derive(Deserialize)]
                struct ProbeResponse {
                    revision: String,
                }

                let body: ProbeResponse = resp.json().await.map_err(|e| Error::Xet(format!("Failed to parse revision probe JSON: {e}")))?;
                Ok(body.revision)
            }
            SourceKind::Repo { .. } => {
                Err(Error::Xet("Revision probe is not implemented for Git repositories".to_string()))
            }
        }
    }
}
