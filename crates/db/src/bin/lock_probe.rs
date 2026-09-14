use std::io::Write;
use std::time::Duration;

use shuvarie_db::{LockAcquire, Store};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let db_path = args.next().expect("db path");
    let mode = args.next().expect("mode");
    let client = args.next().expect("client id");
    let session: uuid::Uuid = args.next().expect("session id").parse()?;

    let mut store = Store::open(std::path::Path::new(&db_path))
        .await?
        .with_client_id(client);

    match mode.as_str() {
        "acquire" => {
            let outcome = store.acquire_session_lock(session, now_ms()).await?;
            println!("{outcome:?}");
            match outcome {
                LockAcquire::Held => {}
                LockAcquire::Acquired | LockAcquire::Ours => {
                    store.release_session_lock(session).await?;
                }
            }
        }
        "hold" => {
            let ms: u64 = args.next().expect("hold ms").parse()?;
            let outcome = store.acquire_session_lock(session, now_ms()).await?;
            println!("{outcome:?}");
            std::io::stdout().flush()?;
            tokio::time::sleep(Duration::from_millis(ms)).await;
            store.release_session_lock(session).await?;
            println!("released");
        }
        "list" => {
            let in_use = store
                .list_sessions()
                .await?
                .iter()
                .find(|s| s.id == session)
                .map(|s| s.in_use)
                .unwrap_or_default();
            println!("in_use={in_use}");
        }
        other => anyhow::bail!("unknown mode {other}"),
    }
    Ok(())
}
