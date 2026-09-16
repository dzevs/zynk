use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use sha2::{Digest, Sha256};

const CAPTURE_LIMIT: usize = 2 * 1024 * 1024;
const READ_SIZE: usize = 8192;
const METADATA_LIMIT: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReaderStop {
    Reading,
    Eof,
    ReadError {
        kind: String,
        raw_os_error: Option<i32>,
    },
    Cap,
}

#[derive(Clone)]
pub(super) struct PaintCapture {
    pub(super) bytes: Vec<u8>,
    chunk_ends: Vec<usize>,
    metadata_complete: bool,
    stop: ReaderStop,
    chunk_limit: usize,
}

impl PaintCapture {
    pub(super) fn new(chunk_limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            chunk_ends: Vec::new(),
            metadata_complete: true,
            stop: ReaderStop::Reading,
            chunk_limit: chunk_limit.min(METADATA_LIMIT),
        }
    }

    pub(super) fn append(&mut self, bytes: &[u8]) -> bool {
        if self.stop != ReaderStop::Reading {
            return false;
        }
        if !bytes.is_empty() {
            self.bytes.extend_from_slice(bytes);
            if self.chunk_ends.len() < self.chunk_limit {
                self.chunk_ends.push(self.bytes.len());
            } else {
                self.metadata_complete = false;
            }
        }
        if self.bytes.len() > CAPTURE_LIMIT {
            self.finish(ReaderStop::Cap);
            return false;
        }
        true
    }

    pub(super) fn finish(&mut self, stop: ReaderStop) {
        if self.stop == ReaderStop::Reading {
            self.stop = stop;
        }
    }
}

pub(super) struct PaintFailure {
    pub(super) trigger: String,
    pub(super) cols: u16,
    pub(super) rows: u16,
    pub(super) needle: String,
    pub(super) watermark: usize,
    pub(super) clean_length: Option<usize>,
}

struct CaptureReceipt {
    directory: PathBuf,
    raw_sha256: String,
    manifest_sha256: String,
    complete_sha256: String,
}

pub(super) fn paint_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn retain_paint_capture(
    root: Option<&Path>,
    capture: &PaintCapture,
    evaluated_sha256: &str,
    failure: &PaintFailure,
) -> io::Result<Option<CaptureReceipt>> {
    let Some(root) = root else { return Ok(None) };
    if paint_sha256(&capture.bytes) != evaluated_sha256 {
        return Err(invalid("evaluated capture digest differs"));
    }
    if !root.is_absolute() || root.to_str().is_none() {
        return Err(invalid("evidence root must be an absolute UTF-8 path"));
    }
    let metadata = fs::symlink_metadata(root)?;
    // getuid has no pointer arguments or process-state mutation.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o7777 != 0o700
        || fs::canonicalize(root)? != root
    {
        return Err(invalid(
            "evidence root must be canonical, owned and mode 0700",
        ));
    }
    if capture.bytes.len() > CAPTURE_LIMIT + READ_SIZE
        || capture.chunk_ends.len() > METADATA_LIMIT
        || (failure.cols, failure.rows) != (106, 34)
        || !matches!(failure.trigger.as_str(), "SIZE_BOUND" | "PAINT_DEADLINE")
    {
        return Err(invalid("capture exceeds the diagnostic contract"));
    }
    let (reader_state, reader_error) = match &capture.stop {
        ReaderStop::Reading => ("READING", serde_json::Value::Null),
        ReaderStop::Eof => ("EOF", serde_json::Value::Null),
        ReaderStop::Cap => ("CAP_STOP", serde_json::Value::Null),
        ReaderStop::ReadError { kind, raw_os_error } => (
            "READ_ERROR",
            json!({"kind": kind, "raw_os_error": raw_os_error}),
        ),
    };
    let manifest = serde_json::to_vec(&json!({
        "version": 1,
        "evaluated_sha256": evaluated_sha256,
        "raw_length": capture.bytes.len(),
        "cols": failure.cols,
        "rows": failure.rows,
        "replay_scrollback_bytes": 0,
        "needle": failure.needle,
        "watermark": failure.watermark,
        "clean_length": failure.clean_length,
        "trigger": failure.trigger,
        "chunk_ends": capture.chunk_ends,
        "metadata_complete": capture.metadata_complete,
        "reader_state": reader_state,
        "reader_error": reader_error,
    }))?;
    if manifest.len() > 1024 * 1024 {
        return Err(invalid("capture manifest exceeds its limit"));
    }
    let manifest_sha256 = paint_sha256(&manifest);
    let complete = serde_json::to_vec(&json!({
        "raw_sha256": evaluated_sha256,
        "raw_length": capture.bytes.len(),
        "manifest_sha256": manifest_sha256,
    }))?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let directory = root.join(format!("capture-{}-{time}-{serial}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    write_synced(&directory.join("raw.bin"), &capture.bytes)?;
    write_synced(&directory.join("manifest.json"), &manifest)?;
    write_synced(&directory.join("complete.json"), &complete)?;
    File::open(&directory)?.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(Some(CaptureReceipt {
        directory,
        raw_sha256: evaluated_sha256.to_owned(),
        manifest_sha256,
        complete_sha256: paint_sha256(&complete),
    }))
}

fn diagnostic_line(label: &str, value: serde_json::Value) {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "{label} {value}");
}

pub(super) fn report_paint_failure(
    capture: &PaintCapture,
    evaluated_sha256: &str,
    failure: &PaintFailure,
) {
    // Emit this before any synchronous storage operation, which may not return.
    diagnostic_line(
        "MPD_EVALUATED",
        json!({
            "evaluated_sha256": evaluated_sha256,
            "raw_length": capture.bytes.len(),
            "clean_length": failure.clean_length,
            "trigger": failure.trigger,
            "needle": failure.needle,
            "watermark": failure.watermark,
        }),
    );
    let root = std::env::var_os("ZYNK_TEST_PAINT_EVIDENCE_ROOT");
    let outcome = retain_paint_capture(
        root.as_deref().map(Path::new),
        capture,
        evaluated_sha256,
        failure,
    );
    let report = match outcome {
        Ok(Some(receipt)) => json!({
            "status": "RETAINED", "directory": receipt.directory,
            "raw_sha256": receipt.raw_sha256,
            "manifest_sha256": receipt.manifest_sha256,
            "complete_sha256": receipt.complete_sha256,
            "directory_sync_completed": true,
        }),
        Ok(None) => json!({"status": "NOT_REQUESTED"}),
        Err(error) => json!({"status": "RETENTION_FAILED", "error": error.to_string()}),
    };
    diagnostic_line("MPD_RETENTION", report);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mpd_failure() -> PaintFailure {
        PaintFailure {
            trigger: "PAINT_DEADLINE".into(),
            cols: 106,
            rows: 34,
            needle: "PAINTFIRST".into(),
            watermark: 0,
            clean_length: Some(0),
        }
    }

    struct MpdDirectory(std::path::PathBuf);

    impl MpdDirectory {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt;
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = crate::support::test_root()
                .join(format!("paint-control-{}-{id}", std::process::id()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            Self(root)
        }
    }

    impl Drop for MpdDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mpd_capture_records_order_and_reader_stop() {
        let mut capture = PaintCapture::new(4096);
        assert!(capture.append(b"PAINT"));
        assert!(capture.append(b"FIRST"));
        assert_eq!(capture.bytes, b"PAINTFIRST");
        assert_eq!(capture.chunk_ends, vec![5, 10]);
        assert_eq!(capture.stop, ReaderStop::Reading);
        assert!(capture.metadata_complete);
        capture.finish(ReaderStop::Eof);
        assert_eq!(capture.stop, ReaderStop::Eof);
        let mut failed = PaintCapture::new(4096);
        failed.finish(ReaderStop::ReadError {
            kind: "Other".into(),
            raw_os_error: Some(5),
        });
        assert_eq!(
            failed.stop,
            ReaderStop::ReadError {
                kind: "Other".into(),
                raw_os_error: Some(5),
            }
        );
    }

    #[test]
    fn mpd_metadata_limit_keeps_raw_bytes_and_marks_incomplete() {
        let mut capture = PaintCapture::new(1);
        assert!(capture.append(b"first"));
        assert!(capture.append(b"second"));
        assert_eq!(capture.bytes, b"firstsecond");
        assert_eq!(capture.chunk_ends, vec![5]);
        assert!(!capture.metadata_complete);
        assert_eq!(capture.stop, ReaderStop::Reading);
    }

    #[test]
    fn mpd_capture_preserves_append_before_cap_overshoot() {
        let mut capture = PaintCapture::new(4096);
        for _ in 0..256 {
            assert!(capture.append(&[b'x'; 8192]));
        }
        assert_eq!(capture.bytes.len(), 2 * 1024 * 1024);
        assert_eq!(capture.stop, ReaderStop::Reading);
        assert!(!capture.append(&[b'y'; 8192]));
        assert_eq!(capture.bytes.len(), 2 * 1024 * 1024 + 8192);
        assert_eq!(capture.stop, ReaderStop::Cap);
        assert_eq!(capture.chunk_ends.last(), Some(&capture.bytes.len()));
        capture.finish(ReaderStop::Eof);
        assert_eq!(capture.stop, ReaderStop::Cap);
    }

    #[test]
    fn mpd_absent_or_unsafe_root_never_falls_back() {
        use std::os::unix::fs::symlink;
        let fixture = MpdDirectory::new();
        let mut capture = PaintCapture::new(4096);
        assert!(capture.append(b"first"));
        let digest = paint_sha256(&capture.bytes);
        assert!(
            retain_paint_capture(None, &capture, &digest, &mpd_failure())
                .unwrap()
                .is_none()
        );
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 0);
        let alias = fixture.0.join("alias");
        symlink(&fixture.0, &alias).unwrap();
        assert!(retain_paint_capture(Some(&alias), &capture, &digest, &mpd_failure()).is_err());
        assert!(retain_paint_capture(
            Some(std::path::Path::new("relative-root")),
            &capture,
            &digest,
            &mpd_failure()
        )
        .is_err());
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 1);
    }

    #[test]
    fn mpd_retention_refuses_a_different_evaluated_clone() {
        let fixture = MpdDirectory::new();
        let mut capture = PaintCapture::new(4096);
        assert!(capture.append(b"evaluated"));
        let evaluated_digest = paint_sha256(&capture.bytes);
        assert!(capture.append(b"later"));
        assert!(retain_paint_capture(
            Some(&fixture.0),
            &capture,
            &evaluated_digest,
            &mpd_failure()
        )
        .is_err());
        assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 0);
    }

    #[test]
    fn mpd_disk_artifact_is_the_evaluated_snapshot_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = MpdDirectory::new();
        let mut capture = PaintCapture::new(4096);
        assert!(capture.append(b"first"));
        assert!(capture.append(b"second"));
        let evaluated = capture.clone();
        let evaluated_digest = paint_sha256(&evaluated.bytes);
        assert!(capture.append(b"teardown"));
        let mut failure = mpd_failure();
        failure.needle = "rowsecondx".into();
        failure.watermark = 5;
        failure.clean_length = Some(11);
        let receipt =
            retain_paint_capture(Some(&fixture.0), &evaluated, &evaluated_digest, &failure)
                .unwrap()
                .unwrap();
        let raw = std::fs::read(receipt.directory.join("raw.bin")).unwrap();
        assert_eq!(raw, b"firstsecond");
        assert_eq!(paint_sha256(&raw), evaluated_digest);
        assert_eq!(receipt.raw_sha256, evaluated_digest);
        let manifest_bytes = std::fs::read(receipt.directory.join("manifest.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
        assert_eq!(manifest["chunk_ends"], serde_json::json!([5, 11]));
        assert_eq!(
            (manifest["cols"].as_u64(), manifest["rows"].as_u64()),
            (Some(106), Some(34))
        );
        assert_eq!(manifest["evaluated_sha256"], evaluated_digest);
        assert_eq!(manifest["reader_state"], "READING");
        assert_eq!(manifest["replay_scrollback_bytes"], 0);
        assert_eq!(manifest["clean_length"], 11);
        assert_eq!(manifest["needle"], "rowsecondx");
        assert_eq!(manifest["watermark"], 5);
        assert_eq!(manifest["trigger"], "PAINT_DEADLINE");
        let complete: serde_json::Value = serde_json::from_slice(
            &std::fs::read(receipt.directory.join("complete.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(complete["raw_sha256"], evaluated_digest);
        assert_eq!(complete["manifest_sha256"], paint_sha256(&manifest_bytes));
        assert_eq!(complete["raw_length"], raw.len());
        for file in ["raw.bin", "manifest.json", "complete.json"] {
            let mode = std::fs::metadata(receipt.directory.join(file))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(
            std::fs::metadata(&receipt.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn mpd_size_failure_preserves_overshoot_and_unavailable_clean_length() {
        let fixture = MpdDirectory::new();
        let mut capture = PaintCapture::new(4096);
        for _ in 0..257 {
            capture.append(&[b'x'; 8192]);
        }
        let mut failure = mpd_failure();
        failure.trigger = "SIZE_BOUND".into();
        failure.clean_length = None;
        let digest = paint_sha256(&capture.bytes);
        let receipt = retain_paint_capture(Some(&fixture.0), &capture, &digest, &failure)
            .unwrap()
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(receipt.directory.join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["reader_state"], "CAP_STOP");
        assert_eq!(manifest["raw_length"], 2 * 1024 * 1024 + 8192);
        assert_eq!(manifest.get("clean_length"), Some(&serde_json::Value::Null));
        assert_eq!(manifest["trigger"], "SIZE_BOUND");
        assert_eq!(
            paint_sha256(&std::fs::read(receipt.directory.join("raw.bin")).unwrap()),
            digest
        );
    }

    #[test]
    fn mpd_failure_retention_survives_cleanup_and_keeps_panic() {
        const CHILD: &str = "ZYNK_TEST_PAINT_WITNESS";
        if std::env::var_os(CHILD).is_some() {
            struct Cleanup(std::path::PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    crate::support::cleanup_test_base(&self.0);
                }
            }
            let base = MpdDirectory::new();
            let path = base.0.clone();
            std::mem::forget(base);
            let _cleanup = Cleanup(path.clone());
            eprintln!("MPD_FIXTURE {}", serde_json::to_string(&path).unwrap());
            let mut captured = PaintCapture::new(4096);
            assert!(captured.append(b"\x1b]2;PAINTFIRST\x07"));
            let evaluated = captured.clone();
            let digest = paint_sha256(&evaluated.bytes);
            assert!(captured.append(b"later-not-evaluated"));
            report_paint_failure(&evaluated, &digest, &mpd_failure());
            let bytes = &evaluated.bytes;
            let text = String::new();
            let needle = "PAINTFIRST";
            let watermark = 0;
            panic!(
                "configured paint {needle:?} absent after OSC/CSI removal; raw bytes={}, clean bytes={}, watermark={watermark}",
                bytes.len(),
                text.len()
            );
        }
        for mode in ["retained", "retention-error"] {
            let evidence = MpdDirectory::new();
            let root = if mode == "retained" {
                evidence.0.clone()
            } else {
                evidence.0.join("absent")
            };
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "paint_capture::tests::mpd_failure_retention_survives_cleanup_and_keeps_panic",
                    "--nocapture",
                ])
                .env(CHILD, mode)
                .env("ZYNK_TEST_PAINT_EVIDENCE_ROOT", &root)
                .output()
                .unwrap();
            assert!(!output.status.success());
            let output = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let expected = format!(
                "configured paint \"PAINTFIRST\" absent after OSC/CSI removal; raw bytes={}, clean bytes=0, watermark=0",
                b"\x1b]2;PAINTFIRST\x07".len()
            );
            assert!(output.lines().any(|line| line == expected));
            let evaluated: serde_json::Value = serde_json::from_str(
                output
                    .lines()
                    .find_map(|line| line.strip_prefix("MPD_EVALUATED "))
                    .unwrap(),
            )
            .unwrap();
            let removed: std::path::PathBuf = serde_json::from_str(
                output
                    .lines()
                    .find_map(|line| line.strip_prefix("MPD_FIXTURE "))
                    .unwrap(),
            )
            .unwrap();
            assert!(!removed.exists());
            let retention: serde_json::Value = serde_json::from_str(
                output
                    .lines()
                    .find_map(|line| line.strip_prefix("MPD_RETENTION "))
                    .unwrap(),
            )
            .unwrap();
            let children: Vec<_> = std::fs::read_dir(&evidence.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            if mode == "retention-error" {
                assert_eq!(retention["status"], "RETENTION_FAILED");
                assert!(children.is_empty());
                continue;
            }
            assert_eq!(retention["status"], "RETAINED");
            assert_eq!(children.len(), 1);
            let raw = std::fs::read(children[0].join("raw.bin")).unwrap();
            assert_eq!(raw, b"\x1b]2;PAINTFIRST\x07");
            assert_eq!(evaluated["evaluated_sha256"], paint_sha256(&raw));
            assert_eq!(evaluated["raw_length"], raw.len());
            assert_eq!(evaluated["clean_length"], 0);
            assert!(children[0].join("complete.json").is_file());
        }
    }
}
