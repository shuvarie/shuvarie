use shuvarie_core::Config;
use tokio::sync::mpsc::Sender;

use shuvarie_core::Command;

pub struct UpdateCtx {
    pub config: Config,
    pub cmd_tx: Sender<Command>,
}

impl UpdateCtx {
    pub fn new(config: Config, cmd_tx: Sender<Command>) -> Self {
        Self { config, cmd_tx }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.try_send(cmd);
    }
}
