use shuvarie_core::{Command, Connections};
use tokio::sync::mpsc::Sender;

pub struct UpdateCtx {
    pub connections: Connections,
    pub cmd_tx: Sender<Command>,
}

impl UpdateCtx {
    pub fn new(connections: Connections, cmd_tx: Sender<Command>) -> Self {
        Self {
            connections,
            cmd_tx,
        }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.cmd_tx.try_send(cmd);
    }
}
