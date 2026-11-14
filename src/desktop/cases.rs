//! Versioned case repository for desktop evidence capture.
//!
//! Each case is one JSON document under `<root>/cases/<case id>.json`. Evidence
//! is append-only: captured operation parameters, responses and source metadata
//! are hashed with keccak256 and are never rewritten; notes and bookmarks live
//! beside the evidence in separate lists. Imports are bounded, schema-checked
//! (unknown fields are rejected) and refuse credential-looking material;
//! captures and exports redact secret-keyed values and sanitize credential
//! URLs (userinfo and known credential query parameters) instead of storing
//! them. First-time persistence never overwrites an existing case document.
//!
//! Credential screening is a bounded, best-effort filter — not a guarantee
//! that no secret remains. Free-form strings can carry secrets the scanner
//! cannot recognize, so callers must never capture raw keys or credentialed
//! RPC URLs in the first place. Walks that hit their size or depth bounds fail
//! closed rather than continue with an unexamined remainder. Folder and file
//! checks re-run on every operation; a concurrent swap between a check and its
//! use is a bounded race that the following operation detects. This module
//! performs local file I/O only — no network, no process execution.

use ethers::utils::{hex, keccak256};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
const REDACTED: &str = "[redacted]";
const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;
const MAX_CASE_FILE_BYTES: u64 = MAX_IMPORT_BYTES as u64;
const MAX_CASES_LISTED: usize = 500;
const MAX_EVIDENCE_PER_CASE: usize = 1000;
const MAX_NOTES_PER_CASE: usize = 2000;
const MAX_NAME_BYTES: usize = 200;
const MAX_NOTE_BYTES: usize = 4000;
const MAX_OPERATION_BYTES: usize = 256;
const MAX_ID_CHARS: usize = 64;
const MAX_SECRET_HITS: usize = 256;
const MAX_SCAN_DEPTH: usize = 32;
const MAX_DIFFERENCES: usize = 128;
const MAX_DIFF_DEPTH: usize = 24;
const CASE_ID_BYTES: usize = 16;
const EVIDENCE_ID_BYTES: usize = 12;
const NOTE_ID_BYTES: usize = 8;
const SECRET_KEYS: [&str; 16] = [
    "privatekey",
    "private_key",
    "password",
    "passphrase",
    "authorization",
    "token",
    "secret",
    "apikey",
    "api_key",
    "access_token",
    "accesskey",
    "access_key",
    "mnemonic",
    "seedphrase",
    "seed_phrase",
    "credentials",
];
const URL_SECRET_PARAMS: [&str; 8] = [
    "key",
    "token",
    "password",
    "secret",
    "apikey",
    "api_key",
    "access_token",
    "authorization",
];
const INSPECT_MAX_DEPTH: usize = 4;
const INSPECT_MAX_ENTRIES: usize = 4000;
const INSPECT_MAX_LISTED: usize = 500;
const INSPECT_MAX_READ: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseFile {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub created: String,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub notes: Vec<CaseNote>,
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Evidence {
    pub id: String,
    pub operation: String,
    pub parameters: Value,
    pub response: Value,
    pub source: Value,
    #[serde(default)]
    pub metadata: Value,
    pub captured: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaseNote {
    pub id: String,
    pub evidence_id: String,
    pub note: String,
    pub created: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Bookmark {
    pub id: String,
    pub evidence_id: String,
    pub created: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseSummary {
    pub id: String,
    pub name: String,
    pub created: String,
    pub evidence_count: usize,
    pub note_count: usize,
    pub bookmark_count: usize,
}

#[derive(Clone, Debug)]
pub struct CaseRepository {
    root: PathBuf,
}

impl CaseRepository {
    /// Root is the parent-controlled application data folder; everything lives
    /// under `<root>/cases/`.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn create(&self, name: &str) -> Result<CaseSummary, String> {
        let name = validate_case_name(name)?;
        let dir = self.cases_dir()?;
        let id = random_hex_id(CASE_ID_BYTES);
        let file = CaseFile {
            schema_version: SCHEMA_VERSION,
            id: id.clone(),
            name,
            created: utc_now(),
            evidence: Vec::new(),
            notes: Vec::new(),
            bookmarks: Vec::new(),
        };
        let path = dir.join(format!("{id}.json"));
        if path.exists() {
            return Err("Case id collision; try again.".into());
        }
        let bytes = serialize_case_document(&file)?;
        atomic_write(&path, &bytes, false)?;
        Ok(case_summary(&file))
    }

    /// Lists case summaries, newest first. Unreadable or invalid documents are
    /// skipped; the result is capped at `MAX_CASES_LISTED`.
    pub fn list(&self) -> Result<Vec<CaseSummary>, String> {
        let dir = self.cases_dir()?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("Could not read the case folder: {error}")),
        };
        let mut summaries = Vec::new();
        for entry in entries {
            if summaries.len() >= MAX_CASES_LISTED {
                break;
            }
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let is_json = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("json"))
                .unwrap_or(false);
            if !is_json {
                continue;
            }
            let Ok(bytes) = read_case_bytes(&path) else {
                continue;
            };
            let Ok(file) = serde_json::from_slice::<CaseFile>(&bytes) else {
                continue;
            };
            if validate_case_file(&file).is_err() {
                continue;
            }
            summaries.push(case_summary(&file));
        }
        summaries.sort_by(|left, right| {
            right
                .created
                .cmp(&left.created)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(summaries)
    }

    pub fn load(&self, case_id: &str) -> Result<CaseFile, String> {
        let path = self.case_path(case_id)?;
        let bytes = read_case_bytes(&path)?;
        let file: CaseFile = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Case {case_id} is not a valid case document: {error}"))?;
        validate_case_file(&file)?;
        if file.id != case_id {
            return Err(format!(
                "Case file {} holds case {}.",
                path.display(),
                file.id
            ));
        }
        Ok(file)
    }

    /// Appends one immutable evidence record. Secret-keyed values are redacted
    /// and credential URLs are sanitized before storage, with the touched paths
    /// recorded in metadata. Screening is best-effort, not a guarantee: a walk
    /// that cannot complete fails closed and nothing is stored.
    pub fn append_evidence(
        &self,
        case_id: &str,
        operation: &str,
        parameters: &Value,
        response: &Value,
        source: &Value,
    ) -> Result<Evidence, String> {
        let mut file = self.load(case_id)?;
        if file.evidence.len() >= MAX_EVIDENCE_PER_CASE {
            return Err("This case has reached the evidence limit.".into());
        }
        let operation = validate_operation(operation)?;
        let mut parameters = parameters.clone();
        let mut response = response.clone();
        let mut source = source.clone();
        let mut redaction = RedactWalk::default();
        redact_secrets(&mut parameters, "parameters", &mut redaction, 0);
        redact_secrets(&mut response, "response", &mut redaction, 0);
        redact_secrets(&mut source, "source", &mut redaction, 0);
        if redaction.overflow {
            return Err(
                "Evidence could not be fully screened for credentials; nothing was stored.".into(),
            );
        }
        let metadata = if redaction.redacted.is_empty() {
            json!({})
        } else {
            json!({ "redactedKeys": redaction.redacted })
        };
        let mut evidence = Evidence {
            id: random_hex_id(EVIDENCE_ID_BYTES),
            operation,
            parameters,
            response,
            source,
            metadata,
            captured: utc_now(),
            digest: String::new(),
        };
        evidence.digest = evidence_digest(&evidence);
        file.evidence.push(evidence.clone());
        self.write_case_document(case_id, &file)?;
        Ok(evidence)
    }

    /// Appends a note and/or a bookmark for one evidence record. Evidence
    /// itself is never touched; bookmarks are one per evidence record.
    pub fn annotate(
        &self,
        case_id: &str,
        evidence_id: &str,
        note: &str,
        bookmark: bool,
    ) -> Result<(), String> {
        validate_id(evidence_id)?;
        let mut file = self.load(case_id)?;
        if find_evidence(&file, evidence_id).is_err() {
            return Err(format!("No evidence {evidence_id} in this case."));
        }
        let note = note.trim();
        if note.is_empty() && !bookmark {
            return Err("Nothing to record. Add a note or bookmark the evidence.".into());
        }
        if note.len() > MAX_NOTE_BYTES {
            return Err("Notes are limited to 4000 characters.".into());
        }
        if !note.is_empty() {
            if file.notes.len() >= MAX_NOTES_PER_CASE {
                return Err("This case has reached the note limit.".into());
            }
            file.notes.push(CaseNote {
                id: random_hex_id(NOTE_ID_BYTES),
                evidence_id: evidence_id.to_string(),
                note: note.to_string(),
                created: utc_now(),
            });
        }
        if bookmark
            && !file
                .bookmarks
                .iter()
                .any(|entry| entry.evidence_id == evidence_id)
        {
            file.bookmarks.push(Bookmark {
                id: random_hex_id(NOTE_ID_BYTES),
                evidence_id: evidence_id.to_string(),
                created: utc_now(),
            });
        }
        self.write_case_document(case_id, &file)?;
        Ok(())
    }

    /// Field-path diff of two evidence records (operation, parameters, response),
    /// bounded in depth and count.
    pub fn compare(&self, case_id: &str, left_id: &str, right_id: &str) -> Result<Value, String> {
        validate_id(left_id)?;
        validate_id(right_id)?;
        let file = self.load(case_id)?;
        let left = find_evidence(&file, left_id)?;
        let right = find_evidence(&file, right_id)?;
        let mut differences = Vec::new();
        diff_values(
            "operation",
            &Value::String(left.operation.clone()),
            &Value::String(right.operation.clone()),
            &mut differences,
            0,
        );
        diff_values(
            "parameters",
            &left.parameters,
            &right.parameters,
            &mut differences,
            0,
        );
        diff_values(
            "response",
            &left.response,
            &right.response,
            &mut differences,
            0,
        );
        let truncated = differences.len() >= MAX_DIFFERENCES
            || differences.iter().any(|difference| {
                difference.get("kind").and_then(Value::as_str) == Some("depth_limit")
            });
        Ok(json!({
            "left": {"id": left.id, "captured": left.captured, "digest": left.digest},
            "right": {"id": right.id, "captured": right.captured, "digest": right.digest},
            "identical": differences.is_empty() && !truncated,
            "truncated": truncated,
            "differences": differences,
        }))
    }

    /// Imports a serialized case export (at most 8 MiB). The document must be
    /// schema version 1 with no unknown fields, every evidence digest must
    /// verify, and every note or bookmark must reference evidence inside the
    /// same document. Documents with credential-looking fields, or documents
    /// whose credential screening cannot complete, are rejected outright; the
    /// case id must not collide with an existing case.
    pub fn import_json(&self, bytes: &[u8]) -> Result<CaseSummary, String> {
        if bytes.len() > MAX_IMPORT_BYTES {
            return Err(format!(
                "Case exports are limited to {} MiB.",
                MAX_IMPORT_BYTES / (1024 * 1024)
            ));
        }
        let file: CaseFile = serde_json::from_slice(bytes)
            .map_err(|error| format!("This is not a valid case export: {error}"))?;
        validate_case_file(&file)?;
        let value = serde_json::to_value(&file).map_err(|error| error.to_string())?;
        let mut screening = SecretWalk::default();
        scan_secrets(&value, "case", &mut screening, 0);
        if screening.overflow {
            return Err(
                "Import rejected: the document could not be fully screened for credentials.".into(),
            );
        }
        if !screening.hits.is_empty() {
            return Err(format!(
                "Import rejected: {} field(s) look like credentials, starting at {}.",
                screening.hits.len(),
                screening.hits[0]
            ));
        }
        let dir = self.cases_dir()?;
        let path = dir.join(format!("{}.json", file.id));
        if path.exists() {
            return Err("A case with this id already exists in this repository.".into());
        }
        let bytes = serialize_case_document(&file)?;
        atomic_write(&path, &bytes, false)?;
        Ok(case_summary(&file))
    }

    /// Serializes the case for sharing. Secret-keyed values are replaced with
    /// `[redacted]` and credential URLs are sanitized in the returned copy;
    /// the stored document is untouched. A screening walk that cannot complete
    /// refuses the export instead of shipping an unexamined remainder.
    pub fn export_json(&self, case_id: &str) -> Result<Vec<u8>, String> {
        let file = self.load(case_id)?;
        let mut value = serde_json::to_value(&file).map_err(|error| error.to_string())?;
        let mut redaction = RedactWalk::default();
        redact_secrets(&mut value, "case", &mut redaction, 0);
        if redaction.overflow {
            return Err(
                "Export refused: the document could not be fully screened for credentials.".into(),
            );
        }
        serde_json::to_vec_pretty(&value).map_err(|error| error.to_string())
    }

    /// Builds and validates the `<root>/cases` folder one level at a time so
    /// neither creation nor validation follows links or reparse points.
    fn cases_dir(&self) -> Result<PathBuf, String> {
        if !self.root.is_absolute()
            || self
                .root
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(
                "The case repository root must be an absolute local filesystem path.".into(),
            );
        }
        for ancestor in self.root.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            match ancestor.symlink_metadata() {
                Ok(metadata) => ensure_real_directory(ancestor, &metadata)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(ancestor).map_err(|error| {
                        format!("Could not create the case repository folder: {error}")
                    })?;
                    let metadata = ancestor.symlink_metadata().map_err(|error| {
                        format!("Could not inspect the case repository folder: {error}")
                    })?;
                    ensure_real_directory(ancestor, &metadata)?;
                }
                Err(error) => {
                    return Err(format!(
                        "Could not inspect the case repository folder: {error}"
                    ))
                }
            }
        }
        let dir = self.root.join("cases");
        match dir.symlink_metadata() {
            Ok(metadata) => ensure_real_directory(&dir, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&dir)
                    .map_err(|error| format!("Could not create the case folder: {error}"))?;
                let metadata = dir
                    .symlink_metadata()
                    .map_err(|error| format!("Could not inspect the case folder: {error}"))?;
                ensure_real_directory(&dir, &metadata)?;
            }
            Err(error) => return Err(format!("Could not inspect the case folder: {error}")),
        }
        Ok(dir)
    }

    fn case_path(&self, case_id: &str) -> Result<PathBuf, String> {
        validate_id(case_id)?;
        let dir = self.cases_dir()?;
        Ok(dir.join(format!("{case_id}.json")))
    }

    fn write_case_document(&self, case_id: &str, file: &CaseFile) -> Result<(), String> {
        validate_case_file(file)?;
        let path = self.case_path(case_id)?;
        let bytes = serialize_case_document(file)?;
        atomic_write(&path, &bytes, true)
    }
}

/// Passive project summary: lists source, ABI and build-info file names under
/// `root` with sizes, without reading source contents, executing anything or
/// writing. Dot-prefixed entries (`.env`, `.git`, …), `target` and
/// `node_modules` are skipped, links and reparse points are never followed.
pub fn inspect_project(root: &Path) -> Result<Value, String> {
    if !root.is_absolute()
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("The project root must be an absolute local filesystem path.".into());
    }
    let metadata = root
        .symlink_metadata()
        .map_err(|error| format!("Could not inspect {}: {error}", root.display()))?;
    if is_reparse_point(&metadata) || !metadata.is_dir() {
        return Err("The project root must be a real folder, not a link or file.".into());
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        if root == PathBuf::from(home) {
            return Err("Refusing to inspect the home directory directly.".into());
        }
    }
    if root
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with('.'))
        .unwrap_or(false)
    {
        return Err("Refusing to inspect a hidden configuration folder.".into());
    }
    let mut scan = ProjectScan::default();
    scan_project_dir(root, "", 0, &mut scan);
    Ok(json!({
        "root": root.display().to_string(),
        "sources": scan.sources,
        "abis": scan.abis,
        "buildInfo": scan.build_info,
        "counts": {
            "sources": scan.sources.len(),
            "abis": scan.abis.len(),
            "buildInfo": scan.build_info.len(),
        },
        "truncated": scan.truncated,
    }))
}

#[derive(Default)]
struct ProjectScan {
    sources: Vec<Value>,
    abis: Vec<Value>,
    build_info: Vec<Value>,
    visited: usize,
    truncated: bool,
}

impl ProjectScan {
    fn listed(&self) -> usize {
        self.sources.len() + self.abis.len() + self.build_info.len()
    }
}

fn scan_project_dir(dir: &Path, relative: &str, depth: usize, scan: &mut ProjectScan) {
    if depth > INSPECT_MAX_DEPTH || scan.listed() >= INSPECT_MAX_LISTED {
        scan.truncated = true;
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            scan.truncated = true;
            return;
        }
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        scan.visited += 1;
        if scan.visited > INSPECT_MAX_ENTRIES || scan.listed() >= INSPECT_MAX_LISTED {
            scan.truncated = true;
            return;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let path = dir.join(name);
        let Ok(metadata) = path.symlink_metadata() else {
            continue;
        };
        if is_reparse_point(&metadata) {
            scan.truncated = true;
            continue;
        }
        let child = if relative.is_empty() {
            name.to_string()
        } else {
            format!("{relative}/{name}")
        };
        if metadata.is_dir() {
            if matches!(name, "target" | "node_modules") {
                continue;
            }
            scan_project_dir(&path, &child, depth + 1, scan);
        } else if metadata.is_file() {
            record_project_file(&path, &child, &metadata, scan);
        }
    }
}

fn record_project_file(
    path: &Path,
    relative: &str,
    metadata: &std::fs::Metadata,
    scan: &mut ProjectScan,
) {
    let entry = || json!({ "path": relative, "bytes": metadata.len() });
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "sol" => scan.sources.push(entry()),
        "abi" => scan.abis.push(entry()),
        "json" => {
            if relative.contains("/build-info/") || relative.starts_with("build-info/") {
                scan.build_info.push(entry());
                return;
            }
            // Only small JSON documents are opened, and only to classify them.
            if metadata.len() > INSPECT_MAX_READ {
                return;
            }
            let Some(bytes) = read_small(path, INSPECT_MAX_READ) else {
                return;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                return;
            };
            if value.is_array() || value.get("abi").is_some() {
                scan.abis.push(entry());
            }
        }
        _ => {}
    }
}

fn validate_case_file(file: &CaseFile) -> Result<(), String> {
    if file.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "Unsupported case schema version {} (expected {SCHEMA_VERSION}).",
            file.schema_version
        ));
    }
    validate_id(&file.id)?;
    validate_case_name(&file.name)?;
    validate_timestamp(&file.created)?;
    if file.evidence.len() > MAX_EVIDENCE_PER_CASE {
        return Err("This case holds more evidence than the repository accepts.".into());
    }
    if file.notes.len() > MAX_NOTES_PER_CASE {
        return Err("This case holds more notes than the repository accepts.".into());
    }
    let mut evidence_ids = HashSet::new();
    for evidence in &file.evidence {
        validate_id(&evidence.id)?;
        validate_operation(&evidence.operation)?;
        validate_timestamp(&evidence.captured)?;
        if !evidence_ids.insert(evidence.id.as_str()) {
            return Err(format!("Duplicate evidence id {}.", evidence.id));
        }
        if evidence_digest(evidence) != evidence.digest {
            return Err(format!(
                "Evidence {} failed its digest check; the record was modified or truncated.",
                evidence.id
            ));
        }
    }
    let mut annotation_ids = HashSet::new();
    for note in &file.notes {
        validate_timestamp(&note.created)?;
        let text = note.note.trim();
        if text.is_empty() || text.len() > MAX_NOTE_BYTES {
            return Err(format!(
                "Note {} is empty or exceeds the {} character limit.",
                note.id, MAX_NOTE_BYTES
            ));
        }
        check_annotation(
            &note.id,
            &note.evidence_id,
            &evidence_ids,
            &mut annotation_ids,
        )?;
    }
    for bookmark in &file.bookmarks {
        validate_timestamp(&bookmark.created)?;
        check_annotation(
            &bookmark.id,
            &bookmark.evidence_id,
            &evidence_ids,
            &mut annotation_ids,
        )?;
    }
    Ok(())
}

fn check_annotation<'a>(
    id: &'a str,
    evidence_id: &'a str,
    evidence_ids: &HashSet<&'a str>,
    annotation_ids: &mut HashSet<&'a str>,
) -> Result<(), String> {
    validate_id(id)?;
    if !annotation_ids.insert(id) {
        return Err(format!("Duplicate annotation id {id}."));
    }
    if !evidence_ids.contains(evidence_id) {
        return Err(format!(
            "Annotation {id} references evidence {evidence_id} which is not in this case."
        ));
    }
    Ok(())
}

fn find_evidence<'a>(file: &'a CaseFile, evidence_id: &str) -> Result<&'a Evidence, String> {
    file.evidence
        .iter()
        .find(|evidence| evidence.id == evidence_id)
        .ok_or_else(|| format!("No evidence {evidence_id} in this case."))
}

fn case_summary(file: &CaseFile) -> CaseSummary {
    CaseSummary {
        id: file.id.clone(),
        name: file.name.clone(),
        created: file.created.clone(),
        evidence_count: file.evidence.len(),
        note_count: file.notes.len(),
        bookmark_count: file.bookmarks.len(),
    }
}

fn validate_case_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a case name.".into());
    }
    if name.chars().any(char::is_control) {
        return Err("Case names cannot contain control characters.".into());
    }
    if name.len() > MAX_NAME_BYTES {
        return Err("Case names are limited to 200 characters.".into());
    }
    Ok(name.to_string())
}

fn validate_operation(operation: &str) -> Result<String, String> {
    let operation = operation.trim();
    if operation.is_empty() {
        return Err("Name the operation that produced this evidence.".into());
    }
    if operation.len() > MAX_OPERATION_BYTES {
        return Err("Operation names are limited to 256 characters.".into());
    }
    Ok(operation.to_string())
}

fn validate_id(id: &str) -> Result<(), String> {
    // Hex-only ids cannot form path segments like `..`, `/` or `\`.
    if id.is_empty() || id.len() > MAX_ID_CHARS || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("Ids must be 1-64 hexadecimal characters.".into());
    }
    Ok(())
}

fn validate_timestamp(value: &str) -> Result<(), String> {
    // Our writer emits YYYY-MM-DDTHH:MM:SSZ; stored and imported timestamps
    // must match that shape byte for byte (every byte position is checked, so
    // the byte-index slicing below cannot split a multi-byte character).
    let bytes = value.as_bytes();
    let digits =
        |mut range: std::ops::Range<usize>| range.all(|index| bytes[index].is_ascii_digit());
    if bytes.len() != 20
        || !digits(0..4)
        || bytes[4] != b'-'
        || !digits(5..7)
        || bytes[7] != b'-'
        || !digits(8..10)
        || bytes[10] != b'T'
        || !digits(11..13)
        || bytes[13] != b':'
        || !digits(14..16)
        || bytes[16] != b':'
        || !digits(17..19)
        || bytes[19] != b'Z'
    {
        return Err("Timestamps must use the YYYY-MM-DDTHH:MM:SSZ format.".into());
    }
    let month: u32 = value[5..7]
        .parse()
        .map_err(|_| "Timestamp month is out of range.".to_string())?;
    let day: u32 = value[8..10]
        .parse()
        .map_err(|_| "Timestamp day is out of range.".to_string())?;
    let hour: u32 = value[11..13]
        .parse()
        .map_err(|_| "Timestamp hour is out of range.".to_string())?;
    let minute: u32 = value[14..16]
        .parse()
        .map_err(|_| "Timestamp minute is out of range.".to_string())?;
    let second: u32 = value[17..19]
        .parse()
        .map_err(|_| "Timestamp second is out of range.".to_string())?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return Err("Timestamp components are out of range.".into());
    }
    Ok(())
}

fn random_hex_id(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buffer);
    hex::encode(buffer)
}

#[derive(Serialize)]
struct DigestInput<'a> {
    id: &'a str,
    operation: &'a str,
    parameters: &'a Value,
    response: &'a Value,
    source: &'a Value,
    metadata: &'a Value,
    captured: &'a str,
}

fn evidence_digest(evidence: &Evidence) -> String {
    let input = DigestInput {
        id: &evidence.id,
        operation: &evidence.operation,
        parameters: &evidence.parameters,
        response: &evidence.response,
        source: &evidence.source,
        metadata: &evidence.metadata,
        captured: &evidence.captured,
    };
    let bytes = serde_json::to_vec(&input).unwrap_or_default();
    format!("0x{}", hex::encode(keccak256(&bytes)))
}

#[derive(Default)]
struct SecretWalk {
    hits: Vec<String>,
    overflow: bool,
}

#[derive(Default)]
struct RedactWalk {
    redacted: Vec<String>,
    overflow: bool,
}

fn scan_secrets(value: &Value, path: &str, walk: &mut SecretWalk, depth: usize) {
    if walk.overflow {
        return;
    }
    if walk.hits.len() >= MAX_SECRET_HITS || depth > MAX_SCAN_DEPTH {
        // Fail closed: an unexamined remainder must not look like a pass.
        walk.overflow = true;
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}/{key}");
                if is_secret_key(key) && !child.is_null() && child.as_str() != Some(REDACTED) {
                    walk.hits.push(child_path.clone());
                }
                scan_secrets(child, &child_path, walk, depth + 1);
                if walk.overflow {
                    return;
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                scan_secrets(child, &format!("{path}[{index}]"), walk, depth + 1);
                if walk.overflow {
                    return;
                }
            }
        }
        Value::String(text) => {
            if is_credential_url(text) {
                walk.hits.push(path.to_string());
            }
        }
        _ => {}
    }
}

fn redact_secrets(value: &mut Value, path: &str, walk: &mut RedactWalk, depth: usize) {
    if walk.overflow {
        return;
    }
    if walk.redacted.len() >= MAX_SECRET_HITS || depth > MAX_SCAN_DEPTH {
        walk.overflow = true;
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let child_path = format!("{path}/{key}");
                if is_secret_key(key) && !child.is_null() && child.as_str() != Some(REDACTED) {
                    walk.redacted.push(child_path);
                    *child = Value::String(REDACTED.to_string());
                    continue;
                }
                redact_secrets(child, &child_path, walk, depth + 1);
                if walk.overflow {
                    return;
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                redact_secrets(child, &format!("{path}[{index}]"), walk, depth + 1);
                if walk.overflow {
                    return;
                }
            }
        }
        Value::String(text) => {
            if let Some(sanitized) = sanitize_url(text) {
                walk.redacted.push(path.to_string());
                *text = sanitized;
            }
        }
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    // Exact matches only: substring matching would flag unrelated fields
    // such as `tokenSymbol` or `tokenAddress` in ERC-20 responses.
    SECRET_KEYS.contains(&key.trim().to_ascii_lowercase().as_str())
}

fn url_param_is_sensitive(param: &str) -> bool {
    let name = param
        .split('=')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    URL_SECRET_PARAMS.contains(&name.as_str())
}

/// Returns the URL with userinfo and credential query parameters removed, or
/// `None` when the text is not a URL or carries no credential material.
/// Everything else about the URL is preserved; path-embedded credentials are
/// out of scope for this bounded filter.
fn sanitize_url(text: &str) -> Option<String> {
    let (scheme, rest) = text.split_once("://")?;
    let (head, fragment) = match rest.split_once('#') {
        Some((head, fragment)) => (head, Some(fragment)),
        None => (rest, None),
    };
    let (before_query, query) = match head.split_once('?') {
        Some((before_query, query)) => (before_query, Some(query)),
        None => (head, None),
    };
    let (authority, path) = match before_query.find('/') {
        Some(index) => (&before_query[..index], Some(&before_query[index..])),
        None => (before_query, None),
    };
    let (host, dropped_userinfo) = match authority.rsplit_once('@') {
        Some((_, host)) => (host, true),
        None => (authority, false),
    };
    let dropped_params = query
        .map(|query| {
            query
                .split('&')
                .filter(|param| url_param_is_sensitive(param))
                .count()
        })
        .unwrap_or(0);
    if !dropped_userinfo && dropped_params == 0 {
        return None;
    }
    let mut cleaned = format!("{scheme}://{host}");
    if let Some(path) = path {
        cleaned.push_str(path);
    }
    if let Some(query) = query {
        let kept: Vec<&str> = query
            .split('&')
            .filter(|param| !url_param_is_sensitive(param))
            .collect();
        if !kept.is_empty() {
            cleaned.push('?');
            cleaned.push_str(&kept.join("&"));
        }
    }
    if let Some(fragment) = fragment {
        cleaned.push('#');
        cleaned.push_str(fragment);
    }
    Some(cleaned)
}

fn is_credential_url(text: &str) -> bool {
    sanitize_url(text).is_some()
}

fn diff_values(base: &str, left: &Value, right: &Value, out: &mut Vec<Value>, depth: usize) {
    if out.len() >= MAX_DIFFERENCES {
        return;
    }
    if left == right {
        return;
    }
    if depth >= MAX_DIFF_DEPTH {
        out.push(json!({ "path": base, "kind": "depth_limit", "left": left, "right": right }));
        return;
    }
    match (left, right) {
        (Value::Object(left_map), Value::Object(right_map)) => {
            let mut keys: Vec<&str> = left_map.keys().map(String::as_str).collect();
            keys.extend(
                right_map
                    .keys()
                    .map(String::as_str)
                    .filter(|key| !left_map.contains_key(*key)),
            );
            keys.sort_unstable();
            for key in keys {
                if out.len() >= MAX_DIFFERENCES {
                    return;
                }
                let child = format!("{base}/{key}");
                match (left_map.get(key), right_map.get(key)) {
                    (Some(left_value), Some(right_value)) => {
                        diff_values(&child, left_value, right_value, out, depth + 1)
                    }
                    (Some(left_value), None) => {
                        out.push(json!({ "path": child, "kind": "removed", "left": left_value }))
                    }
                    (None, Some(right_value)) => {
                        out.push(json!({ "path": child, "kind": "added", "right": right_value }))
                    }
                    (None, None) => {}
                }
            }
        }
        (Value::Array(left_items), Value::Array(right_items)) => {
            for index in 0..left_items.len().max(right_items.len()) {
                if out.len() >= MAX_DIFFERENCES {
                    return;
                }
                let child = format!("{base}[{index}]");
                match (left_items.get(index), right_items.get(index)) {
                    (Some(left_value), Some(right_value)) => {
                        diff_values(&child, left_value, right_value, out, depth + 1)
                    }
                    (Some(left_value), None) => {
                        out.push(json!({ "path": child, "kind": "removed", "left": left_value }))
                    }
                    (None, Some(right_value)) => {
                        out.push(json!({ "path": child, "kind": "added", "right": right_value }))
                    }
                    (None, None) => {}
                }
            }
        }
        _ => out.push(json!({ "path": base, "kind": "changed", "left": left, "right": right })),
    }
}

fn serialize_case_document(file: &CaseFile) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec_pretty(file).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_CASE_FILE_BYTES {
        return Err(
            "The serialized case exceeds the 8 MiB limit; trim the case before saving.".into(),
        );
    }
    Ok(bytes)
}

fn read_case_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let len = checked_regular_file(path)?;
    if len > MAX_CASE_FILE_BYTES {
        return Err(format!(
            "Case file {} exceeds the {} MiB limit.",
            path.display(),
            MAX_CASE_FILE_BYTES / (1024 * 1024)
        ));
    }
    let mut file = std::fs::File::open(path)
        .map_err(|error| format!("Could not read the case file: {error}"))?;
    let mut bytes = Vec::new();
    // The regular-file check and this open are not one atomic step; a document
    // that grows in between is still refused because the read itself is capped.
    file.take(MAX_CASE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the case file: {error}"))?;
    if bytes.len() as u64 > MAX_CASE_FILE_BYTES {
        return Err("Case file exceeds the size limit.".into());
    }
    Ok(bytes)
}

fn checked_regular_file(path: &Path) -> Result<u64, String> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
    if is_reparse_point(&metadata) || !metadata.is_file() {
        return Err("Case files must be regular files, not links or directories.".into());
    }
    Ok(metadata.len())
}

fn read_small(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn ensure_real_directory(path: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    if is_reparse_point(metadata) || !metadata.is_dir() {
        return Err(format!(
            "{} must be a real folder, not a link or file.",
            path.display()
        ));
    }
    Ok(())
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn utc_now() -> String {
    utc_timestamp(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
}

fn utc_timestamp(secs: u64) -> String {
    let (year, month, day) = civil_from_days((secs / 86400) as i64);
    let rest = secs % 86400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// Days-since-epoch to civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Atomic save: temp file + fsync. Updates replace the target via ReplaceFileW
/// on Windows (persist on other platforms); first-time writes use a
/// clobber-refusing persist so an existing case document is never overwritten.
/// Mirrors the session settings saver, duplicated here because that helper is
/// private.
fn atomic_write(path: &Path, bytes: &[u8], overwrite: bool) -> Result<(), String> {
    let dir = path.parent().ok_or("Case file has no parent folder.")?;
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|error| error.to_string())?;
    temp.write_all(bytes).map_err(|error| error.to_string())?;
    temp.as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    let temp = temp.into_temp_path();
    #[cfg(windows)]
    let mut temp = temp;
    #[cfg(windows)]
    if overwrite {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
        use windows_sys::Win32::Storage::FileSystem::{
            ReplaceFileW, SetFileAttributesW, FILE_ATTRIBUTE_NORMAL,
        };

        let target: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let staging: Vec<u16> = temp
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // Both NUL-terminated path buffers remain valid throughout these calls.
        unsafe {
            if SetFileAttributesW(staging.as_ptr(), FILE_ATTRIBUTE_NORMAL) == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if ReplaceFileW(
                target.as_ptr(),
                staging.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            ) != 0
            {
                temp.disable_cleanup(true);
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_FILE_NOT_FOUND as i32) {
                return Err(error.to_string());
            }
        }
    }
    #[cfg(windows)]
    let persisted = temp.persist_noclobber(path).map(|_| ());
    #[cfg(not(windows))]
    let persisted = if overwrite {
        temp.persist(path).map(|_| ())
    } else {
        temp.persist_noclobber(path).map(|_| ())
    };
    persisted.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRepo {
        dir: tempfile::TempDir,
        repo: CaseRepository,
    }

    impl TempRepo {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let repo = CaseRepository::new(dir.path().join("cases-root"));
            Self { dir, repo }
        }

        fn case_path(&self, case_id: &str) -> PathBuf {
            self.dir
                .path()
                .join("cases-root")
                .join("cases")
                .join(format!("{case_id}.json"))
        }
    }

    #[test]
    fn create_list_and_load_roundtrip() {
        let temp = TempRepo::new();
        let summary = temp.repo.create("Incident A").unwrap();
        assert_eq!(summary.name, "Incident A");
        assert_eq!(summary.evidence_count, 0);
        let listed = temp.repo.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, summary.id);
        let file = temp.repo.load(&summary.id).unwrap();
        assert_eq!(file.schema_version, SCHEMA_VERSION);
        assert!(file.evidence.is_empty() && file.notes.is_empty() && file.bookmarks.is_empty());
        assert!(CaseRepository::new(PathBuf::from("relative-root"))
            .create("x")
            .is_err());
    }

    #[test]
    fn case_ids_are_confined_to_hex() {
        let temp = TempRepo::new();
        let summary = temp.repo.create("Ids").unwrap();
        assert!(temp.repo.load(&summary.id).is_ok());
        let file_name = format!("{}.json", summary.id);
        for bad in ["", "../escape", "a/b", "a\\b", "zz", file_name.as_str()] {
            assert!(temp.repo.load(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn evidence_digests_are_stable_keccak_hashes() {
        let temp = TempRepo::new();
        let summary = temp.repo.create("Digests").unwrap();
        let evidence = temp
            .repo
            .append_evidence(
                &summary.id,
                "eth_call",
                &json!({"a": 1}),
                &json!({"b": 2}),
                &json!({}),
            )
            .unwrap();
        assert_eq!(evidence.digest.len(), 66);
        assert!(evidence.digest.starts_with("0x"));
        let reloaded = temp.repo.load(&summary.id).unwrap();
        assert_eq!(reloaded.evidence[0].digest, evidence.digest);
        assert_eq!(reloaded.evidence[0].operation, "eth_call");
    }

    #[test]
    fn stored_evidence_tampering_fails_digest_validation() {
        let temp = TempRepo::new();
        let summary = temp.repo.create("Tamper").unwrap();
        temp.repo
            .append_evidence(
                &summary.id,
                "eth_call",
                &json!({}),
                &json!({"status": "0x1"}),
                &json!({}),
            )
            .unwrap();
        let path = temp.case_path(&summary.id);
        let text = std::fs::read_to_string(&path).unwrap();
        let tampered = text.replace("\"0x1\"", "\"0x2\"");
        assert_ne!(text, tampered);
        std::fs::write(&path, tampered).unwrap();
        assert!(temp.repo.load(&summary.id).unwrap_err().contains("digest"));
    }

    #[test]
    fn load_rejects_case_id_filename_mismatch() {
        let temp = TempRepo::new();
        let summary = temp.repo.create("Mismatch").unwrap();
        let path = temp.case_path(&summary.id);
        let text = std::fs::read_to_string(&path).unwrap();
        let swapped = text.replace(
            &format!("\"id\": \"{}\"", summary.id),
            "\"id\": \"00112233445566778899aabbccddeeff\"",
        );
        assert_ne!(text, swapped);
        std::fs::write(&path, swapped).unwrap();
        assert!(temp.repo.load(&summary.id).is_err());
    }

    #[test]
    fn notes_and_bookmarks_stay_separate_from_evidence() {
        let temp = TempRepo::new();
        let summary = temp.repo.create("Notes").unwrap();
        let evidence = temp
            .repo
            .append_evidence(&summary.id, "eth_call", &json!({}), &json!({}), &json!({}))
            .unwrap();
        let before = temp.repo.load(&summary.id).unwrap();
        temp.repo
            .annotate(&summary.id, &evidence.id, "  check this  ", true)
            .unwrap();
        let after = temp.repo.load(&summary.id).unwrap();
        assert_eq!(after.evidence.len(), before.evidence.len());
        assert_eq!(after.evidence[0].digest, before.evidence[0].digest);
        assert_eq!(after.notes.len(), 1);
        assert_eq!(after.notes[0].note, "check this");
        assert_eq!(after.notes[0].evidence_id, evidence.id);
        assert_eq!(after.bookmarks.len(), 1);
        assert_eq!(after.bookmarks[0].evidence_id, evidence.id);
        assert!(temp
            .repo
            .annotate(&summary.id, &evidence.id, "   ", false)
            .is_err());
        assert!(temp
            .repo
            .annotate(&summary.id, "ffffffffffffffffffffffff", "missing", false)
            .is_err());
        temp.repo
            .annotate(&summary.id, &evidence.id, "again", true)
            .unwrap();
        let latest = temp.repo.load(&summary.id).unwrap();
        assert_eq!(latest.notes.len(), 2);
        assert_eq!(latest.bookmarks.len(), 1);
    }

    #[test]
    fn export_roundtrips_and_tampered_exports_are_rejected() {
        let source = TempRepo::new();
        let summary = source.repo.create("Roundtrip").unwrap();
        source
            .repo
            .append_evidence(
                &summary.id,
                "eth_call",
                &json!({"to": "0x1"}),
                &json!({"ok": true}),
                &json!({"rpc": "http://127.0.0.1:8545"}),
            )
            .unwrap();
        let exported = source.repo.export_json(&summary.id).unwrap();
        let target = TempRepo::new();
        let imported = target.repo.import_json(&exported).unwrap();
        assert_eq!(imported.id, summary.id);
        assert_eq!(imported.evidence_count, 1);
        let mut value: Value = serde_json::from_slice(&exported).unwrap();
        value["evidence"][0]["response"] = json!({"ok": false});
        assert!(target
            .repo
            .import_json(&serde_json::to_vec(&value).unwrap())
            .is_err());
    }

    #[test]
    fn import_enforces_schema_version_and_size_bound() {
        let temp = TempRepo::new();
        let value = serde_json::to_value(CaseFile {
            schema_version: SCHEMA_VERSION,
            id: "00112233445566778899aabbccddeeff".into(),
            name: "Schema".into(),
            created: "2026-01-01T00:00:00Z".into(),
            evidence: Vec::new(),
            notes: Vec::new(),
            bookmarks: Vec::new(),
        })
        .unwrap();
        let mut wrong_version = value.clone();
        wrong_version["schemaVersion"] = json!(2);
        assert!(temp
            .repo
            .import_json(&serde_json::to_vec(&wrong_version).unwrap())
            .is_err());
        let mut unknown_field = value.clone();
        unknown_field["extraTopLevel"] = json!(true);
        assert!(temp
            .repo
            .import_json(&serde_json::to_vec(&unknown_field).unwrap())
            .is_err());
        let oversized = vec![b'a'; MAX_IMPORT_BYTES + 1];
        assert!(temp.repo.import_json(&oversized).is_err());
    }

    #[test]
    fn import_rejects_secrets_and_broken_references() {
        let temp = TempRepo::new();
        let evidence = |parameters: Value, response: Value| -> Evidence {
            let mut evidence = Evidence {
                id: "aabbccdd1122334455667788".into(),
                operation: "eth_call".into(),
                parameters,
                response,
                source: json!({}),
                metadata: json!({}),
                captured: "2026-01-01T00:00:00Z".into(),
                digest: String::new(),
            };
            evidence.digest = evidence_digest(&evidence);
            evidence
        };
        let base = |evidence: Evidence, notes: Vec<CaseNote>| -> CaseFile {
            CaseFile {
                schema_version: SCHEMA_VERSION,
                id: "00112233445566778899aabbccddeeff".into(),
                name: "Imported".into(),
                created: "2026-01-01T00:00:00Z".into(),
                evidence: vec![evidence],
                notes,
                bookmarks: Vec::new(),
            }
        };
        let file = base(
            evidence(json!({"privateKey": "0xabc"}), json!({"status": "0x1"})),
            Vec::new(),
        );
        let bytes = serde_json::to_vec(&file).unwrap();
        assert!(temp
            .repo
            .import_json(&bytes)
            .unwrap_err()
            .contains("credentials"));
        let file = base(
            evidence(json!({}), json!({"rpc": "https://node/?key=secret"})),
            Vec::new(),
        );
        let bytes = serde_json::to_vec(&file).unwrap();
        assert!(temp
            .repo
            .import_json(&bytes)
            .unwrap_err()
            .contains("credentials"));
        let note = CaseNote {
            id: "deadbeefdeadbeef".into(),
            evidence_id: "00000000000000000000000f".into(),
            note: "x".into(),
            created: "2026-01-01T00:00:00Z".into(),
        };
        let file = base(evidence(json!({}), json!({"status": "0x1"})), vec![note]);
        let bytes = serde_json::to_vec(&file).unwrap();
        assert!(temp
            .repo
            .import_json(&bytes)
            .unwrap_err()
            .contains("references"));
        let file = base(evidence(json!({}), json!({"status": "0x1"})), Vec::new());
        let bytes = serde_json::to_vec(&file).unwrap();
        let summary = temp.repo.import_json(&bytes).unwrap();
        assert_eq!(summary.id, file.id);
        assert!(temp.repo.import_json(&bytes).is_err());
        let mut file = base(evidence(json!({}), json!({"status": "0x1"})), Vec::new());
        file.id = "../escape".into();
        assert!(temp
            .repo
            .import_json(&serde_json::to_vec(&file).unwrap())
            .is_err());
    }

    #[test]
    fn import_validates_field_bounds() {
        let temp = TempRepo::new();
        let evidence = || -> Evidence {
            let mut evidence = Evidence {
                id: "aabbccdd1122334455667788".into(),
                operation: "eth_call".into(),
                parameters: json!({}),
                response: json!({"status": "0x1"}),
                source: json!({}),
                metadata: json!({}),
                captured: "2026-01-01T00:00:00Z".into(),
                digest: String::new(),
            };
            evidence.digest = evidence_digest(&evidence);
            evidence
        };
        let base = |created: &str, evidence: Vec<Evidence>, notes: Vec<CaseNote>| -> CaseFile {
            CaseFile {
                schema_version: SCHEMA_VERSION,
                id: "00112233445566778899aabbccddeeff".into(),
                name: "Bounds".into(),
                created: created.to_string(),
                evidence,
                notes,
                bookmarks: Vec::new(),
            }
        };
        assert!(temp
            .repo
            .import_json(
                &serde_json::to_vec(&base("January 1 2026", vec![evidence()], Vec::new())).unwrap()
            )
            .unwrap_err()
            .contains("Timestamp"));
        let mut bad = base("2026-01-01T00:00:00Z", vec![evidence()], Vec::new());
        bad.evidence[0].captured = "2026-01-01".into();
        assert!(temp
            .repo
            .import_json(&serde_json::to_vec(&bad).unwrap())
            .unwrap_err()
            .contains("Timestamp"));
        let mut bad = base("2026-01-01T00:00:00Z", vec![evidence()], Vec::new());
        bad.evidence[0].operation = "   ".into();
        assert!(temp
            .repo
            .import_json(&serde_json::to_vec(&bad).unwrap())
            .unwrap_err()
            .contains("operation"));
        let notes = vec![CaseNote {
            id: "deadbeefdeadbeef".into(),
            evidence_id: "aabbccdd1122334455667788".into(),
            note: "x".repeat(MAX_NOTE_BYTES + 1),
            created: "2026-01-01T00:00:00Z".into(),
        }];
        assert!(temp
            .repo
            .import_json(
                &serde_json::to_vec(&base("2026-01-01T00:00:00Z", vec![evidence()], notes))
                    .unwrap()
            )
            .unwrap_err()
            .contains("limit"));
        let mut bad = base("2026-01-01T00:00:00Z", vec![evidence()], Vec::new());
        bad.name = "n".repeat(MAX_NAME_BYTES + 1);
        assert!(temp
            .repo
            .import_json(&serde_json::to_vec(&bad).unwrap())
            .is_err());
    }

    #[test]
    fn screening_fails_closed_beyond_scan_bounds() {
        let temp = TempRepo::new();
        let summary = temp.repo.create("Deep").unwrap();
        let mut deep = json!({"leaf": 1});
        for _ in 0..40 {
            deep = json!({ "nested": deep });
        }
        let error = temp
            .repo
            .append_evidence(&summary.id, "eth_call", &deep, &json!({}), &json!({}))
            .unwrap_err();
        assert!(error.contains("fully screened"));
        let mut evidence = Evidence {
            id: "aabbccdd1122334455667788".into(),
            operation: "eth_call".into(),
            parameters: deep,
            response: json!({}),
            source: json!({}),
            metadata: json!({}),
            captured: "2026-01-01T00:00:00Z".into(),
            digest: String::new(),
        };
        evidence.digest = evidence_digest(&evidence);
        let file = CaseFile {
            schema_version: SCHEMA_VERSION,
            id: "00112233445566778899aabbccddeeff".into(),
            name: "Deep".into(),
            created: "2026-01-01T00:00:00Z".into(),
            evidence: vec![evidence],
            notes: Vec::new(),
            bookmarks: Vec::new(),
        };
        let error = temp
            .repo
            .import_json(&serde_json::to_vec(&file).unwrap())
            .unwrap_err();
        assert!(error.contains("fully screened"));
    }

    #[test]
    fn secrets_are_redacted_on_capture_and_absent_from_exports() {
        let temp = TempRepo::new();
        let summary = temp.repo.create("Secrets").unwrap();
        let evidence = temp
            .repo
            .append_evidence(
                &summary.id,
                "send_tx",
                &json!({}),
                &json!({
                    "authorization": "Bearer abc",
                    "rpc": "http://user:pass@127.0.0.1:8545",
                    "explorer": "http://127.0.0.1:8080/tx/0xabc",
                    "node": "https://mainnet.example/api/?token=secret&block=1",
                    "status": "0x1"
                }),
                &json!({}),
            )
            .unwrap();
        assert_eq!(evidence.response["authorization"].as_str(), Some(REDACTED));
        assert_eq!(
            evidence.response["rpc"].as_str(),
            Some("http://127.0.0.1:8545")
        );
        assert_eq!(
            evidence.response["explorer"].as_str(),
            Some("http://127.0.0.1:8080/tx/0xabc")
        );
        assert_eq!(
            evidence.response["node"].as_str(),
            Some("https://mainnet.example/api/?block=1")
        );
        assert_eq!(evidence.response["status"].as_str(), Some("0x1"));
        let keys = evidence.metadata["redactedKeys"].as_array().unwrap();
        assert!(keys
            .iter()
            .any(|key| key.as_str() == Some("response/authorization")));
        assert!(keys.iter().any(|key| key.as_str() == Some("response/rpc")));
        assert!(keys.iter().any(|key| key.as_str() == Some("response/node")));
        let exported = String::from_utf8(temp.repo.export_json(&summary.id).unwrap()).unwrap();
        assert!(!exported.contains("Bearer abc"));
        assert!(!exported.contains("user:pass"));
        assert!(!exported.contains("token=secret"));
        let again = TempRepo::new();
        assert!(again
            .repo
            .import_json(&temp.repo.export_json(&summary.id).unwrap())
            .is_ok());
    }

    #[test]
    fn compare_reports_bounded_field_paths() {
        let temp = TempRepo::new();
        let summary = temp.repo.create("Compare").unwrap();
        let left = temp
            .repo
            .append_evidence(
                &summary.id,
                "eth_call",
                &json!({"to": "0xaaa", "data": "0x1"}),
                &json!({"ok": true}),
                &json!({}),
            )
            .unwrap();
        let right = temp
            .repo
            .append_evidence(
                &summary.id,
                "eth_call",
                &json!({"to": "0xbbb", "data": "0x1", "extra": 1}),
                &json!({"ok": false}),
                &json!({}),
            )
            .unwrap();
        let report = temp.repo.compare(&summary.id, &left.id, &right.id).unwrap();
        let paths: Vec<String> = report["differences"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|difference| difference["path"].as_str().map(str::to_string))
            .collect();
        assert!(paths.contains(&"parameters/to".to_string()));
        assert!(paths.contains(&"parameters/extra".to_string()));
        assert!(paths.contains(&"response/ok".to_string()));
        assert_eq!(report["differences"].as_array().unwrap().len(), 3);
        assert_eq!(report["identical"].as_bool(), Some(false));
        let same = temp.repo.compare(&summary.id, &left.id, &left.id).unwrap();
        assert_eq!(same["identical"].as_bool(), Some(true));
        assert!(temp.repo.compare(&summary.id, "nothex", &right.id).is_err());
    }

    #[test]
    fn timestamps_are_utc_rfc3339() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(utc_timestamp(1_000_000_000), "2001-09-09T01:46:40Z");
    }

    #[test]
    fn inspect_project_lists_only_source_abi_and_build_info_names() {
        let dir = tempfile::Builder::new()
            .prefix("chasm-passive-fixture-")
            .tempdir()
            .unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/Vault.sol"), "contract Vault {}").unwrap();
        std::fs::create_dir_all(root.join("out/build-info")).unwrap();
        std::fs::write(root.join("out/build-info/info.json"), r#"{"id":"x"}"#).unwrap();
        std::fs::write(root.join("abi.json"), "[{\"type\":\"function\"}]").unwrap();
        std::fs::write(root.join("notes.txt"), "ignored").unwrap();
        std::fs::write(root.join(".env"), "SECRET=1").unwrap();
        let report = inspect_project(root).unwrap();
        let sources = report["sources"].as_array().unwrap();
        assert!(sources
            .iter()
            .any(|item| item["path"].as_str() == Some("src/Vault.sol")));
        assert!(report["abis"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["path"].as_str() == Some("abi.json")));
        assert!(report["buildInfo"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["path"].as_str() == Some("out/build-info/info.json")));
        assert_eq!(report["truncated"].as_bool(), Some(false));
        let text = serde_json::to_string(&report).unwrap();
        assert!(!text.contains(".env") && !text.contains("SECRET") && !text.contains("notes.txt"));
        assert!(inspect_project(Path::new("relative")).is_err());
    }
}
