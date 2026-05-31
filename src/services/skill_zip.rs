//! Zip bundle pack/unpack and SKILL.md frontmatter parsing for the OpenAI
//! Skills upload/download surface. Server-only (pulls in the `zip` crate).
//!
//! Security: every entry path is validated with [`validate_skill_path`]
//! (rejecting absolute paths, `..`, backslashes, empty segments), the total
//! extracted size is capped by counting actually-read bytes (not the zip's
//! declared sizes) to defend against zip bombs, and non-UTF-8 entries are
//! rejected (v1 skill files are text-only).

use std::{
    collections::HashSet,
    io::{Cursor, Read, Write},
};

use crate::models::{SkillFile, SkillFileInput, validate_skill_path};

#[derive(Debug, thiserror::Error)]
pub enum SkillZipError {
    #[error("invalid zip archive: {0}")]
    InvalidArchive(String),
    #[error("skill bundle exceeds the maximum of {0} files")]
    TooManyFiles(usize),
    #[error("skill bundle exceeds the maximum size of {0} bytes")]
    TooLarge(u64),
    #[error("invalid file path in bundle: {0}")]
    InvalidPath(String),
    #[error("file '{0}' is not valid UTF-8 text (binary files are unsupported)")]
    NotUtf8(String),
    #[error("skill bundle contains no files")]
    Empty,
    #[error("failed to build zip bundle: {0}")]
    Pack(String),
}

/// Extract a zip bundle into skill file inputs. `max_total_bytes == 0` means
/// unlimited; `max_files` bounds the entry count.
pub fn unpack_zip_to_files(
    bytes: &[u8],
    max_total_bytes: u64,
    max_files: usize,
) -> Result<Vec<SkillFileInput>, SkillZipError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| SkillZipError::InvalidArchive(e.to_string()))?;

    let mut raw: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| SkillZipError::InvalidArchive(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        // Ignore archive cruft.
        if name.starts_with("__MACOSX/") || name.rsplit('/').next() == Some(".DS_Store") {
            continue;
        }
        if raw.len() >= max_files {
            return Err(SkillZipError::TooManyFiles(max_files));
        }
        // Read one byte past the remaining budget so an over-limit entry is
        // detectable without trusting the declared uncompressed size.
        let cap = if max_total_bytes == 0 {
            u64::MAX
        } else {
            max_total_bytes.saturating_sub(total).saturating_add(1)
        };
        let mut buf = Vec::new();
        entry
            .take(cap)
            .read_to_end(&mut buf)
            .map_err(|e| SkillZipError::InvalidArchive(e.to_string()))?;
        total = total.saturating_add(buf.len() as u64);
        if max_total_bytes > 0 && total > max_total_bytes {
            return Err(SkillZipError::TooLarge(max_total_bytes));
        }
        raw.push((name, buf));
    }
    if raw.is_empty() {
        return Err(SkillZipError::Empty);
    }

    let stripped = strip_common_dir(raw);

    let mut files = Vec::with_capacity(stripped.len());
    for (name, bytes) in stripped {
        validate_skill_path(&name).map_err(|_| SkillZipError::InvalidPath(name.clone()))?;
        let content = String::from_utf8(bytes).map_err(|_| SkillZipError::NotUtf8(name.clone()))?;
        files.push(SkillFileInput {
            path: name,
            content,
            content_type: None,
        });
    }
    Ok(files)
}

/// If every entry shares one top-level directory (a directory upload zipped
/// with its folder), strip that prefix so `SKILL.md` sits at the root.
fn strip_common_dir(raw: Vec<(String, Vec<u8>)>) -> Vec<(String, Vec<u8>)> {
    if !raw.iter().all(|(n, _)| n.contains('/')) {
        return raw;
    }
    let segs: HashSet<&str> = raw
        .iter()
        .filter_map(|(n, _)| n.split('/').next())
        .collect();
    if segs.len() != 1 {
        return raw;
    }
    let prefix_len = segs.iter().next().map(|s| s.len() + 1).unwrap_or(0);
    raw.into_iter()
        .map(|(n, b)| (n[prefix_len..].to_string(), b))
        .collect()
}

/// Pack skill files into a deflate zip bundle for download.
pub fn pack_files_to_zip(files: &[SkillFile]) -> Result<Vec<u8>, SkillZipError> {
    let mut buf = Vec::new();
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    {
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
        for f in files {
            zip.start_file(&f.path, options)
                .map_err(|e| SkillZipError::Pack(e.to_string()))?;
            zip.write_all(f.content.as_bytes())
                .map_err(|e| SkillZipError::Pack(e.to_string()))?;
        }
        zip.finish()
            .map_err(|e| SkillZipError::Pack(e.to_string()))?;
    }
    Ok(buf)
}

/// Parsed SKILL.md YAML frontmatter (best-effort, line-based).
#[derive(Debug, Default, Clone)]
pub struct SkillFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub user_invocable: Option<bool>,
    pub disable_model_invocation: Option<bool>,
    pub allowed_tools: Option<Vec<String>>,
    pub argument_hint: Option<String>,
}

/// Parse the leading `---` fenced YAML frontmatter of a SKILL.md document.
/// Mirrors the frontend importer's lightweight parser (no full YAML engine).
pub fn parse_skill_frontmatter(content: &str) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter::default();
    let Some(rest) = content.strip_prefix("---") else {
        return fm;
    };
    let rest = rest.trim_start_matches(['\r', '\n']);
    let Some(end) = rest.find("\n---") else {
        return fm;
    };
    for line in rest[..end].lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = strip_quotes(value.trim());
        match key.trim() {
            "name" => fm.name = Some(value.to_string()),
            "description" => fm.description = Some(value.to_string()),
            "argument-hint" | "argument_hint" => fm.argument_hint = Some(value.to_string()),
            "user-invocable" | "user_invocable" => fm.user_invocable = parse_bool(value),
            "disable-model-invocation" | "disable_model_invocation" => {
                fm.disable_model_invocation = parse_bool(value)
            }
            "allowed-tools" | "allowed_tools" => fm.allowed_tools = parse_inline_list(value),
            _ => {}
        }
    }
    fm
}

fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "yes" => Some(true),
        "false" | "no" => Some(false),
        _ => None,
    }
}

/// Parse a YAML inline list `[a, b]` or a comma-separated value.
fn parse_inline_list(s: &str) -> Option<Vec<String>> {
    let inner = s.trim();
    let inner = inner
        .strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(inner);
    let items: Vec<String> = inner
        .split(',')
        .map(|x| strip_quotes(x.trim()).to_string())
        .filter(|x| !x.is_empty())
        .collect();
    if items.is_empty() { None } else { Some(items) }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn file(path: &str, content: &str) -> SkillFile {
        SkillFile {
            path: path.into(),
            content: content.into(),
            byte_size: content.len() as i64,
            content_type: "text/markdown".into(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn zip_round_trip() {
        let files = vec![
            file("SKILL.md", "# Hello"),
            file("scripts/run.py", "print('hi')"),
        ];
        let bytes = pack_files_to_zip(&files).unwrap();
        let unpacked = unpack_zip_to_files(&bytes, 0, 100).unwrap();
        assert_eq!(unpacked.len(), 2);
        let main = unpacked.iter().find(|f| f.path == "SKILL.md").unwrap();
        assert_eq!(main.content, "# Hello");
    }

    #[test]
    fn unpack_strips_common_dir() {
        let files = vec![file("myskill/SKILL.md", "x"), file("myskill/a.txt", "y")];
        let bytes = pack_files_to_zip(&files).unwrap();
        let unpacked = unpack_zip_to_files(&bytes, 0, 100).unwrap();
        let paths: HashSet<&str> = unpacked.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains("SKILL.md"));
        assert!(paths.contains("a.txt"));
    }

    #[test]
    fn unpack_enforces_size_cap() {
        let files = vec![file("SKILL.md", &"a".repeat(1000))];
        let bytes = pack_files_to_zip(&files).unwrap();
        let err = unpack_zip_to_files(&bytes, 100, 100).unwrap_err();
        assert!(matches!(err, SkillZipError::TooLarge(_)));
    }

    #[test]
    fn unpack_rejects_traversal_path() {
        // After stripping the common `evil/` dir this becomes `../x.md`.
        let files = vec![file("evil/../x.md", "x")];
        let bytes = pack_files_to_zip(&files).unwrap();
        let err = unpack_zip_to_files(&bytes, 0, 100).unwrap_err();
        assert!(matches!(err, SkillZipError::InvalidPath(_)));
    }

    #[test]
    fn frontmatter_extracts_name_and_description() {
        let md = "---\nname: pdf-tools\ndescription: \"Work with PDFs\"\nuser-invocable: true\n---\n\n# Body";
        let fm = parse_skill_frontmatter(md);
        assert_eq!(fm.name.as_deref(), Some("pdf-tools"));
        assert_eq!(fm.description.as_deref(), Some("Work with PDFs"));
        assert_eq!(fm.user_invocable, Some(true));
    }
}
