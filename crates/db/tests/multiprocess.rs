use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use shuvarie_db::Store;

fn spawn_probe(
    db: &Path,
    mode: &str,
    client: &str,
    session: uuid::Uuid,
    extra: Option<&str>,
) -> (Child, Receiver<String>) {
    let exe = env!("CARGO_BIN_EXE_lock_probe");
    let mut command = Command::new(exe);
    command
        .arg(db)
        .arg(mode)
        .arg(client)
        .arg(session.to_string());
    if let Some(extra) = extra {
        command.arg(extra);
    }
    let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    (child, rx)
}

fn next_line(rx: &Receiver<String>) -> String {
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(line) => line,
        Err(RecvTimeoutError::Timeout) => panic!("probe produced no line in time"),
        Err(RecvTimeoutError::Disconnected) => panic!("probe exited without printing"),
    }
}

#[tokio::test]
async fn two_processes_coordinate_session_locks_over_multiprocess_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.db");
    let session = {
        let mut store = Store::open(&path).await.unwrap().with_client_id("parent");
        store.create_session("probe", None, None).await.unwrap()
    };

    let (mut holder, holder_rx) = spawn_probe(&path, "hold", "a", session, Some("2000"));
    assert_eq!(next_line(&holder_rx), "Acquired");

    let (_, contender) = spawn_probe(&path, "acquire", "b", session, None);
    assert_eq!(next_line(&contender), "Held");

    let (_, watcher) = spawn_probe(&path, "list", "b", session, None);
    assert_eq!(next_line(&watcher), "in_use=true");

    holder.wait().unwrap();
    drop(holder_rx.recv_timeout(Duration::from_secs(15)));

    let (_, latecomer) = spawn_probe(&path, "acquire", "b", session, None);
    assert_eq!(next_line(&latecomer), "Acquired");
}
