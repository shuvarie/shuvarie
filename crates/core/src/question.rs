use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuestionPrompt {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default = "default_true")]
    pub custom: bool,
}

fn default_true() -> bool {
    true
}

/// One per-question answer: the selected option labels (multiple when the
/// question allows multi-select), or a single custom label typed by the user.
pub type Answer = Vec<String>;
/// All answers, in question order.
pub type Answers = Vec<Answer>;

/// `None` when the user dismissed the question.
pub type AnswerResponse = Option<Answers>;

pub struct QuestionRequest {
    pub questions: Vec<QuestionPrompt>,
    pub respond: oneshot::Sender<AnswerResponse>,
}

#[derive(Clone)]
pub struct QuestionGate {
    tx: mpsc::Sender<QuestionRequest>,
}

impl QuestionGate {
    pub fn new(tx: mpsc::Sender<QuestionRequest>) -> Self {
        Self { tx }
    }

    /// Blocks until the user answers or dismisses. `Err` on dismiss, channel
    /// closure, or an invalid payload.
    pub async fn ask(&self, questions: Vec<QuestionPrompt>) -> Result<Answers, String> {
        if questions.is_empty() {
            return Err("no questions provided".to_string());
        }
        if questions.iter().any(|q| q.options.is_empty() && !q.custom) {
            return Err("each question needs at least one option or a custom answer".to_string());
        }
        let (respond, rx) = oneshot::channel();
        let req = QuestionRequest { questions, respond };
        self.tx
            .send(req)
            .await
            .map_err(|_| "question channel closed".to_string())?;
        match rx.await {
            Ok(Some(answers)) => Ok(answers),
            Ok(None) | Err(_) => Err("The user dismissed this question".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(label: &str) -> QuestionPrompt {
        QuestionPrompt {
            question: "Which layout?".into(),
            header: "Layout".into(),
            options: vec![QuestionOption {
                label: label.into(),
                description: "desc".into(),
            }],
            multiple: false,
            custom: false,
        }
    }

    #[tokio::test]
    async fn gate_returns_answers() {
        let (tx, mut rx) = mpsc::channel::<QuestionRequest>(8);
        let gate = QuestionGate::new(tx);

        let ask = tokio::spawn({
            let gate = gate.clone();
            async move { gate.ask(vec![prompt("sidebar")]).await }
        });
        let req = rx.recv().await.unwrap();
        assert_eq!(req.questions.len(), 1);
        assert_eq!(req.questions[0].options[0].label, "sidebar");
        req.respond
            .send(Some(vec![vec!["sidebar".into()]]))
            .unwrap();
        let answers = ask.await.unwrap().unwrap();
        assert_eq!(answers, vec![vec!["sidebar".to_string()]]);
    }

    #[tokio::test]
    async fn gate_dismiss_is_error() {
        let (tx, mut rx) = mpsc::channel::<QuestionRequest>(8);
        let gate = QuestionGate::new(tx);

        let ask = tokio::spawn({
            let gate = gate.clone();
            async move { gate.ask(vec![prompt("a")]).await }
        });
        let req = rx.recv().await.unwrap();
        req.respond.send(None).unwrap();
        let err = ask.await.unwrap().unwrap_err();
        assert_eq!(err, "The user dismissed this question");
    }

    #[tokio::test]
    async fn gate_rejects_empty_questions() {
        let (tx, _rx) = mpsc::channel::<QuestionRequest>(8);
        let gate = QuestionGate::new(tx);
        assert!(gate.ask(Vec::new()).await.is_err());
    }
}
