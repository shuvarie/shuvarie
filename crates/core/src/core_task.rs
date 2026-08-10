use std::collections::HashMap;

use tokio::sync::mpsc::{Receiver, Sender};

use shuvarie_llm::ProviderClient;

use crate::command::Command;
use crate::config::{Config, ProviderConfig};
use crate::event::Event;

pub async fn run(mut config: Config, mut cmd_rx: Receiver<Command>, event_tx: Sender<Event>) {
    let mut clients: HashMap<String, ProviderClient> = HashMap::new();

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::Ping => {
                let _ = event_tx.send(Event::Pong).await;
            }
            Command::ListModels { provider_name } => {
                let client = match client_for(&mut clients, &mut config, &provider_name) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = event_tx
                            .send(Event::ModelsError {
                                provider_name,
                                error: e,
                            })
                            .await;
                        continue;
                    }
                };
                match client.list_models().await {
                    Ok(models) => {
                        let _ = event_tx
                            .send(Event::ModelsLoaded {
                                provider_name,
                                models,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = event_tx
                            .send(Event::ModelsError {
                                provider_name,
                                error: e.to_string(),
                            })
                            .await;
                    }
                }
            }
            Command::AddProvider { name, config: pc } => {
                config.providers.insert(name.clone(), pc);
                clients.remove(&name);
                persist(&config, &event_tx).await;
            }
            Command::RemoveProvider { name } => {
                config.providers.remove(&name);
                clients.remove(&name);
                if config.active_provider.as_deref() == Some(name.as_str()) {
                    config.active_provider = None;
                    config.active_model = None;
                }
                persist(&config, &event_tx).await;
            }
            Command::SetActiveProvider { name } => {
                if config.providers.contains_key(&name) {
                    config.active_provider = Some(name.clone());
                    if !clients.contains_key(&name)
                        && let Some(pc) = config.providers.get(&name)
                        && let Ok(client) = build_client(pc)
                    {
                        clients.insert(name.clone(), client);
                    }
                    persist(&config, &event_tx).await;
                }
            }
            Command::SetActiveModel { model } => {
                config.active_model = Some(model);
                persist(&config, &event_tx).await;
            }
            Command::SaveConfig => {
                persist(&config, &event_tx).await;
            }
        }
    }
}

fn client_for<'a>(
    clients: &'a mut HashMap<String, ProviderClient>,
    config: &'a mut Config,
    name: &str,
) -> Result<&'a ProviderClient, String> {
    if !clients.contains_key(name) {
        let pc = config
            .providers
            .get(name)
            .ok_or_else(|| format!("provider '{name}' not found"))?;
        let client = build_client(pc)?;
        clients.insert(name.to_string(), client);
    }
    Ok(clients.get(name).unwrap())
}

fn build_client(pc: &ProviderConfig) -> Result<ProviderClient, String> {
    ProviderClient::build(pc.kind, pc.api_key.as_deref(), pc.base_url.as_deref())
        .map_err(|e| e.to_string())
}

async fn persist(config: &Config, event_tx: &Sender<Event>) {
    match config.save() {
        Ok(()) => {
            let _ = event_tx.send(Event::ConfigSaved).await;
        }
        Err(e) => {
            let _ = event_tx
                .send(Event::ConfigError {
                    error: e.to_string(),
                })
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::UiPrefs;
    use shuvarie_llm::Provider;

    fn empty_config() -> Config {
        Config {
            providers: HashMap::new(),
            active_provider: None,
            active_model: None,
            ui: UiPrefs::default(),
        }
    }

    #[tokio::test]
    async fn ping_pong() {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let handle = tokio::spawn(run(empty_config(), cmd_rx, event_tx));
        cmd_tx.send(Command::Ping).await.unwrap();
        let ev = event_rx.recv().await.expect("event");
        assert!(matches!(ev, Event::Pong));
        drop(cmd_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn add_provider_emits_saved() {
        // The core task persists to Config::config_path() (user config dir). We only
        // assert the event is emitted here; disk round-trip is covered by config tests.
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let config = empty_config();
        let handle = tokio::spawn(run(config, cmd_rx, event_tx));
        cmd_tx
            .send(Command::AddProvider {
                name: "shuvarie-test-add".into(),
                config: ProviderConfig::new(Provider::Ollama, None, None),
            })
            .await
            .unwrap();

        let mut saw_saved = false;
        for _ in 0..5 {
            match event_rx.recv().await {
                Some(Event::ConfigSaved) => {
                    saw_saved = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_saved, "expected ConfigSaved event");
        drop(cmd_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn remove_provider_clears_active() {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let mut config = empty_config();
        config.providers.insert(
            "p1".into(),
            ProviderConfig::new(Provider::Ollama, None, None),
        );
        config.active_provider = Some("p1".into());

        let handle = tokio::spawn(run(config, cmd_rx, event_tx));
        cmd_tx
            .send(Command::RemoveProvider { name: "p1".into() })
            .await
            .unwrap();

        let mut saved = false;
        for _ in 0..5 {
            if matches!(event_rx.recv().await, Some(Event::ConfigSaved)) {
                saved = true;
                break;
            }
        }
        assert!(saved);
        drop(cmd_tx);
        let _ = handle.await;
    }
}
