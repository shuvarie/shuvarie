use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::question::{QuestionGate, QuestionOption, QuestionPrompt};

pub(crate) struct Question {
    gate: QuestionGate,
}

impl Question {
    pub(crate) fn new(gate: QuestionGate) -> Self {
        Self { gate }
    }
}

impl Tool for Question {
    const NAME: &'static str = "question";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Use this tool when you need to ask the user questions during execution. This allows you to:\n1. Gather user preferences or requirements\n2. Clarify ambiguous instructions\n3. Get decisions on implementation choices as you work\n4. Offer choices to the user about what direction to take.\n\nUsage notes:\n- When `custom` is enabled (default), a \"Type your own answer\" option is added automatically; don't include \"Other\" or catch-all options\n- Answers are returned as arrays of labels; set `multiple: true` to allow selecting more than one\n- If you recommend a specific option, make that the first option in the list and add \"(Recommended)\" at the end of the label".to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "Questions to ask",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "Complete question" },
                            "header": { "type": "string", "description": "Very short label (max 30 chars)" },
                            "options": {
                                "type": "array",
                                "description": "Available choices",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "Display text (1-5 words, concise)" },
                                        "description": { "type": "string", "description": "Explanation of choice" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multiple": { "type": "boolean", "description": "Allow selecting multiple choices" },
                            "custom": { "type": "boolean", "description": "Allow typing a custom answer (default: true)" }
                        },
                        "required": ["question", "header", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let gate = self.gate.clone();
        let result: Result<ToolOutput, String> = async move {
            let raw = args
                .get("questions")
                .and_then(Value::as_array)
                .ok_or("missing 'questions' array argument")?;
            let prompts: Vec<QuestionPrompt> = raw
                .iter()
                .map(|q| {
                    let question = q
                        .get("question")
                        .and_then(Value::as_str)
                        .ok_or("missing string argument 'question'")?
                        .to_string();
                    let header = q
                        .get("header")
                        .and_then(Value::as_str)
                        .ok_or("missing string argument 'header'")?
                        .to_string();
                    let options = q
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .map(|o| QuestionOption {
                                    label: o
                                        .get("label")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    description: o
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let multiple = q.get("multiple").and_then(Value::as_bool).unwrap_or(false);
                    let custom = q.get("custom").and_then(Value::as_bool).unwrap_or(true);
                    Ok::<QuestionPrompt, String>(QuestionPrompt {
                        question,
                        header,
                        options,
                        multiple,
                        custom,
                    })
                })
                .collect::<Result<_, String>>()?;
            let count = prompts.len();
            let answers = gate.ask(prompts.clone()).await?;
            let formatted = prompts
                .iter()
                .zip(&answers)
                .map(|(q, a)| {
                    let joined = if a.is_empty() {
                        "Unanswered".to_string()
                    } else {
                        a.join(", ")
                    };
                    format!("\"{}\"=\"{joined}\"", q.question)
                })
                .collect::<Vec<_>>()
                .join(", ");
            let plural = if count == 1 { "" } else { "s" };
            Ok(ToolOutput::text(format!(
                "User answered {count} question{plural}: {formatted}"
            )))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}
