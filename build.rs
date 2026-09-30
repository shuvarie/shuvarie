use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=.git/HEAD");
    println!("cargo::rerun-if-changed=.git/refs");
    println!("cargo::rerun-if-changed=.git/packed-refs");

    if std::env::var("PROFILE").as_deref() != Ok("debug") {
        return;
    }

    if let Some(hash) = commit_hash() {
        println!("cargo::rustc-env=GIT_COMMIT_SHORT_HASH={hash}");
    }
}

fn commit_hash() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let hash = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if hash.is_empty() {
        return None;
    }
    Some(hash)
}
