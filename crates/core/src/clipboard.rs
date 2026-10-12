//! Reading an image off the system clipboard, for the composer's
//! paste-from-clipboard keybinding.
//!
//! There is no terminal protocol to lean on: OSC 5522 (the clipboard-image read
//! request) is kitty-only and, like every OSC outside 10–19, invisible to
//! termina's parser. So the clipboard is read through a subprocess — the
//! configured `attachments { clipboard-command "…" }` program, or an
//! auto-detected platform helper (`pngpaste` on macOS, `wl-paste`/`xclip` on
//! Linux, PowerShell on Windows). The reader's stdout must be the raw image; it
//! is sniffed, fitted to the image limits, and staged to a temp file that enters
//! the ordinary pending-attachment pipeline. Over SSH the clipboard lives on the
//! client, so the reader has to be a user-supplied ferry.
//!
//! Everything here is synchronous — callers run it on the blocking pool.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use shuvarie_llm::AttachmentKind;

use crate::attachments::{
    AttachmentSettings, ClipboardAttachment, DirectiveProbe, fit_image, image_media_type,
};

/// How long the clipboard reader may run before it is killed.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The raw clipboard byte cap, before any image processing.
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024 * 1024;
/// Staged temp files older than this are swept on the next fetch.
const STALE_AGE: Duration = Duration::from_secs(60 * 60);
/// The stem every staged clipboard directory shares, so the sweep can find its
/// own droppings and nothing else.
const STAGED_PREFIX: &str = "shuvarie-paste-";
/// The staged image's file name. It becomes the attachment's display name (and
/// the name persisted with the message), so keep it meaningful: a pid-stamped
/// temp name would surface in the composer chip and the transcript.
const STAGED_NAME: &str = "clipboard";

/// Read an image off the system clipboard, stage it to a temp file, and return
/// it as a pending attachment. `Err` carries a user-facing reason (no reader
/// configured or found, an empty or non-image clipboard, an oversized image).
pub(crate) fn fetch_image(settings: &AttachmentSettings) -> Result<ClipboardAttachment, String> {
    sweep_stale(&std::env::temp_dir());
    let reader = reader(settings)?;
    let bytes = run_reader(&reader)?;
    if bytes.is_empty() {
        return Err(format!(
            "the clipboard holds no image ({} returned nothing)",
            reader.program
        ));
    }
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        return Err(format!(
            "the clipboard image is {} — the cap is {}",
            shuvarie_llm::format_size(bytes.len() as u64),
            shuvarie_llm::format_size(MAX_CLIPBOARD_BYTES as u64)
        ));
    }
    let Some(media_type) = image_media_type(&bytes) else {
        return Err(
            "the clipboard does not hold a supported image (png, jpeg, gif, or webp)".to_string(),
        );
    };
    let (bytes, media_type) = fit_image(&bytes, media_type, settings.image_edge)
        .map_err(|error| format!("clipboard image rejected: {error}"))?;
    let path = stage(&bytes, media_type)?;
    Ok(ClipboardAttachment {
        probe: DirectiveProbe {
            kind: Some(AttachmentKind::Image),
            size: Some(bytes.len() as u64),
            error: None,
        },
        path: path.to_string_lossy().into_owned(),
    })
}

/// A clipboard reader program invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reader {
    program: String,
    args: Vec<String>,
}

impl Reader {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
        }
    }
}

/// The reader to run: the configured `clipboard-command` when set, otherwise
/// the first auto-detected platform helper that exists on `PATH`.
fn reader(settings: &AttachmentSettings) -> Result<Reader, String> {
    if let Some(configured) = settings.clipboard_command.as_deref() {
        let mut parts = split_command(configured);
        if parts.is_empty() {
            return Err("`clipboard-command` is empty".to_string());
        }
        let program = parts.remove(0);
        return Ok(Reader {
            program,
            args: parts,
        });
    }
    default_readers()
        .into_iter()
        .find(|candidate| crate::shell::find_in_path(&candidate.program).is_some())
        .ok_or_else(|| {
            "no clipboard reader found: install wl-paste, xclip, or pngpaste, or set \
             `attachments { clipboard-command \"…\" }`"
                .to_string()
        })
}

/// The platform helpers tried when no `clipboard-command` is configured.
fn default_readers() -> Vec<Reader> {
    #[cfg(target_os = "macos")]
    {
        vec![Reader::new("pngpaste", &["-"])]
    }
    #[cfg(windows)]
    {
        // PowerShell writes the PNG bytes to its standard output stream.
        const SCRIPT: &str = "Add-Type -AssemblyName System.Windows.Forms; \
            $img=[Windows.Forms.Clipboard]::GetImage(); \
            if ($img) { $ms=New-Object IO.MemoryStream; \
            $img.Save($ms,[Drawing.Imaging.ImageFormat]::Png); \
            [Console]::OpenStandardOutput().Write($ms.ToArray(),0,$ms.Length) }";
        vec![
            Reader::new("powershell.exe", &["-NoProfile", "-Command", SCRIPT]),
            Reader::new("pwsh.exe", &["-NoProfile", "-Command", SCRIPT]),
        ]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let wl_paste = Reader::new("wl-paste", &["--no-newline", "--type", "image/png"]);
        let xclip = Reader::new(
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        );
        // Wayland first on a Wayland session, then X11; reversed otherwise.
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            vec![wl_paste, xclip]
        } else {
            vec![xclip, wl_paste]
        }
    }
}

/// Split a `clipboard-command` into program and arguments, honouring single and
/// double quotes so a program path with spaces can be quoted.
fn split_command(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for ch in input.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if started {
        parts.push(current);
    }
    parts
}

/// Run the reader and collect its standard output, bounded by [`TIMEOUT`] and
/// [`MAX_CLIPBOARD_BYTES`]. A killed slow reader leaves its thread to finish on
/// the closed pipe.
fn run_reader(reader: &Reader) -> Result<Vec<u8>, String> {
    let mut child = Command::new(&reader.program)
        .args(&reader.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run {:?}: {error}", reader.program))?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut handle = stdout.take(MAX_CLIPBOARD_BYTES as u64 + 1);
        let _ = handle.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(TIMEOUT) {
        Ok(bytes) => {
            let _ = child.wait();
            Ok(bytes)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!(
                "{:?} timed out after {}s reading the clipboard",
                reader.program,
                TIMEOUT.as_secs()
            ))
        }
    }
}

/// Write the image bytes to a unique temp directory as `clipboard.<ext>` and
/// return the file's path. The directory lives until the next fetch's sweep or
/// the OS's own temp cleanup; naming the file itself plainly keeps the
/// attachment's display name readable.
fn stage(bytes: &[u8], media_type: &str) -> Result<PathBuf, String> {
    let ext = match media_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        other => return Err(format!("cannot stage image media type {other}")),
    };
    let dir = std::env::temp_dir().join(format!(
        "{STAGED_PREFIX}{}-{}.{ext}",
        std::process::id(),
        unique_suffix()
    ));
    let path = dir.join(format!("{STAGED_NAME}.{ext}"));
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot stage the clipboard image: {error}"))?;
    std::fs::write(&path, bytes)
        .map_err(|error| format!("cannot stage the clipboard image: {error}"))?;
    Ok(path)
}

/// Remove staged clipboard directories (and any legacy flat files) older than
/// [`STALE_AGE`] from `dir`. Best-effort: any error is ignored, and an entry
/// with an unreadable or future mtime is kept.
fn sweep_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(STAGED_PREFIX) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        // A future mtime (clock skew) counts as fresh, never deleted.
        let stale = meta
            .modified()
            .map(|mtime| mtime.elapsed().map(|age| age >= STALE_AGE).unwrap_or(false))
            .unwrap_or(false);
        if !stale {
            continue;
        }
        if meta.is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        } else {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A process-unique suffix: nanos plus a per-process counter, so two reads in
/// the same instant never collide.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}-{seq}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(command: &str) -> AttachmentSettings {
        AttachmentSettings {
            clipboard_command: Some(command.to_string()),
            ..AttachmentSettings::default()
        }
    }

    fn png_1x1() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([220, 40, 40, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encodes");
        bytes.into_inner()
    }

    #[cfg(unix)]
    fn script(dir: &Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-clipboard.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn split_command_honours_quotes() {
        assert_eq!(
            split_command("wl-paste --type image/png"),
            ["wl-paste", "--type", "image/png"]
        );
        assert_eq!(split_command("  spaced   out  "), ["spaced", "out"]);
        assert_eq!(
            split_command("\"/opt/My Tools/reader\" -o"),
            ["/opt/My Tools/reader", "-o"]
        );
        assert_eq!(split_command("'a b' 'c'"), ["a b", "c"]);
        assert!(split_command("   ").is_empty());
        assert!(split_command("").is_empty());
    }

    #[test]
    fn reader_uses_the_configured_command() {
        let reader = reader(&settings("\"/opt/My Tools/reader\" -o")).unwrap();
        assert_eq!(reader.program, "/opt/My Tools/reader");
        assert_eq!(reader.args, ["-o"]);
    }

    #[test]
    fn reader_rejects_an_empty_configured_command() {
        let error = reader(&settings("   ")).unwrap_err();
        assert!(error.contains("clipboard-command"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn fetch_image_stages_a_png_from_the_reader() {
        let dir = tempfile::tempdir().unwrap();
        let png = png_1x1();
        let fixture = dir.path().join("shot.png");
        std::fs::write(&fixture, &png).unwrap();
        let command = script(dir.path(), &format!("cat '{}'", fixture.display()));
        let attachment = fetch_image(&settings(&command)).expect("a staged image");
        assert_eq!(attachment.probe.kind, Some(AttachmentKind::Image));
        assert!(attachment.probe.error.is_none());
        assert_eq!(
            std::fs::read(&attachment.path).unwrap(),
            png,
            "the staged bytes round-trip"
        );
        assert!(
            Path::new(&attachment.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(STAGED_NAME),
            "staged under our plain name: {}",
            attachment.path
        );
        assert!(
            Path::new(&attachment.path)
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(STAGED_PREFIX),
            "inside one of our staged dirs: {}",
            attachment.path
        );
        // Clean up the whole staged dir.
        if let Some(dir) = Path::new(&attachment.path).parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn fetch_image_reports_an_empty_clipboard() {
        let dir = tempfile::tempdir().unwrap();
        let command = script(dir.path(), "exit 0");
        let error = fetch_image(&settings(&command)).unwrap_err();
        assert!(error.contains("no image"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn fetch_image_reports_a_non_image_clipboard() {
        let dir = tempfile::tempdir().unwrap();
        let command = script(dir.path(), "printf 'just some text'");
        let error = fetch_image(&settings(&command)).unwrap_err();
        assert!(error.contains("supported image"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn fetch_image_reports_a_missing_reader() {
        let error = fetch_image(&settings("/definitely/not/a/reader")).unwrap_err();
        assert!(error.contains("cannot run"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn sweep_removes_only_stale_staged_entries() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join(format!("{STAGED_PREFIX}old"));
        let fresh = dir.path().join(format!("{STAGED_PREFIX}new"));
        let other = dir.path().join("not-ours.png");
        for path in [&stale, &fresh, &other] {
            std::fs::create_dir(path).unwrap();
            std::fs::write(path.join("clipboard.png"), b"x").unwrap();
        }
        // Age `stale` (and the unrelated file, to prove the prefix gate).
        let old = SystemTime::now() - STALE_AGE - Duration::from_secs(60);
        for path in [&stale, &other] {
            // The sweep reads the entry's own mtime, so age the directory.
            std::fs::File::open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        sweep_stale(dir.path());
        assert!(!stale.exists(), "a stale staged dir is removed");
        assert!(fresh.exists(), "a fresh staged dir is kept");
        assert!(other.exists(), "a non-staged entry is never touched");
    }
}
