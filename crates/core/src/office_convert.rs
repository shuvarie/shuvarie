//! The external converter for the legacy Office formats (`.doc`, `.ppt`):
//! an optional config-selected program (`attachments { office-converter … }`)
//! runs the LibreOffice/soffice-style CLI
//! (`--headless --convert-to <docx|pptx> --outdir <dir> <file>`) on a staged
//! copy inside a scratch dir, and the converted container comes back for the
//! regular [`shuvarie_doc`] pipeline. Everything here is synchronous — callers
//! run it on the blocking pool.

use shuvarie_doc::SourceFormat;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the external converter may run before it is killed.
const TIMEOUT: Duration = Duration::from_secs(90);

/// The converted container's byte cap, mirroring the attachment ingest cap.
const MAX_CONVERTED_BYTES: u64 = 64 * 1024 * 1024;

/// Converts a legacy `.doc`/`.ppt` container with the external `program`,
/// returning the converted `.docx`/`.pptx` bytes.
pub(crate) fn convert_container(
    program: &str,
    format: SourceFormat,
    bytes: &[u8],
) -> Result<Vec<u8>, String> {
    convert_container_with(program, format, bytes, TIMEOUT)
}

fn convert_container_with(
    program: &str,
    format: SourceFormat,
    bytes: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let (source_ext, target_ext) = match format {
        SourceFormat::LegacyDoc => ("doc", "docx"),
        SourceFormat::LegacyPpt => ("ppt", "pptx"),
        _ => {
            return Err(
                "only legacy .doc/.ppt files go through the external converter".to_string(),
            );
        }
    };

    let dir = tempfile::tempdir().map_err(|error| format!("cannot stage the document: {error}"))?;
    let source = dir.path().join(format!("document.{source_ext}"));
    std::fs::write(&source, bytes)
        .map_err(|error| format!("cannot stage the document: {error}"))?;
    let outdir = dir.path().join("out");
    std::fs::create_dir(&outdir).map_err(|error| format!("cannot stage the document: {error}"))?;

    // Headless run with an isolated user profile dir: a converter CLI shares
    // its profile with a running interactive instance, so an isolated one
    // keeps the conversion out of the user's session. The path turns into a
    // `file://` URL with ` ` escaped and `\` folded forward (Windows drive
    // paths included).
    let profile = dir.path().join("profile");
    let profile = profile
        .to_string_lossy()
        .replace('\\', "/")
        .replace(' ', "%20");
    let profile_url = if profile.starts_with('/') {
        format!("file://{profile}")
    } else {
        format!("file:///{profile}")
    };
    let out_path = outdir.to_string_lossy().into_owned();
    let source_path = source.to_string_lossy().into_owned();

    // stdout/stderr go to files inside the scratch dir: a killed child cannot
    // wedge on a full pipe, and the stderr tail feeds the error message.
    let stdout_log = std::fs::File::create(dir.path().join("out.log"))
        .map_err(|error| format!("cannot stage the document: {error}"))?;
    let stderr_log = std::fs::File::create(dir.path().join("err.log"))
        .map_err(|error| format!("cannot stage the document: {error}"))?;
    let stdout_log_path = dir.path().join("err.log");
    let mut child = Command::new(program)
        .arg(format!("-env:UserInstallation={profile_url}"))
        .args(["--headless", "--convert-to", target_ext])
        .arg("--outdir")
        .arg(&out_path)
        .arg(&source_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log))
        .spawn()
        .map_err(|error| format!("cannot run {program:?}: {error} (is it installed?)"))?;

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child
            .try_wait()
            .map_err(|error| format!("{program:?}: {error}"))?
        {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{program:?} timed out after {}s",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    if !status.success() {
        return Err(format!(
            "{program:?} exited with {status}: {}",
            log_tail(&stdout_log_path)
        ));
    }
    let converted = std::fs::read(outdir.join(format!("document.{target_ext}"))).map_err(|_| {
        format!(
            "{program:?} completed but produced no {target_ext} — {}",
            log_tail(&stdout_log_path)
        )
    })?;
    if converted.len() as u64 > MAX_CONVERTED_BYTES {
        return Err(format!(
            "the converted document is {} — the ingest cap is {}",
            shuvarie_llm::format_size(converted.len() as u64),
            shuvarie_llm::format_size(MAX_CONVERTED_BYTES)
        ));
    }
    Ok(converted)
}

/// The last ~200 chars of a converter log, newlines folded, for error
/// messages. The converter's own chatter is usually the only clue.
fn log_tail(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .map(|log| {
            let log = log.trim();
            if log.len() <= 200 {
                log.to_string()
            } else {
                let mut start = log.len() - 200;
                while !log.is_char_boundary(start) {
                    start += 1;
                }
                format!("…{}", &log[start..])
            }
        })
        .unwrap_or_default()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Writes an executable shell script; returns its path (the converter
    /// `program`).
    fn script(dir: &std::path::Path, body: &str) -> String {
        let script = dir.join("fake-convert.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().into_owned()
    }

    /// A fake `${program} --headless --convert-to docx --outdir OUT IN`
    /// invocation: our converter passes the outdir as the 6th argument; the
    /// fake copies a real docx fixture there as `document.docx`.
    fn fake_converter(dir: &std::path::Path) -> String {
        let fixture = dir.join("fixture.docx");
        std::fs::write(&fixture, include_bytes!("../tests/fixtures/report.docx")).unwrap();
        let fixture = fixture.to_string_lossy().into_owned();
        script(dir, &format!("cp '{fixture}' \"$6/document.docx\""))
    }

    #[test]
    fn legacy_doc_converts_through_a_program() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_converter(dir.path());
        let converted = convert_container_with(
            &program,
            SourceFormat::LegacyDoc,
            b"raw legacy bytes",
            TIMEOUT,
        )
        .expect("conversion succeeds");
        assert_eq!(
            &converted,
            include_bytes!("../tests/fixtures/report.docx").as_slice()
        );
    }

    #[test]
    fn failing_program_reports_the_stderr_tail() {
        let dir = tempfile::tempdir().unwrap();
        let program = script(
            dir.path(),
            "echo 'Error: source file is invalid' >&2\nexit 1\n",
        );
        let error = convert_container_with(&program, SourceFormat::LegacyDoc, b"bytes", TIMEOUT)
            .expect_err("conversion fails");
        assert!(error.contains("source file is invalid"), "error: {error}");
    }

    #[test]
    fn a_missing_output_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let program = script(dir.path(), "exit 0");
        let error = convert_container_with(&program, SourceFormat::LegacyDoc, b"bytes", TIMEOUT)
            .expect_err("no output");
        assert!(error.contains("produced no docx"), "error: {error}");
    }

    #[test]
    fn a_slow_program_hits_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let program = script(dir.path(), "sleep 10");
        let error = convert_container_with(
            &program,
            SourceFormat::LegacyDoc,
            b"bytes",
            Duration::from_millis(300),
        )
        .expect_err("times out");
        assert!(error.contains("timed out after 0s"), "error: {error}");
    }

    #[test]
    fn non_legacy_formats_are_rejected() {
        let error =
            convert_container("soffice", SourceFormat::Csv, b"a,b\n").expect_err("rejected");
        assert!(error.contains("legacy .doc/.ppt"), "error: {error}");
    }
}
