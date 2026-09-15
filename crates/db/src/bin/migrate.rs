use std::path::PathBuf;

use toasty_cli::{Config, MigrationConfig, MigrationPrefixStyle, ToastyCli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let migration_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("toasty");
    let config = Config::new().migration(
        MigrationConfig::new()
            .path(migration_dir)
            .prefix_style(MigrationPrefixStyle::Sequential),
    );

    let mut builder = toasty::Db::builder();
    builder.models(toasty::models!(
        shuvarie_db::Session,
        shuvarie_db::Message,
        shuvarie_db::MessageEmbedding,
        shuvarie_db::ToolCall
    ));
    let db = builder
        .build(toasty_driver_turso::Turso::in_memory())
        .await?;

    let cli = ToastyCli::with_config(db, config);
    cli.parse_and_run().await?;
    Ok(())
}
