//! Resolve `input_file` content parts on a Responses-API request into
//! ready-to-mount byte blobs.
//!
//! Walks `payload.input` for [`ResponseInputContentItem::InputFile`]
//! parts, resolves each through one of three sources (Files API
//! lookup, base64 data URL, or HTTP URL with SSRF protection), and
//! returns the bytes alongside a sanitized filename. The caller
//! (`apply_streaming_pipeline`) feeds the result into
//! [`ContainerSession::ingest_user_files`] so the shell tool sees the
//! files at `/mnt/data/<filename>` on its first command.
//!
//! Errors are surfaced as 400-eligible so callers can turn them into
//! a clear client response.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashSet;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    AppState,
    api_types::responses::{
        CreateResponsesPayload, EasyInputMessageContent, ResponseInputContentItem, ResponsesInput,
        ResponsesInputItem,
    },
    config::ContainersConfig,
};

/// One resolved input file, ready to write into `/mnt/data`.
#[derive(Debug, Clone)]
pub struct StagedFile {
    /// Sanitized basename. Empty path components, traversal segments,
    /// and absolute prefixes are stripped before this struct is built.
    /// On filename collision a `<sha8>-` prefix is added to keep the
    /// originals distinguishable, matching OpenAI's behavior.
    pub filename: String,
    /// File bytes.
    pub bytes: bytes::Bytes,
    /// MIME type when known. Comes from the Files API metadata for
    /// `file_id`, the data URL prefix for `file_data`, or the HTTP
    /// `Content-Type` header for `file_url`.
    pub content_type: Option<String>,
    /// Original Files-API id when the source was `file_id`. Used to
    /// cross-link the container file row back to the source upload
    /// and recorded for traceability.
    pub source_file_id: Option<String>,
}

/// Errors emitted by [`stage_input_files`].
///
/// All variants are 400-eligible from the client's perspective — the
/// caller is expected to translate to a `bad_request` API error with
/// a code drawn from `error_code()`.
#[derive(Debug, Error)]
pub enum StageError {
    #[error("input_file is missing any of file_id / file_data / file_url")]
    NoSource,
    #[error("input_file specifies multiple sources; pick one of file_id / file_data / file_url")]
    AmbiguousSource,
    #[error("input_file file_id '{0}' is not a valid file identifier")]
    InvalidFileId(String),
    #[error("input_file file_id '{0}' was not found")]
    FileNotFound(String),
    #[error("input_file file_data is not a valid data URL: {0}")]
    BadDataUrl(String),
    #[error("input_file file_data base64 decode failed: {0}")]
    BadBase64(String),
    #[error("input_file file_url is blocked or invalid: {0}")]
    BlockedUrl(String),
    #[error("input_file file_url fetch failed: {0}")]
    FetchFailed(String),
    #[error("input_file file_url HTTP {0}")]
    HttpStatus(u16),
    #[error(
        "input_file '{filename}' exceeds the {limit}-byte per-request input budget at {bytes} bytes"
    )]
    FileTooLarge {
        filename: String,
        bytes: u64,
        limit: u64,
    },
    #[error("request stages {count} input_file parts, exceeding the configured maximum of {limit}")]
    TooManyFiles { count: usize, limit: usize },
    #[error("input_file storage backend error: {0}")]
    Storage(String),
}

impl StageError {
    /// Stable machine code for the API error envelope.
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::NoSource | Self::AmbiguousSource => "invalid_input_file",
            Self::InvalidFileId(_) | Self::FileNotFound(_) => "invalid_input_file_id",
            Self::BadDataUrl(_) | Self::BadBase64(_) => "invalid_input_file_data",
            Self::BlockedUrl(_) | Self::FetchFailed(_) | Self::HttpStatus(_) => {
                "invalid_input_file_url"
            }
            Self::FileTooLarge { .. } => "input_file_too_large",
            Self::TooManyFiles { .. } => "too_many_input_files",
            Self::Storage(_) => "input_file_storage_error",
        }
    }
}

/// Walk `payload.input`, resolve every `input_file` part, and return
/// the staged files in encounter order.
///
/// Skips staging entirely when `config.enabled = false`. Skips
/// resolution when no `input_file` parts are present (returns `Ok([])`
/// without touching the network or DB).
pub async fn stage_input_files(
    state: &AppState,
    payload: &CreateResponsesPayload,
    config: &ContainersConfig,
) -> Result<Vec<StagedFile>, StageError> {
    if !config.enabled {
        return Ok(Vec::new());
    }

    let parts = collect_input_file_parts(payload);
    if parts.is_empty() {
        return Ok(Vec::new());
    }

    if parts.len() > config.max_input_files_per_request {
        return Err(StageError::TooManyFiles {
            count: parts.len(),
            limit: config.max_input_files_per_request,
        });
    }

    let mut total_bytes: u64 = 0;
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<StagedFile> = Vec::with_capacity(parts.len());

    for part in parts {
        let resolved = resolve_one(state, &part).await?;
        let bytes_len = resolved.bytes.len() as u64;
        if total_bytes.saturating_add(bytes_len) > config.max_input_bytes_per_request {
            return Err(StageError::FileTooLarge {
                filename: resolved.filename,
                bytes: total_bytes + bytes_len,
                limit: config.max_input_bytes_per_request,
            });
        }
        total_bytes += bytes_len;

        let filename = dedupe_filename(resolved.filename, &resolved.bytes, &mut seen);

        out.push(StagedFile {
            filename,
            bytes: resolved.bytes,
            content_type: resolved.content_type,
            source_file_id: resolved.source_file_id,
        });
    }

    Ok(out)
}

/// Internal shape returned by `resolve_one` — same as `StagedFile`
/// except the filename hasn't been deduped yet.
struct ResolvedFile {
    filename: String,
    bytes: bytes::Bytes,
    content_type: Option<String>,
    source_file_id: Option<String>,
}

/// Snapshot of one `input_file` part, broken out of the AST so the
/// resolution code doesn't need to know about [`ResponseInputContentItem`]
/// internals.
#[derive(Debug, Clone)]
struct InputFilePart {
    file_id: Option<String>,
    file_data: Option<String>,
    file_url: Option<String>,
    filename: Option<String>,
}

fn collect_input_file_parts(payload: &CreateResponsesPayload) -> Vec<InputFilePart> {
    let mut out = Vec::new();
    // Files pre-uploaded via `shell.environment.container_auto.file_ids`
    // (spec: ContainerAutoParam.file_ids) are staged the same way as
    // `input_file` parts — they end up in /mnt/data before the model's
    // first shell command. Treated as `file_id`-only parts with no
    // inline data or remote URL.
    if let Some(tools) = payload.tools.as_ref() {
        for tool in tools {
            if let Some(shell) = tool.as_shell()
                && let Some(env) = shell.environment.as_ref()
                && let crate::api_types::responses::ShellEnvironment::ContainerAuto(auto) = env
                && let Some(ids) = auto.file_ids.as_ref()
            {
                for id in ids {
                    out.push(InputFilePart {
                        file_id: Some(id.clone()),
                        file_data: None,
                        file_url: None,
                        filename: None,
                    });
                }
            }
        }
    }
    let Some(input) = payload.input.as_ref() else {
        return out;
    };
    match input {
        ResponsesInput::Text(_) => {}
        ResponsesInput::Items(items) => {
            for item in items {
                collect_from_item(item, &mut out);
            }
        }
    }
    out
}

fn collect_from_item(item: &ResponsesInputItem, out: &mut Vec<InputFilePart>) {
    match item {
        ResponsesInputItem::EasyMessage(msg) => match &msg.content {
            EasyInputMessageContent::Text(_) => {}
            EasyInputMessageContent::Parts(parts) => {
                for part in parts {
                    push_if_input_file(part, out);
                }
            }
        },
        ResponsesInputItem::MessageItem(msg) => {
            for part in &msg.content {
                push_if_input_file(part, out);
            }
        }
        // Other variants (FunctionCall, FunctionCallOutput, OutputMessage,
        // tool-specific outputs, Reasoning, ImageGeneration) carry no
        // user-supplied file parts.
        _ => {}
    }
}

fn push_if_input_file(part: &ResponseInputContentItem, out: &mut Vec<InputFilePart>) {
    if let ResponseInputContentItem::InputFile {
        file_id,
        file_data,
        filename,
        file_url,
        ..
    } = part
    {
        out.push(InputFilePart {
            file_id: file_id.clone(),
            file_data: file_data.clone(),
            file_url: file_url.clone(),
            filename: filename.clone(),
        });
    }
}

async fn resolve_one(state: &AppState, part: &InputFilePart) -> Result<ResolvedFile, StageError> {
    validate_source_count(part)?;

    if let Some(ref id) = part.file_id {
        return resolve_file_id(state, id, part.filename.as_deref()).await;
    }
    if let Some(ref data) = part.file_data {
        return resolve_file_data(data, part.filename.as_deref());
    }
    if let Some(ref url) = part.file_url {
        return resolve_file_url(state, url, part.filename.as_deref()).await;
    }
    unreachable!("validate_source_count guarantees exactly one source is set")
}

/// Ensure exactly one of `file_id` / `file_data` / `file_url` is set.
fn validate_source_count(part: &InputFilePart) -> Result<(), StageError> {
    let count = [
        part.file_id.is_some(),
        part.file_data.is_some(),
        part.file_url.is_some(),
    ]
    .into_iter()
    .filter(|b| *b)
    .count();
    match count {
        0 => Err(StageError::NoSource),
        1 => Ok(()),
        _ => Err(StageError::AmbiguousSource),
    }
}

/// Apply collision dedupe: if `filename` is already in `seen`, prefix
/// it with the short content hash so the staged set keeps both files.
/// If that prefixed form is also taken (e.g. three uploads with the
/// same content + filename), append an incrementing `-N-` counter
/// until a free name is found.
///
/// Mutates `seen` to remember the returned name.
fn dedupe_filename(filename: String, bytes: &[u8], seen: &mut HashSet<String>) -> String {
    if seen.insert(filename.clone()) {
        return filename;
    }
    let prefix = short_content_hash(bytes);
    let prefixed = format!("{prefix}-{filename}");
    if seen.insert(prefixed.clone()) {
        return prefixed;
    }
    // Same content + same filename arriving a third time. Walk a
    // counter to keep collisions unique — bounded by attempt count
    // so we never spin forever on a pathologically full `seen`.
    for n in 2u32..u32::MAX {
        let candidate = format!("{prefix}-{n}-{filename}");
        if seen.insert(candidate.clone()) {
            return candidate;
        }
    }
    // Fall-through is unreachable in practice (we'd need to have
    // ingested ~4 billion files with identical content in one
    // request, which `max_input_files_per_request` rules out). Emit
    // a UUID-suffixed fallback rather than risk a duplicate-path
    // overwrite at /mnt/data write time.
    let fallback = format!("{prefix}-{}-{filename}", uuid::Uuid::new_v4().simple());
    seen.insert(fallback.clone());
    fallback
}

async fn resolve_file_id(
    state: &AppState,
    raw_id: &str,
    override_filename: Option<&str>,
) -> Result<ResolvedFile, StageError> {
    use std::str::FromStr;

    let file_id = crate::models::FileId::from_str(raw_id)
        .map_err(|_| StageError::InvalidFileId(raw_id.to_string()))?;
    let uuid = file_id.into_inner();

    let services = state
        .services
        .as_ref()
        .ok_or_else(|| StageError::Storage("files service unavailable".into()))?;

    let metadata = services
        .files
        .get(uuid)
        .await
        .map_err(|e| StageError::Storage(e.to_string()))?
        .ok_or_else(|| StageError::FileNotFound(raw_id.to_string()))?;

    let content = services
        .files
        .get_content(uuid)
        .await
        .map_err(|e| StageError::Storage(e.to_string()))?;

    let filename = sanitize_filename(
        override_filename.unwrap_or(&metadata.filename),
        Some(&metadata.filename),
    );

    Ok(ResolvedFile {
        filename,
        bytes: bytes::Bytes::from(content),
        content_type: metadata.content_type,
        source_file_id: Some(raw_id.to_string()),
    })
}

fn resolve_file_data(
    raw: &str,
    override_filename: Option<&str>,
) -> Result<ResolvedFile, StageError> {
    let parsed = crate::providers::image::parse_data_url(raw)
        .map_err(|e| StageError::BadDataUrl(e.to_string()))?;
    let bytes = BASE64
        .decode(parsed.data.as_bytes())
        .map_err(|e| StageError::BadBase64(e.to_string()))?;

    // Best-effort filename: caller may have supplied one, else
    // synthesize from the media type so the model sees something
    // sensible at `/mnt/data/<name>`.
    let synthesized = format!("upload{}", media_type_to_ext(&parsed.media_type));
    let filename = sanitize_filename(
        override_filename.unwrap_or(&synthesized),
        Some(&synthesized),
    );

    Ok(ResolvedFile {
        filename,
        bytes: bytes::Bytes::from(bytes),
        content_type: Some(parsed.media_type),
        source_file_id: None,
    })
}

async fn resolve_file_url(
    state: &AppState,
    url: &str,
    override_filename: Option<&str>,
) -> Result<ResolvedFile, StageError> {
    // SSRF guard — same options the image fetcher uses. Loopback is
    // forbidden because content URLs in user requests are untrusted.
    let validated = crate::validation::validate_base_url_opts(
        url,
        crate::validation::UrlValidationOptions::default(),
    )
    .map_err(|e| StageError::BlockedUrl(e.to_string()))?;
    let pinned = crate::validation::pinned_reqwest_client(&validated)
        .map_err(|e| StageError::FetchFailed(format!("pin dns: {e}")))?;
    let _ = &state.http_client; // pool unused — pin is per-host
    let resp = pinned
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| StageError::FetchFailed(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(StageError::HttpStatus(status.as_u16()));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string());

    let url_filename = url_basename(url);
    let filename = sanitize_filename(
        override_filename.unwrap_or(&url_filename),
        Some(&url_filename),
    );

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| StageError::FetchFailed(e.to_string()))?;

    Ok(ResolvedFile {
        filename,
        bytes,
        content_type,
        source_file_id: None,
    })
}

/// First 8 hex chars of sha256(content) — used as the dedupe prefix.
fn short_content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    hex::encode(&out[..4])
}

fn url_basename(url: &str) -> String {
    let path_only = url.split(['?', '#']).next().unwrap_or(url);
    // Strip the scheme so we don't mistake the host for a filename
    // when the path is empty (e.g. `https://example.com/`).
    let after_scheme = match path_only.find("://") {
        Some(idx) => &path_only[idx + 3..],
        None => path_only,
    };
    // Drop the host: everything up to and including the first '/'.
    let path = match after_scheme.find('/') {
        Some(idx) => &after_scheme[idx + 1..],
        None => "",
    };
    let last = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    if last.is_empty() {
        "upload".to_string()
    } else {
        last.to_string()
    }
}

/// Strip path components, traversal segments, leading dots, and any
/// characters that would let one file clobber another. Always returns
/// a non-empty string — falls back to `fallback` (or "upload") if the
/// primary input sanitizes to nothing.
fn sanitize_filename(raw: &str, fallback: Option<&str>) -> String {
    let candidate = clean_filename(raw);
    if !candidate.is_empty() {
        return truncate(candidate);
    }
    if let Some(fb) = fallback {
        let cleaned = clean_filename(fb);
        if !cleaned.is_empty() {
            return truncate(cleaned);
        }
    }
    "upload".to_string()
}

fn clean_filename(raw: &str) -> String {
    let base = std::path::Path::new(raw)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    // Drop any leading dots so we don't accidentally create dotfiles
    // (which could shadow real config in the image like `.bashrc`).
    let trimmed = base.trim_start_matches('.');
    // Replace any character that isn't a safe filename char with `_`.
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '+' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn truncate(s: String) -> String {
    const MAX: usize = 200;
    if s.len() <= MAX {
        return s;
    }
    // Preserve the extension if possible.
    let path = std::path::Path::new(&s);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let stem = path.file_stem().and_then(|e| e.to_str()).unwrap_or(&s);
    let stem_keep = MAX.saturating_sub(ext.len());
    let mut out: String = stem.chars().take(stem_keep).collect();
    out.push_str(&ext);
    out
}

/// Pick a sensible extension for a media type when the caller didn't
/// supply a filename. Keeps the synthesized name small and grep-able.
fn media_type_to_ext(media_type: &str) -> &'static str {
    match media_type.to_ascii_lowercase().as_str() {
        "text/plain" => ".txt",
        "text/csv" => ".csv",
        "text/tab-separated-values" => ".tsv",
        "application/json" => ".json",
        "application/x-ndjson" => ".jsonl",
        "text/html" => ".html",
        "application/xml" | "text/xml" => ".xml",
        "application/yaml" | "text/yaml" => ".yaml",
        "application/pdf" => ".pdf",
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/svg+xml" => ".svg",
        "application/zip" => ".zip",
        "application/gzip" => ".gz",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_strips_path_components() {
        assert_eq!(sanitize_filename("../../etc/passwd", None), "passwd");
        assert_eq!(sanitize_filename("/abs/path/foo.csv", None), "foo.csv");
        assert_eq!(sanitize_filename("foo.csv", None), "foo.csv");
        assert_eq!(
            sanitize_filename("..", Some("fallback.txt")),
            "fallback.txt"
        );
        assert_eq!(sanitize_filename("", None), "upload");
        assert_eq!(sanitize_filename(".bashrc", None), "bashrc");
    }

    #[test]
    fn sanitize_filename_neutralizes_dangerous_chars() {
        assert_eq!(sanitize_filename("foo bar.csv", None), "foo bar.csv");
        // Path-style traversal collapses to the OS-level basename
        // ".csv", which our leading-dot strip turns into "csv".
        assert_eq!(sanitize_filename("foo;rm -rf /.csv", None), "csv");
        // No path separators here, so the dangerous chars get mapped
        // to '_' by the char filter.
        assert_eq!(sanitize_filename("foo;rm.csv", None), "foo_rm.csv");
        // The `ï` is one codepoint, so it gets replaced with one `_`.
        assert_eq!(sanitize_filename("naïve.txt", None), "na_ve.txt");
    }

    #[test]
    fn sanitize_filename_truncates_long_names_preserving_extension() {
        let long = format!("{}.csv", "a".repeat(300));
        let result = sanitize_filename(&long, None);
        assert!(result.len() <= 200);
        assert!(result.ends_with(".csv"));
    }

    #[test]
    fn url_basename_strips_query_and_fragment() {
        assert_eq!(url_basename("https://example.com/foo/bar.csv"), "bar.csv");
        assert_eq!(
            url_basename("https://example.com/foo/bar.csv?x=1"),
            "bar.csv"
        );
        assert_eq!(
            url_basename("https://example.com/foo/bar.csv#sec"),
            "bar.csv"
        );
        assert_eq!(url_basename("https://example.com/"), "upload");
    }

    #[test]
    fn media_type_to_ext_covers_common_types() {
        assert_eq!(media_type_to_ext("text/csv"), ".csv");
        assert_eq!(media_type_to_ext("Application/JSON"), ".json");
        assert_eq!(media_type_to_ext("image/png"), ".png");
        assert_eq!(media_type_to_ext("application/x-unknown"), "");
    }

    #[test]
    fn resolve_file_data_handles_inline_base64() {
        // "hello" = aGVsbG8=
        let raw = "data:text/plain;base64,aGVsbG8=";
        let r = resolve_file_data(raw, Some("greeting.txt")).unwrap();
        assert_eq!(r.filename, "greeting.txt");
        assert_eq!(r.content_type.as_deref(), Some("text/plain"));
        assert_eq!(r.bytes.as_ref(), b"hello");
    }

    #[test]
    fn resolve_file_data_rejects_malformed_data_url() {
        assert!(resolve_file_data("not-a-data-url", None).is_err());
        assert!(resolve_file_data("data:text/plain;base64,!!!", None).is_err());
    }

    #[test]
    fn collect_input_file_parts_walks_message_items() {
        let payload_json = serde_json::json!({
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "see attached"},
                        {"type": "input_file", "file_id": "file-1111111111111111", "filename": "a.csv"}
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {"type": "input_file", "file_data": "data:text/plain;base64,aGk="}
                    ]
                }
            ]
        });
        let payload: CreateResponsesPayload = serde_json::from_value(payload_json).unwrap();
        let parts = collect_input_file_parts(&payload);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].file_id.as_deref() == Some("file-1111111111111111"));
        assert!(parts[1].file_data.is_some());
    }

    #[test]
    fn collect_input_file_parts_returns_empty_for_text_input() {
        let payload: CreateResponsesPayload = serde_json::from_value(serde_json::json!({
            "input": "hello"
        }))
        .unwrap();
        assert!(collect_input_file_parts(&payload).is_empty());
    }

    #[test]
    fn short_content_hash_is_stable_and_8_chars() {
        let h = short_content_hash(b"hello");
        assert_eq!(h.len(), 8);
        // sha256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(h, "2cf24dba");
    }

    #[test]
    fn validate_source_count_rejects_zero_and_multiple() {
        let none = InputFilePart {
            file_id: None,
            file_data: None,
            file_url: None,
            filename: None,
        };
        assert!(matches!(
            validate_source_count(&none),
            Err(StageError::NoSource)
        ));

        let ambiguous = InputFilePart {
            file_id: Some("file-x".into()),
            file_data: Some("data:text/plain;base64,Zm9v".into()),
            file_url: None,
            filename: None,
        };
        assert!(matches!(
            validate_source_count(&ambiguous),
            Err(StageError::AmbiguousSource)
        ));

        let single = InputFilePart {
            file_id: None,
            file_data: Some("data:text/plain;base64,Zm9v".into()),
            file_url: None,
            filename: None,
        };
        assert!(validate_source_count(&single).is_ok());
    }

    #[test]
    fn dedupe_filename_prefixes_collisions_with_hash() {
        let mut seen = HashSet::new();
        let first = dedupe_filename("data.csv".into(), b"first", &mut seen);
        assert_eq!(first, "data.csv");
        let second = dedupe_filename("data.csv".into(), b"second", &mut seen);
        assert_ne!(second, "data.csv");
        assert!(second.ends_with("-data.csv"));
        // The prefix is the short hash of the *second* file's content.
        assert!(second.starts_with(&short_content_hash(b"second")));
        // And both names are tracked so a third collision picks up the
        // already-prefixed name in the set rather than re-colliding.
        assert!(seen.contains(&first));
        assert!(seen.contains(&second));
    }

    #[test]
    fn dedupe_filename_no_op_when_unique() {
        let mut seen = HashSet::new();
        let r = dedupe_filename("a.txt".into(), b"x", &mut seen);
        assert_eq!(r, "a.txt");
        let r2 = dedupe_filename("b.txt".into(), b"y", &mut seen);
        assert_eq!(r2, "b.txt");
    }

    #[test]
    fn dedupe_filename_handles_repeat_same_content_collisions() {
        // Three uploads with identical content + filename: the first
        // gets the bare name, the second a content-hash-prefixed name,
        // the third a counter-suffixed prefix so all three stage
        // distinct paths under `/mnt/data`.
        let mut seen = HashSet::new();
        let bytes = b"identical";
        let a = dedupe_filename("doc.csv".into(), bytes, &mut seen);
        let b = dedupe_filename("doc.csv".into(), bytes, &mut seen);
        let c = dedupe_filename("doc.csv".into(), bytes, &mut seen);
        assert_eq!(a, "doc.csv");
        assert_ne!(b, a);
        assert_ne!(c, a);
        assert_ne!(c, b, "third occurrence must not collide with the second");
        // Counter variant is recognizable.
        let prefix = short_content_hash(bytes);
        assert!(c.starts_with(&format!("{prefix}-2-")), "got: {c}");
    }
}
