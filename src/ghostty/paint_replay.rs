use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{ActiveScreen, Terminal, MODE_SYNCHRONIZED_OUTPUT};

const RAW_LIMIT: usize = 2 * 1024 * 1024 + 8192;
const DOCUMENT_LIMIT: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    evaluated_sha256: String,
    raw_length: usize,
    cols: u16,
    rows: u16,
    replay_scrollback_bytes: usize,
    needle: String,
    watermark: usize,
    clean_length: Option<usize>,
    trigger: String,
    chunk_ends: Vec<usize>,
    metadata_complete: bool,
    reader_state: String,
    reader_error: Option<ReaderError>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReaderError {
    kind: String,
    raw_os_error: Option<i32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    raw_sha256: String,
    raw_length: usize,
    manifest_sha256: String,
}

struct ReplayCapture {
    raw: Vec<u8>,
    manifest: Manifest,
    manifest_sha256: String,
    complete_sha256: String,
}

struct ReplayBudget {
    regex_prefix_bytes: usize,
    byte_samples: usize,
}

impl Default for ReplayBudget {
    fn default() -> Self {
        Self {
            regex_prefix_bytes: 64 * 1024 * 1024,
            byte_samples: 65536,
        }
    }
}

fn paint_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn validate_paint_capture(
    raw: Vec<u8>,
    manifest_bytes: &[u8],
    complete_bytes: &[u8],
    expected: &str,
) -> io::Result<ReplayCapture> {
    if raw.len() > RAW_LIMIT
        || manifest_bytes.len() > DOCUMENT_LIMIT
        || complete_bytes.len() > DOCUMENT_LIMIT
        || expected.len() != 64
        || !expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("capture size or external digest is invalid"));
    }
    let shape: Value = serde_json::from_slice(manifest_bytes)?;
    if shape.get("clean_length").is_none() || shape.get("reader_error").is_none() {
        return Err(invalid("nullable manifest fields must be explicit"));
    }
    let manifest: Manifest = serde_json::from_slice(manifest_bytes)?;
    let complete: Completion = serde_json::from_slice(complete_bytes)?;
    let manifest_sha256 = paint_digest(manifest_bytes);
    if paint_digest(&raw) != expected
        || manifest.evaluated_sha256 != expected
        || complete.raw_sha256 != expected
        || complete.raw_length != raw.len()
        || manifest.raw_length != raw.len()
        || complete.manifest_sha256 != manifest_sha256
    {
        return Err(invalid(
            "capture bytes, manifest, completion or external digest disagree",
        ));
    }
    if manifest.version != 1
        || (manifest.cols, manifest.rows) != (106, 34)
        || manifest.replay_scrollback_bytes != 0
        || manifest.needle.is_empty()
        || !matches!(manifest.trigger.as_str(), "SIZE_BOUND" | "PAINT_DEADLINE")
        || (manifest.trigger == "SIZE_BOUND" && manifest.clean_length.is_some())
        || manifest.chunk_ends.len() > 4096
    {
        return Err(invalid("invalid diagnostic schema, geometry or metadata"));
    }
    match (manifest.reader_state.as_str(), &manifest.reader_error) {
        ("READING" | "EOF" | "CAP_STOP", None) => {}
        ("READ_ERROR", Some(error)) if !error.kind.is_empty() => {}
        _ => return Err(invalid("unknown or inconsistent reader state")),
    }
    let mut start = 0;
    for &end in &manifest.chunk_ends {
        if end <= start || end > raw.len() || end - start > 8192 {
            return Err(invalid("chunk endpoints are not a bounded ordered prefix"));
        }
        start = end;
    }
    if manifest.metadata_complete && start != raw.len() {
        return Err(invalid("complete metadata does not cover the raw capture"));
    }
    Ok(ReplayCapture {
        raw,
        manifest,
        manifest_sha256,
        complete_sha256: paint_digest(complete_bytes),
    })
}

fn private_directory(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err(invalid("artifact directory must be an absolute UTF-8 path"));
    }
    let metadata = fs::symlink_metadata(path)?;
    // getuid has no pointer arguments or process-state mutation.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o7777 != 0o700
        || fs::canonicalize(path)? != path
    {
        return Err(invalid(
            "artifact directory must be canonical, owned and private",
        ));
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    if !path.is_absolute() || path.to_str().is_none() || fs::canonicalize(path)? != path {
        return Err(invalid("input must be an absolute canonical UTF-8 path"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    // getuid has no pointer arguments or process-state mutation.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.len() > limit as u64
    {
        return Err(invalid(
            "input must be an owned mode-0600 bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid("input grew beyond its bound"));
    }
    Ok(bytes)
}

fn read_paint_capture(directory: &Path, expected: &str) -> io::Result<ReplayCapture> {
    private_directory(directory)?;
    let raw = read_bounded(&directory.join("raw.bin"), RAW_LIMIT)?;
    let manifest = read_bounded(&directory.join("manifest.json"), DOCUMENT_LIMIT)?;
    let complete = read_bounded(&directory.join("complete.json"), DOCUMENT_LIMIT)?;
    validate_paint_capture(raw, &manifest, &complete, expected)
}

#[derive(Default)]
struct Submission {
    bytes: usize,
    digest: Sha256,
}

impl Submission {
    fn write(&mut self, terminal: &mut Terminal, bytes: &[u8]) {
        terminal.write(bytes);
        self.digest.update(bytes);
        self.bytes += bytes.len();
    }

    fn sha256(&self) -> String {
        format!("{:x}", self.digest.clone().finalize())
    }
}

fn observer(manifest: &Manifest, errors: &mut Vec<String>) -> io::Result<Option<Terminal>> {
    let terminal = match Terminal::new(
        manifest.cols,
        manifest.rows,
        manifest.replay_scrollback_bytes,
    ) {
        Ok(terminal) => terminal,
        Err(error) => {
            errors.push(format!("constructor: {error}"));
            return Ok(None);
        }
    };
    let geometry = terminal
        .cols()
        .and_then(|cols| terminal.rows().map(|rows| (cols, rows)));
    match geometry {
        Ok(value) if value == (manifest.cols, manifest.rows) => Ok(Some(terminal)),
        Ok(_) => Err(invalid("replay geometry differs from the bound fixture")),
        Err(error) => {
            errors.push(format!("geometry query: {error}"));
            Ok(None)
        }
    }
}

fn sample(terminal: &Terminal, manifest: &Manifest) -> io::Result<(bool, &'static str, bool)> {
    let text = terminal
        .read_text_viewport(
            (0, 0),
            (manifest.cols - 1, u32::from(manifest.rows - 1)),
            false,
        )
        .map_err(io::Error::other)?;
    let screen = match terminal.active_screen().map_err(io::Error::other)? {
        ActiveScreen::Primary => "PRIMARY",
        ActiveScreen::Alternate => "ALTERNATE",
    };
    let synchronized = terminal
        .mode_get(MODE_SYNCHRONIZED_OUTPUT)
        .map_err(io::Error::other)?;
    Ok((text.contains(&manifest.needle), screen, synchronized))
}

fn replay_paint_capture(capture: &ReplayCapture, budget: ReplayBudget) -> io::Result<Value> {
    let manifest = &capture.manifest;
    let raw = &capture.raw;
    let mut reasons = Vec::new();
    if matches!(manifest.reader_state.as_str(), "READ_ERROR" | "CAP_STOP") {
        reasons.push(manifest.reader_state.as_str());
    }
    if !manifest.metadata_complete {
        reasons.push("METADATA_LIMIT");
    }
    let mut errors = Vec::new();
    let mut chunk_terminal = observer(manifest, &mut errors)?;
    let geometry_observed = chunk_terminal.is_some();
    let mut chunk_submission = Submission::default();
    let mut chunk_samples = Vec::new();
    let mut submitted_ends = Vec::new();
    let mut regex_prefix_bytes = 0usize;
    let mut chunk_matches = BTreeMap::new();
    let mut chunk_seen = false;
    let mut chunk_later_absence = false;
    let mut disagreement = false;
    let osc = regex::Regex::new(r"(?s)\x1b\].*?(?:\x07|\x1b\\)").map_err(io::Error::other)?;
    let trailing_osc = regex::Regex::new(r"(?s)\x1b\].*$").map_err(io::Error::other)?;
    let csi = regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").map_err(io::Error::other)?;
    let chunks: Vec<_> = manifest
        .chunk_ends
        .iter()
        .scan(0, |start, &end| {
            let chunk = (*start, end);
            *start = end;
            Some(chunk)
        })
        .collect();
    if let Some(terminal) = &mut chunk_terminal {
        for &(start, end) in &chunks {
            let Some(charged) = regex_prefix_bytes
                .checked_add(end)
                .filter(|&n| n <= budget.regex_prefix_bytes)
            else {
                reasons.push("REGEX_BUDGET");
                break;
            };
            regex_prefix_bytes = charged;
            chunk_submission.write(terminal, &raw[start..end]);
            submitted_ends.push(end);
            let (cell_match, screen, synchronized_output) = match sample(terminal, manifest) {
                Ok(value) => value,
                Err(error) => {
                    errors.push(format!("chunk prefix {end}: {error}"));
                    break;
                }
            };
            let text = String::from_utf8_lossy(&raw[..end]);
            let without_osc = osc.replace_all(&text, "");
            let complete = trailing_osc.replace_all(&without_osc, "");
            let clean = csi.replace_all(&complete, "");
            let regex_literal_match = clean.contains(&manifest.needle);
            let regex_predicate_match = clean
                .get(manifest.watermark..)
                .is_some_and(|tail| tail.contains(&manifest.needle));
            chunk_samples.push(json!({"end_offset": end, "cell_match": cell_match,
                "regex_literal_match": regex_literal_match, "regex_predicate_match": regex_predicate_match,
                "screen": screen, "synchronized_output": synchronized_output}));
            chunk_matches.insert(end, cell_match);
            chunk_later_absence |= chunk_seen && !cell_match;
            chunk_seen |= cell_match;
            disagreement |= cell_match && !regex_literal_match;
        }
    }
    let expected_ends: Vec<_> = manifest
        .chunk_ends
        .iter()
        .copied()
        .take(submitted_ends.len())
        .collect();
    let chunk_integrity = submitted_ends == expected_ends
        && chunk_submission.bytes <= raw.len()
        && chunk_submission.sha256() == paint_digest(&raw[..chunk_submission.bytes.min(raw.len())]);
    let chunk_complete = geometry_observed
        && chunk_integrity
        && manifest.metadata_complete
        && chunk_submission.bytes == raw.len()
        && chunk_samples.len() == manifest.chunk_ends.len();

    let mut byte_terminal = observer(manifest, &mut errors)?;
    let byte_geometry_observed = byte_terminal.is_some();
    let mut byte_submission = Submission::default();
    let mut byte_samples = 0usize;
    let mut first_cell_match = None;
    let mut first_later_absence = None;
    let mut match_transitions = 0usize;
    let mut absence_transitions = 0usize;
    let mut previous_match = false;
    let mut endpoint_disagreements = Vec::new();
    if let Some(terminal) = &mut byte_terminal {
        for (offset, byte) in raw.iter().enumerate() {
            if byte_samples >= budget.byte_samples {
                reasons.push("BYTE_BUDGET");
                break;
            }
            byte_submission.write(terminal, std::slice::from_ref(byte));
            let end = offset + 1;
            let (cell_match, _, _) = match sample(terminal, manifest) {
                Ok(value) => value,
                Err(error) => {
                    errors.push(format!("byte prefix {end}: {error}"));
                    break;
                }
            };
            byte_samples += 1;
            if cell_match && first_cell_match.is_none() {
                first_cell_match = Some(end);
            }
            if !cell_match && first_cell_match.is_some() && first_later_absence.is_none() {
                first_later_absence = Some(end);
            }
            match_transitions += usize::from(cell_match && !previous_match);
            absence_transitions += usize::from(!cell_match && previous_match);
            previous_match = cell_match;
            if chunk_matches
                .get(&end)
                .is_some_and(|&value| value != cell_match)
            {
                endpoint_disagreements.push(end);
            }
        }
    }
    let byte_integrity = byte_submission.bytes <= raw.len()
        && byte_submission.sha256() == paint_digest(&raw[..byte_submission.bytes.min(raw.len())]);
    let byte_complete = byte_geometry_observed
        && byte_integrity
        && byte_submission.bytes == raw.len()
        && byte_samples == raw.len();
    if !chunk_integrity || !byte_integrity || !endpoint_disagreements.is_empty() {
        reasons.push("SUBMISSION_MISMATCH");
    }
    if !errors.is_empty() {
        reasons.push("QUERY_ERROR");
    }
    let mut facts = Vec::new();
    if !raw
        .windows(manifest.needle.len())
        .any(|bytes| bytes == manifest.needle.as_bytes())
    {
        facts.push("RAW_LITERAL_ABSENT");
    }
    if chunk_seen || first_cell_match.is_some() {
        facts.push("CELL_MATCH_OBSERVED");
    } else {
        facts.push("NO_CELL_MATCH_AT_OBSERVED_PREFIXES");
    }
    if chunk_later_absence || first_later_absence.is_some() {
        facts.push("LATER_CELL_ABSENCE");
    }
    if disagreement {
        facts.push("CELL_REGEX_DISAGREEMENT");
    }
    if !reasons.is_empty() || !chunk_complete || !byte_complete {
        facts.push("INDETERMINATE");
    }
    Ok(json!({
        "geometry": if geometry_observed || byte_geometry_observed { json!({"cols": manifest.cols, "rows": manifest.rows}) } else { Value::Null },
        "replay_scrollback_bytes": manifest.replay_scrollback_bytes,
        "evaluated_sha256": manifest.evaluated_sha256,
        "raw_length": raw.len(), "needle": manifest.needle,
        "reader_state": manifest.reader_state,
        "reader_error": manifest.reader_error.as_ref().map(|error| json!({"kind": error.kind, "raw_os_error": error.raw_os_error})),
        "metadata_complete": manifest.metadata_complete,
        "facts": facts, "indeterminate_reasons": reasons, "query_errors": errors,
        "chunk_pass": {"complete": chunk_complete, "submitted_bytes": chunk_submission.bytes,
            "submitted_sha256": chunk_submission.sha256(), "submitted_ends": submitted_ends,
            "regex_prefix_bytes": regex_prefix_bytes, "regex_prefix_budget": budget.regex_prefix_bytes,
            "samples": chunk_samples},
        "byte_pass": {"complete": byte_complete, "submitted_bytes": byte_submission.bytes,
            "submitted_sha256": byte_submission.sha256(), "samples": byte_samples, "sample_budget": budget.byte_samples,
            "first_cell_match": first_cell_match, "first_later_absence": first_later_absence,
            "match_transitions": match_transitions, "absence_transitions": absence_transitions},
        "endpoint_disagreements": endpoint_disagreements,
        "observation_domains": {"raw": "retained byte buffer", "chunk": "parser viewport at reader chunk endpoints", "byte": "parser viewport at bounded byte prefixes", "legacy_regex": "same raw prefix, watermark applies only to cleaned predicate"},
        "limit": "Captured outer PTY prefix, not displayed frames or inner pane state. Later absence has no causal classification; raw literal absence does not imply cell absence. READING and EOF do not establish that all intended producer output was captured."
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayRequest {
    artifact_directory: PathBuf,
    evaluated_sha256: String,
}

fn replay_request(input: Option<&Path>) -> io::Result<Option<Value>> {
    let Some(input) = input else { return Ok(None) };
    let bytes = read_bounded(input, DOCUMENT_LIMIT)?;
    let request: ReplayRequest = serde_json::from_slice(&bytes)?;
    let capture = read_paint_capture(&request.artifact_directory, &request.evaluated_sha256)?;
    let mut report = replay_paint_capture(&capture, ReplayBudget::default())?;
    report["requested"] = json!(true);
    report["input"] = json!({"path": input, "sha256": paint_digest(&bytes)});
    report["artifact"] = json!({"directory": request.artifact_directory,
        "raw_sha256": capture.manifest.evaluated_sha256,
        "manifest_sha256": capture.manifest_sha256, "complete_sha256": capture.complete_sha256});
    Ok(Some(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mpd_parts(chunks: &[&[u8]]) -> (Vec<u8>, serde_json::Value) {
        let mut raw = Vec::new();
        let mut ends = Vec::new();
        for chunk in chunks {
            raw.extend_from_slice(chunk);
            ends.push(raw.len());
        }
        let manifest = serde_json::json!({
            "version": 1,
            "evaluated_sha256": paint_digest(&raw),
            "raw_length": raw.len(),
            "cols": 106,
            "rows": 34,
            "replay_scrollback_bytes": 0,
            "needle": "PAINTFIRST",
            "watermark": 0,
            "clean_length": 0,
            "trigger": "PAINT_DEADLINE",
            "chunk_ends": ends,
            "metadata_complete": true,
            "reader_state": "EOF",
            "reader_error": null
        });
        (raw, manifest)
    }

    fn mpd_encoded(manifest: &serde_json::Value) -> (Vec<u8>, Vec<u8>) {
        let bytes = serde_json::to_vec(manifest).unwrap();
        let complete = serde_json::to_vec(&serde_json::json!({
            "raw_sha256": manifest["evaluated_sha256"],
            "raw_length": manifest["raw_length"],
            "manifest_sha256": paint_digest(&bytes)
        }))
        .unwrap();
        (bytes, complete)
    }

    fn mpd_validated(raw: Vec<u8>, manifest: serde_json::Value) -> ReplayCapture {
        let expected = manifest["evaluated_sha256"].as_str().unwrap();
        let (manifest_bytes, complete) = mpd_encoded(&manifest);
        validate_paint_capture(raw, &manifest_bytes, &complete, expected).unwrap()
    }

    fn mpd_replay(chunks: &[&[u8]]) -> serde_json::Value {
        let (raw, manifest) = mpd_parts(chunks);
        replay_paint_capture(&mpd_validated(raw, manifest), ReplayBudget::default()).unwrap()
    }

    fn mpd_has(report: &serde_json::Value, fact: &str) -> bool {
        report["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == fact)
    }

    #[test]
    fn mpd_raw_title_and_cells_remain_distinct_observations() {
        let absent = mpd_replay(&[b"other"]);
        assert!(mpd_has(&absent, "RAW_LITERAL_ABSENT"));
        assert!(mpd_has(&absent, "NO_CELL_MATCH_AT_OBSERVED_PREFIXES"));
        assert!(!mpd_has(&absent, "CELL_MATCH_OBSERVED"));
        let title = mpd_replay(&[b"\x1b]2;PAINTFIRST\x07"]);
        assert!(!mpd_has(&title, "RAW_LITERAL_ABSENT"));
        assert!(mpd_has(&title, "NO_CELL_MATCH_AT_OBSERVED_PREFIXES"));
        assert!(!mpd_has(&title, "CELL_MATCH_OBSERVED"));
        let cells = mpd_replay(&[b"PAINTFIRST"]);
        assert!(mpd_has(&cells, "CELL_MATCH_OBSERVED"));
        assert!(!mpd_has(&cells, "NO_CELL_MATCH_AT_OBSERVED_PREFIXES"));
        assert!(!mpd_has(&cells, "INDETERMINATE"));
        assert_eq!(cells["byte_pass"]["first_cell_match"], 10);
    }

    #[test]
    fn mpd_cancelled_osc_exposes_regex_disagreement_without_later_absence() {
        let report = mpd_replay(&[b"\x1b]2;unfinished\x18PAINTFIRST"]);
        assert!(mpd_has(&report, "CELL_REGEX_DISAGREEMENT"));
        assert!(!mpd_has(&report, "LATER_CELL_ABSENCE"));
        assert_eq!(report["chunk_pass"]["samples"][0]["cell_match"], true);
        assert_eq!(
            report["chunk_pass"]["samples"][0]["regex_literal_match"],
            false
        );
        assert_eq!(
            report["chunk_pass"]["samples"][0]["regex_predicate_match"],
            false
        );
        assert_eq!(report["chunk_pass"]["complete"], true);
        assert_eq!(report["byte_pass"]["complete"], true);
    }

    #[test]
    fn mpd_later_absence_is_not_regex_disagreement() {
        let report = mpd_replay(&[b"PAINTFIRST", b"\x1b[2J\x1b[H"]);
        assert!(mpd_has(&report, "LATER_CELL_ABSENCE"));
        assert!(!mpd_has(&report, "CELL_REGEX_DISAGREEMENT"));
        assert_eq!(report["chunk_pass"]["samples"][0]["cell_match"], true);
        assert_eq!(report["chunk_pass"]["samples"][1]["cell_match"], false);
        assert_eq!(
            report["chunk_pass"]["samples"][1]["regex_literal_match"],
            true
        );
        assert!(report["byte_pass"]["first_later_absence"].as_u64().unwrap() > 10);
    }

    #[test]
    fn mpd_same_chunk_transient_is_observed_only_by_byte_prefix_pass() {
        let report = mpd_replay(&[b"PAINTFIRST\x1b[2J\x1b[H"]);
        assert_eq!(report["chunk_pass"]["samples"][0]["cell_match"], false);
        assert_eq!(report["byte_pass"]["first_cell_match"], 10);
        assert!(report["byte_pass"]["first_later_absence"].as_u64().unwrap() > 10);
        assert!(mpd_has(&report, "CELL_MATCH_OBSERVED"));
        assert!(mpd_has(&report, "LATER_CELL_ABSENCE"));
        assert!(!mpd_has(&report, "NO_CELL_MATCH_AT_OBSERVED_PREFIXES"));
    }

    #[test]
    fn mpd_screen_and_sync_are_recorded_not_presented_frame_claims() {
        let report = mpd_replay(&[
            b"\x1b[?2026hPAINTFIRST",
            b"\x1b[?2026l\x1b[?1049h",
            b"\x1b[?1049l",
        ]);
        let samples = report["chunk_pass"]["samples"].as_array().unwrap();
        assert_eq!(samples[0]["synchronized_output"], true);
        assert_eq!(samples[0]["screen"], "PRIMARY");
        assert_eq!(samples[0]["cell_match"], true);
        assert_eq!(samples[1]["synchronized_output"], false);
        assert_eq!(samples[1]["screen"], "ALTERNATE");
        assert_eq!(samples[1]["cell_match"], false);
        assert_eq!(samples[2]["screen"], "PRIMARY");
        assert_eq!(samples[2]["cell_match"], true);
        assert!(mpd_has(&report, "LATER_CELL_ABSENCE"));
    }

    #[test]
    fn mpd_raw_absence_does_not_preclude_cursor_assembled_cells() {
        let report = mpd_replay(&[b"PAINT\x1b[6GFIRST"]);
        assert!(mpd_has(&report, "RAW_LITERAL_ABSENT"));
        assert!(mpd_has(&report, "CELL_MATCH_OBSERVED"));
        assert_eq!(report["chunk_pass"]["samples"][0]["cell_match"], true);
    }

    #[test]
    fn mpd_geometry_and_formatter_soft_wrap_are_bound() {
        let mut raw = vec![b'x'; 100];
        raw.extend_from_slice(b"PAINTFIRST");
        let (raw, manifest) = mpd_parts(&[&raw]);
        let result = replay_paint_capture(&mpd_validated(raw, manifest), ReplayBudget::default());
        assert!(result.is_ok());
        let report = result.unwrap();
        assert_eq!(
            report["geometry"],
            serde_json::json!({"cols": 106, "rows": 34})
        );
        assert_eq!(report["chunk_pass"]["samples"][0]["cell_match"], true);
        assert!(mpd_has(&report, "CELL_MATCH_OBSERVED"));
    }

    #[test]
    fn mpd_watermark_is_a_cleaned_prefix_offset_not_a_replay_offset() {
        let (raw, mut manifest) = mpd_parts(&[b"PAINTFIRST", b"\x1b[2J\x1b[H"]);
        manifest["watermark"] = serde_json::json!(10);
        let report =
            replay_paint_capture(&mpd_validated(raw, manifest), ReplayBudget::default()).unwrap();
        assert_eq!(report["chunk_pass"]["samples"][0]["end_offset"], 10);
        assert_eq!(report["chunk_pass"]["samples"][0]["cell_match"], true);
        assert_eq!(
            report["chunk_pass"]["samples"][0]["regex_literal_match"],
            true
        );
        assert_eq!(
            report["chunk_pass"]["samples"][0]["regex_predicate_match"],
            false
        );
        assert_eq!(report["byte_pass"]["first_cell_match"], 10);
    }

    #[test]
    fn mpd_capture_and_replay_incompleteness_preserve_positive_prefix_facts() {
        let defaults = ReplayBudget::default();
        assert_eq!(
            (defaults.regex_prefix_bytes, defaults.byte_samples),
            (64 * 1024 * 1024, 65536)
        );
        {
            let (raw, mut manifest) = mpd_parts(&[b"PAINTFIRST"]);
            manifest["reader_state"] = serde_json::json!("READ_ERROR");
            manifest["reader_error"] = serde_json::json!({"kind": "Other", "raw_os_error": 5});
            let report =
                replay_paint_capture(&mpd_validated(raw, manifest), ReplayBudget::default())
                    .unwrap();
            assert!(mpd_has(&report, "INDETERMINATE"));
            assert!(report["indeterminate_reasons"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("READ_ERROR")));
            assert!(mpd_has(&report, "CELL_MATCH_OBSERVED"));
        }
        let mut cap_chunks = vec![vec![b'x'; 8192]; 257];
        cap_chunks[0][..10].copy_from_slice(b"PAINTFIRST");
        let refs: Vec<&[u8]> = cap_chunks.iter().map(Vec::as_slice).collect();
        let (raw, mut manifest) = mpd_parts(&refs);
        manifest["reader_state"] = serde_json::json!("CAP_STOP");
        let report = replay_paint_capture(
            &mpd_validated(raw, manifest),
            ReplayBudget {
                regex_prefix_bytes: 8192,
                byte_samples: 10,
            },
        )
        .unwrap();
        assert!(mpd_has(&report, "INDETERMINATE"));
        assert!(report["indeterminate_reasons"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("CAP_STOP")));
        assert!(mpd_has(&report, "CELL_MATCH_OBSERVED"));
        let (raw, mut manifest) = mpd_parts(&[b"PAINTFIRST", b"later"]);
        manifest["metadata_complete"] = serde_json::json!(false);
        manifest["chunk_ends"] = serde_json::json!([10]);
        let report =
            replay_paint_capture(&mpd_validated(raw, manifest), ReplayBudget::default()).unwrap();
        assert!(mpd_has(&report, "INDETERMINATE"));
        assert_eq!(report["chunk_pass"]["complete"], false);
        assert!(mpd_has(&report, "CELL_MATCH_OBSERVED"));
        let (raw, manifest) = mpd_parts(&[b"PAINTFIRST", b"later"]);
        let capture = mpd_validated(raw, manifest);
        let byte_limited = replay_paint_capture(
            &capture,
            ReplayBudget {
                regex_prefix_bytes: 64 * 1024 * 1024,
                byte_samples: 10,
            },
        )
        .unwrap();
        assert_eq!(byte_limited["byte_pass"]["complete"], false);
        assert_eq!(byte_limited["byte_pass"]["submitted_bytes"], 10);
        assert_eq!(
            byte_limited["byte_pass"]["submitted_sha256"],
            paint_digest(b"PAINTFIRST")
        );
        assert!(mpd_has(&byte_limited, "INDETERMINATE"));
        assert!(mpd_has(&byte_limited, "CELL_MATCH_OBSERVED"));
        let regex_limited = replay_paint_capture(
            &capture,
            ReplayBudget {
                regex_prefix_bytes: 10,
                byte_samples: 65536,
            },
        )
        .unwrap();
        assert_eq!(regex_limited["chunk_pass"]["complete"], false);
        assert_eq!(regex_limited["chunk_pass"]["regex_prefix_bytes"], 10);
        assert_eq!(regex_limited["chunk_pass"]["submitted_bytes"], 10);
        assert_eq!(
            regex_limited["chunk_pass"]["samples"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(mpd_has(&regex_limited, "INDETERMINATE"));
        assert!(mpd_has(&regex_limited, "CELL_MATCH_OBSERVED"));
    }

    struct MpdReplayDirectory(std::path::PathBuf);

    impl MpdReplayDirectory {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt;
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let base = std::env::var_os("ZYNK_TEST_ROOT")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            std::fs::create_dir_all(&base).unwrap();
            let root = base.join(format!("paint-replay-{}-{id}", std::process::id()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            Self(root)
        }

        fn write(&self, name: &str, bytes: &[u8]) {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(self.0.join(name))
                .unwrap();
            file.write_all(bytes).unwrap();
        }
    }

    impl Drop for MpdReplayDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mpd_replay_disk_reader_binds_all_three_files_and_external_digest() {
        let fixture = MpdReplayDirectory::new();
        let (raw, manifest) = mpd_parts(&[b"PAINTFIRST"]);
        let digest = paint_digest(&raw);
        let (metadata, complete) = mpd_encoded(&manifest);
        fixture.write("raw.bin", &raw);
        fixture.write("manifest.json", &metadata);
        fixture.write("complete.json", &complete);
        let request = serde_json::to_vec(&serde_json::json!({
            "artifact_directory": fixture.0,
            "evaluated_sha256": digest
        }))
        .unwrap();
        fixture.write("request.json", &request);
        fixture.write("empty.json", b"");
        fixture.write("malformed.json", b"{");
        assert!(replay_request(Some(&fixture.0.join("empty.json"))).is_err());
        assert!(replay_request(Some(&fixture.0.join("malformed.json"))).is_err());
        assert!(replay_request(Some(&fixture.0.join("absent.json"))).is_err());
        let report = replay_request(Some(&fixture.0.join("request.json")))
            .unwrap()
            .unwrap();
        assert_eq!(report["requested"], true);
        assert_eq!(report["evaluated_sha256"], digest);
        assert!(mpd_has(&report, "CELL_MATCH_OBSERVED"));
        assert!(read_paint_capture(&fixture.0, &"0".repeat(64)).is_err());
        std::fs::remove_file(fixture.0.join("complete.json")).unwrap();
        assert!(read_paint_capture(&fixture.0, &digest).is_err());
        assert!(replay_request(Some(&fixture.0.join("request.json"))).is_err());
    }

    #[test]
    fn mpd_replay_submits_every_chunk_in_bound_order() {
        let report = mpd_replay(&[b"PAINT", b"FIRST"]);
        assert_eq!(report["chunk_pass"]["complete"], true);
        assert_eq!(report["chunk_pass"]["submitted_bytes"], 10);
        assert_eq!(
            report["chunk_pass"]["submitted_sha256"],
            paint_digest(b"PAINTFIRST")
        );
        assert_eq!(report["chunk_pass"]["samples"][0]["end_offset"], 5);
        assert_eq!(report["chunk_pass"]["samples"][1]["end_offset"], 10);
        assert_eq!(report["chunk_pass"]["samples"][1]["cell_match"], true);
        assert_eq!(
            report["byte_pass"]["submitted_sha256"],
            paint_digest(b"PAINTFIRST")
        );
        assert!(!mpd_has(&report, "INDETERMINATE"));
    }

    #[test]
    fn mpd_validation_rejects_dropped_reordered_or_rebound_bytes() {
        let (raw, manifest) = mpd_parts(&[b"AAAAA", b"BBBBB"]);
        let expected = manifest["evaluated_sha256"].as_str().unwrap();
        let (metadata, complete) = mpd_encoded(&manifest);
        assert!(validate_paint_capture(raw, &metadata, &complete, expected).is_ok());
        assert!(validate_paint_capture(b"AAAAA".to_vec(), &metadata, &complete, expected).is_err());
        assert!(
            validate_paint_capture(b"BBBBBAAAAA".to_vec(), &metadata, &complete, expected).is_err()
        );
        assert!(validate_paint_capture(
            b"AAAAABBBBB".to_vec(),
            &metadata,
            &complete,
            &"0".repeat(64)
        )
        .is_err());
        let mut altered = metadata.clone();
        altered.push(b' ');
        assert!(
            validate_paint_capture(b"AAAAABBBBB".to_vec(), &altered, &complete, expected).is_err()
        );
    }

    #[test]
    fn mpd_validation_refuses_geometry_schema_lengths_and_chunk_corruption() {
        let (raw, manifest) = mpd_parts(&[b"first", b"second"]);
        for (key, value) in [
            ("cols", serde_json::json!(105)),
            ("rows", serde_json::json!(33)),
            ("replay_scrollback_bytes", serde_json::json!(10_000_000)),
            ("version", serde_json::json!(2)),
            ("raw_length", serde_json::json!(10)),
            ("chunk_ends", serde_json::json!([5, 5, 11])),
            ("chunk_ends", serde_json::json!([11, 5])),
            ("chunk_ends", serde_json::json!([5])),
            ("reader_state", serde_json::json!("UNKNOWN")),
        ] {
            let mut bad = manifest.clone();
            bad[key] = value;
            let (metadata, complete) = mpd_encoded(&bad);
            assert!(
                validate_paint_capture(
                    raw.clone(),
                    &metadata,
                    &complete,
                    manifest["evaluated_sha256"].as_str().unwrap()
                )
                .is_err(),
                "{key}"
            );
        }
    }

    #[test]
    fn mpd_external_replay_driver_always_runs_a_synthetic_control() {
        let report = mpd_replay(&[b"PAINTFIRST"]);
        assert!(mpd_has(&report, "CELL_MATCH_OBSERVED"));
        assert!(replay_request(None).unwrap().is_none());
        assert!(replay_request(Some(std::path::Path::new("relative-input"))).is_err());
        let requested = std::env::var_os("ZYNK_TEST_PAINT_REPLAY");
        let external = replay_request(requested.as_deref().map(std::path::Path::new)).unwrap();
        assert_eq!(external.is_some(), requested.is_some());
        if let Some(report) = external {
            assert_eq!(report["requested"], true);
            assert_eq!(report["evaluated_sha256"].as_str().unwrap().len(), 64);
            println!("MPD_REPLAY {}", serde_json::to_string(&report).unwrap());
        } else {
            println!("MPD_REPLAY {{\"requested\":false,\"status\":\"NOT_REQUESTED\"}}");
        }
    }
}
