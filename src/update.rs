//! Self-update: download the latest release from GitHub and replace this binary.

use color_eyre::eyre::{Context, OptionExt, bail, eyre};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::env::consts::{ARCH, OS};
use std::io::{Read, Write};
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

const RELEASES_API: &str = "https://api.github.com/repos/shuvarie/shuvarie/releases/latest";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Updates the binary to the latest release, printing progress.
pub async fn run() -> color_eyre::Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("shuvarie/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let release = fetch_latest_release(&client).await?;
    let Some(release) = release else {
        println!("No published release found; nothing to update.");
        return Ok(());
    };

    // Pre-releases are excluded: the `latest` endpoint already skips them, and
    // a release flagged (or tagged) as a pre-release is ignored as well.
    let latest = parse_tag(&release.tag_name)?;
    if release.prerelease || !latest.pre.is_empty() {
        println!("Latest release {latest} is a pre-release; skipping update.");
        return Ok(());
    }

    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    if latest <= current {
        println!("shuvarie {current} is up to date (latest: {latest}).");
        return Ok(());
    }

    let target = target_triple()?;
    let archive = Archive {
        name: format!("shuvarie-{latest}-{target}.{}", ext()),
        binary: binary_file_name(),
    };
    println!("Updating shuvarie {current} -> {latest}");
    let archive_data = download_asset(&client, &release, &archive.name).await?;
    let checksums_data = download_asset(&client, &release, "SHA256SUMS").await?;
    verify_checksum(&archive_data, &archive.name, &checksums_data)?;

    let dir = std::env::current_exe()
        .wrap_err("failed to locate the running binary")?
        .parent()
        .ok_or_eyre("the running binary has no parent directory")?
        .to_path_buf();
    tokio::task::spawn_blocking(move || install(&dir, &archive, &archive_data))
        .await
        .wrap_err("the update task panicked")?
        .wrap_err("failed to install the new binary")?;

    println!("Updated shuvarie to {latest}; restart shuvarie to use it.");
    Ok(())
}

/// Fetches the latest release. `None` means there is no published release.
///
/// The GitHub `latest` endpoint only resolves published, non-draft,
/// non-prerelease releases.
async fn fetch_latest_release(client: &reqwest::Client) -> color_eyre::Result<Option<Release>> {
    let response = client
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .wrap_err("failed to reach GitHub releases")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = response.error_for_status()?;
    let release: Release = response
        .json()
        .await
        .wrap_err("failed to decode the release metadata")?;
    Ok(Some(release))
}

/// Parses a release tag (`v1.2.3`) into a version.
fn parse_tag(tag: &str) -> color_eyre::Result<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag))
        .wrap_err_with(|| format!("release tag {tag:?} is not a valid semver version"))
}

/// The Rust target triple of the release archive for this platform, decided at
/// compile time to match the release workflow's build matrix. musl builds ask
/// for the musl tarball: a glibc binary will not run on a musl system.
fn target_triple() -> color_eyre::Result<&'static str> {
    match (OS, ARCH, cfg!(target_env = "musl")) {
        ("linux", "x86_64", true) => Ok("x86_64-unknown-linux-musl"),
        ("linux", "x86_64", false) => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64", true) => Ok("aarch64-unknown-linux-musl"),
        ("linux", "aarch64", false) => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64", _) => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64", _) => Ok("aarch64-apple-darwin"),
        ("windows", "x86_64", _) => Ok("x86_64-pc-windows-gnullvm"),
        ("windows", "aarch64", _) => Ok("aarch64-pc-windows-gnullvm"),
        _ => bail!(
            "self-update is unsupported on {OS}-{ARCH}; download a release from https://github.com/shuvarie/shuvarie/releases"
        ),
    }
}

/// Archive extension for this platform.
fn ext() -> &'static str {
    if cfg!(windows) { "zip" } else { "tar.gz" }
}

/// Binary file name inside the release archive.
fn binary_file_name() -> &'static str {
    if cfg!(windows) {
        "shuvarie.exe"
    } else {
        "shuvarie"
    }
}

struct Archive {
    name: String,
    binary: &'static str,
}

async fn download_asset(
    client: &reqwest::Client,
    release: &Release,
    name: &str,
) -> color_eyre::Result<Vec<u8>> {
    let url = release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_eyre(format!("release asset {name:?} is missing"))?
        .browser_download_url
        .clone();
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to download {name}"))?
        .error_for_status()?;
    response
        .bytes()
        .await
        .with_context(|| format!("failed to read {name}"))
        .map(|bytes| bytes.to_vec())
}

/// Verifies `data` against its entry in a `sha256sum`-formatted checksum file.
fn verify_checksum(data: &[u8], name: &str, checksums: &[u8]) -> color_eyre::Result<()> {
    let expected = std::str::from_utf8(checksums)
        .wrap_err("SHA256SUMS is not UTF-8")?
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let file = parts.next()?;
            (file == name).then(|| hash.to_owned())
        })
        .ok_or_eyre(format!("SHA256SUMS has no entry for {name}"))?;
    let actual = hex(&Sha256::digest(data));
    if !expected.eq_ignore_ascii_case(&actual) {
        bail!("{name} checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Extracts the binary from the downloaded archive into memory.
fn extract_binary(archive: &Archive, data: &[u8]) -> color_eyre::Result<Vec<u8>> {
    #[cfg(unix)]
    {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(data));
        for entry in tar.entries().wrap_err("failed to read the tarball")? {
            let mut entry = entry.wrap_err("failed to read the tarball entry")?;
            let path = entry
                .path()?
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            if path == archive.binary {
                let mut buffer = Vec::new();
                entry
                    .read_to_end(&mut buffer)
                    .wrap_err("failed to read the binary from the tarball")?;
                return Ok(buffer);
            }
        }
        bail!("{} is missing from {}", archive.binary, archive.name);
    }
    #[cfg(windows)]
    {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data))
            .wrap_err("failed to read the zip archive")?;
        for index in 0..zip.len() {
            let mut entry = zip
                .by_index(index)
                .wrap_err("failed to read the zip entry")?;
            let name = entry.name().rsplit('/').next().unwrap_or_default();
            if name == archive.binary {
                let mut buffer = Vec::new();
                entry
                    .read_to_end(&mut buffer)
                    .wrap_err("failed to read the binary from the zip archive")?;
                return Ok(buffer);
            }
        }
        bail!("{} is missing from {}", archive.binary, archive.name);
    }
}

/// Extracts the new binary and swaps it in for the running one.
fn install(dir: &Path, archive: &Archive, data: &[u8]) -> color_eyre::Result<()> {
    let binary = extract_binary(archive, data)?;
    let exe = std::env::current_exe().wrap_err("failed to locate the running binary")?;

    // The staging file lives next to the binary so the swap is a rename on
    // the same filesystem.
    let mut staged = tempfile::Builder::new()
        .prefix(".shuvarie-update-")
        .tempfile_in(dir)
        .wrap_err("failed to stage the new binary")?;
    staged
        .write_all(&binary)
        .and_then(|()| staged.flush())
        .wrap_err("failed to stage the new binary")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o755))
            .wrap_err("failed to stage the new binary")?;
    }

    // On Windows a running executable cannot be renamed over, so move it
    // aside first; on Unix the swap is a single atomic rename.
    #[cfg(windows)]
    {
        let old = backup_path(&exe);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old).wrap_err("failed to move the running binary aside")?;
    }

    staged
        .persist(&exe)
        .map_err(|e| eyre!("failed to replace {}: {e}", exe.display()))
        .map(drop)
}

#[cfg(windows)]
fn backup_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tagged_versions() {
        assert_eq!(parse_tag("v0.1.0").unwrap(), Version::new(0, 1, 0));
        assert_eq!(parse_tag("0.2.0").unwrap(), Version::new(0, 2, 0));
        assert_eq!(
            parse_tag("v1.2.3-rc.1").unwrap(),
            Version::parse("1.2.3-rc.1").unwrap()
        );
        assert!(parse_tag("not-a-version").is_err());
    }

    #[test]
    fn checksums_pass_and_fail() {
        let sums = format!(
            "deadbeef  other-file.tar.gz\n{}  archive.tar.gz\n",
            hex(&Sha256::digest(b"payload"))
        );
        verify_checksum(b"payload", "archive.tar.gz", sums.as_bytes()).unwrap();
        let _ = verify_checksum(b"payload", "missing.tar.gz", sums.as_bytes()).unwrap_err();
        let _ = verify_checksum(b"tampered", "archive.tar.gz", sums.as_bytes()).unwrap_err();
        let _ = verify_checksum(b"payload", "archive.tar.gz", b"").unwrap_err();
    }

    #[test]
    fn target_triple_is_a_release_matrix_target() {
        let triple = target_triple().unwrap();
        assert_eq!(triple.split('-').next().unwrap(), ARCH);
        assert_eq!(
            triple.split('-').nth(1).unwrap(),
            match OS {
                "linux" => "unknown",
                "macos" => "apple",
                "windows" => "pc",
                _ => unreachable!(),
            }
        );
    }
}
