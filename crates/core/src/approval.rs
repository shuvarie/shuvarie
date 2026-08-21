use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalReason {
    OutsideWorkspace,
    HiddenPath,
}

impl ApprovalReason {
    pub fn label(&self) -> &'static str {
        match self {
            ApprovalReason::OutsideWorkspace => "outside the workspace",
            ApprovalReason::HiddenPath => "hidden path",
        }
    }
}

pub struct ApprovalRequest {
    pub tool: String,
    pub path: String,
    pub reason: ApprovalReason,
    pub respond: oneshot::Sender<bool>,
}

#[derive(Clone)]
pub struct ApprovalGate {
    tx: mpsc::Sender<ApprovalRequest>,
}

impl ApprovalGate {
    pub fn new(tx: mpsc::Sender<ApprovalRequest>) -> Self {
        Self { tx }
    }

    pub async fn request(
        &self,
        tool: &str,
        path: &str,
        reason: ApprovalReason,
    ) -> Result<(), String> {
        let (respond, rx) = oneshot::channel();
        let req = ApprovalRequest {
            tool: tool.to_string(),
            path: path.to_string(),
            reason,
            respond,
        };
        self.tx
            .send(req)
            .await
            .map_err(|_| "approval channel closed".to_string())?;
        match rx.await {
            Ok(true) => Ok(()),
            Ok(false) => Err("denied by user".to_string()),
            Err(_) => Err("approval cancelled".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn gate_approves_and_denies() {
        let (tx, mut rx) = mpsc::channel::<ApprovalRequest>(8);
        let gate = ApprovalGate::new(tx);

        let approve = tokio::spawn({
            let gate = gate.clone();
            async move {
                gate.request("read_file", ".git/config", ApprovalReason::HiddenPath)
                    .await
            }
        });
        let req = rx.recv().await.unwrap();
        assert_eq!(req.tool, "read_file");
        assert_eq!(req.reason, ApprovalReason::HiddenPath);
        req.respond.send(true).unwrap();
        assert!(approve.await.unwrap().is_ok());

        let deny = tokio::spawn({
            let gate = gate.clone();
            async move {
                gate.request("write_file", "../x", ApprovalReason::OutsideWorkspace)
                    .await
            }
        });
        let req = rx.recv().await.unwrap();
        req.respond.send(false).unwrap();
        assert_eq!(deny.await.unwrap().unwrap_err(), "denied by user");
    }
}
