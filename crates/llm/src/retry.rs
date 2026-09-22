use rig_agent::agent::StreamingError;
use rig_core::completion::CompletionError;

/// A transport-level failure worth retrying: the reason is a short,
/// human-readable label for the status row (`Connection reset`,
/// `Connection timed out`, ...).
pub struct ConnectionFailure {
    pub reason: String,
}

/// Label a rig streaming error for the turn-retry status row.
///
/// Transport failures (reqwest timeouts, connect failures, resets, broken
/// pipes, HTTP 408/429/5xx) get a specific label (`Connection reset`, ...).
/// Everything else — provider API errors (auth, bad request, unknown
/// model), JSON/URL/request build errors — still retries the turn, but with
/// the generic fallback label.
///
/// Mid-stream SSE transport failures lose their typed error (rig flattens
/// them to `ProviderError`), so they are recognized by the
/// `"Http client error: "` display prefix of `http_client::Error::Instance`.
pub fn classify_connection_error(err: &StreamingError) -> Option<ConnectionFailure> {
    let completion = match err {
        StreamingError::Completion(e) => e,
        StreamingError::Prompt(e) => match e.as_ref() {
            rig_agent::completion::PromptError::CompletionError(e) => e,
            _ => return None,
        },
    };
    classify_completion_error(completion)
}

fn classify_completion_error(err: &CompletionError) -> Option<ConnectionFailure> {
    match err {
        CompletionError::HttpError(http) => classify_http_error(http),
        CompletionError::ProviderError(message) => {
            if message.starts_with("Http client error: ") {
                Some(ConnectionFailure {
                    reason: "Connection lost".to_string(),
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn classify_http_error(err: &rig_core::http_client::Error) -> Option<ConnectionFailure> {
    use rig_core::http_client::Error as HttpError;

    match err {
        HttpError::InvalidStatusCode(status)
        | HttpError::InvalidStatusCodeWithMessage(status, _)
        | HttpError::InvalidStatusCodeWithDetails { status, .. } => {
            retryable_status(*status).map(|reason| ConnectionFailure { reason })
        }
        HttpError::Instance(inner) => {
            if let Some(reqwest_err) = inner.downcast_ref::<reqwest::Error>() {
                classify_reqwest_error(reqwest_err)
            } else {
                classify_source_chain(inner.as_ref())
            }
        }
        _ => None,
    }
}

fn retryable_status(status: http::StatusCode) -> Option<String> {
    if status == http::StatusCode::REQUEST_TIMEOUT {
        Some(format!("Request timeout ({status})"))
    } else if status == http::StatusCode::TOO_MANY_REQUESTS {
        Some(format!("Rate limited ({status})"))
    } else if status.is_server_error() {
        Some(format!("Provider error ({status})"))
    } else {
        None
    }
}

fn classify_reqwest_error(err: &reqwest::Error) -> Option<ConnectionFailure> {
    if err.is_timeout() {
        return Some(ConnectionFailure {
            reason: "Connection timed out".to_string(),
        });
    }
    if err.is_connect() {
        return Some(ConnectionFailure {
            reason: "Connection failed".to_string(),
        });
    }
    classify_source_chain(err)
}

/// Walk a source chain for the concrete `io::Error` kinds that indicate a
/// dropped or unestablishable connection.
fn classify_source_chain(err: &(dyn std::error::Error + 'static)) -> Option<ConnectionFailure> {
    use std::io::ErrorKind;

    let mut source = Some(err);
    while let Some(current) = source {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            let reason = match io.kind() {
                ErrorKind::ConnectionReset => Some("Connection reset"),
                ErrorKind::ConnectionRefused => Some("Connection refused"),
                ErrorKind::ConnectionAborted => Some("Connection aborted"),
                ErrorKind::BrokenPipe => Some("Connection broken"),
                ErrorKind::UnexpectedEof => Some("Connection closed"),
                ErrorKind::TimedOut => Some("Connection timed out"),
                _ => None,
            };
            if let Some(reason) = reason {
                return Some(ConnectionFailure {
                    reason: reason.to_string(),
                });
            }
        }
        source = current.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as StdError;
    use std::fmt;

    fn completion_err(err: CompletionError) -> StreamingError {
        StreamingError::Completion(err)
    }

    fn prompt_err(err: CompletionError) -> StreamingError {
        StreamingError::Prompt(Box::new(
            rig_agent::completion::PromptError::CompletionError(err),
        ))
    }

    fn max_turns_err() -> StreamingError {
        StreamingError::Prompt(Box::new(
            rig_agent::completion::PromptError::MaxTurnsError {
                max_turns: 1,
                chat_history: Box::default(),
                prompt: Box::new(rig_core::message::Message::user("x")),
            },
        ))
    }

    /// A synthetic error whose source is an `io::Error` of `kind`, standing in
    /// for a wrapped reqwest error (the downcast path needs a real reqwest
    /// error, which cannot be constructed synthetically).
    #[derive(Debug)]
    struct WrappedIo(std::io::Error);

    impl fmt::Display for WrappedIo {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "wrapped: {}", self.0)
        }
    }

    impl StdError for WrappedIo {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&self.0)
        }
    }

    fn status_err(status: http::StatusCode) -> StreamingError {
        completion_err(CompletionError::HttpError(
            rig_core::http_client::Error::InvalidStatusCode(status),
        ))
    }

    fn transport_err(inner: Box<dyn StdError + Send + Sync + 'static>) -> StreamingError {
        completion_err(CompletionError::HttpError(
            rig_core::http_client::Error::Instance(inner),
        ))
    }

    #[test]
    fn timeout_status_is_retryable() {
        let got = classify_connection_error(&status_err(http::StatusCode::REQUEST_TIMEOUT))
            .expect("408 is retryable");
        assert!(got.reason.contains("408"));
    }

    #[test]
    fn rate_limit_status_is_retryable() {
        let got = classify_connection_error(&status_err(http::StatusCode::TOO_MANY_REQUESTS))
            .expect("429 is retryable");
        assert!(got.reason.contains("429"));
    }

    #[test]
    fn server_error_status_is_retryable() {
        for status in [
            http::StatusCode::INTERNAL_SERVER_ERROR,
            http::StatusCode::BAD_GATEWAY,
            http::StatusCode::SERVICE_UNAVAILABLE,
            http::StatusCode::GATEWAY_TIMEOUT,
        ] {
            let got = classify_connection_error(&status_err(status))
                .unwrap_or_else(|| panic!("{status} should be retryable"));
            assert!(got.reason.contains("Provider error"));
        }
    }

    #[test]
    fn client_error_statuses_are_not_retryable() {
        for status in [
            http::StatusCode::UNAUTHORIZED,
            http::StatusCode::FORBIDDEN,
            http::StatusCode::NOT_FOUND,
            http::StatusCode::BAD_REQUEST,
        ] {
            assert!(
                classify_connection_error(&status_err(status)).is_none(),
                "{status} should not be retryable"
            );
        }
    }

    #[test]
    fn io_reset_is_retryable_with_reason() {
        let got = classify_connection_error(&transport_err(Box::new(WrappedIo(
            std::io::Error::from(std::io::ErrorKind::ConnectionReset),
        ))))
        .expect("connection reset is retryable");
        assert_eq!(got.reason, "Connection reset");
    }

    #[test]
    fn io_refused_is_retryable() {
        let got = classify_connection_error(&transport_err(Box::new(WrappedIo(
            std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        ))))
        .expect("connection refused is retryable");
        assert_eq!(got.reason, "Connection refused");
    }

    #[test]
    fn io_timeout_is_retryable() {
        let got = classify_connection_error(&transport_err(Box::new(WrappedIo(
            std::io::Error::from(std::io::ErrorKind::TimedOut),
        ))))
        .expect("timed out is retryable");
        assert_eq!(got.reason, "Connection timed out");
    }

    #[test]
    fn unrelated_io_error_is_not_retryable() {
        let got = transport_err(Box::new(WrappedIo(std::io::Error::from(
            std::io::ErrorKind::InvalidData,
        ))));
        assert!(classify_connection_error(&got).is_none());
    }

    #[test]
    fn sse_transport_flatten_is_retryable() {
        let got = classify_connection_error(&completion_err(CompletionError::ProviderError(
            "Http client error: request or response body error".into(),
        )))
        .expect("flattened transport error is retryable");
        assert_eq!(got.reason, "Connection lost");
    }

    #[test]
    fn provider_api_error_is_not_retryable() {
        assert!(
            classify_connection_error(&completion_err(CompletionError::ProviderError(
                "model not found".into(),
            )))
            .is_none()
        );
    }

    #[test]
    fn non_transport_completion_errors_are_not_retryable() {
        assert!(
            classify_connection_error(&completion_err(CompletionError::JsonError(
                serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
            )))
            .is_none()
        );
        assert!(classify_connection_error(&max_turns_err()).is_none());
    }

    #[test]
    fn prompt_wrapped_completion_error_is_classified() {
        let got = classify_connection_error(&prompt_err(CompletionError::ProviderError(
            "Http client error: error sending request".into(),
        )))
        .expect("prompt-wrapped transport error is retryable");
        assert_eq!(got.reason, "Connection lost");
    }
}
