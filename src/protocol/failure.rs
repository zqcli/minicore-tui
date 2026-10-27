//! The friendly account of a failed turn, shared by the TUI and the phone.

use super::{Delivery, ModelError};

/// A failed turn in words: a short title, what to do about it, and the
/// built-in command that helps, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub title: String,
    /// Empty when there is nothing to add to the title.
    pub hint: String,
    /// `compact` or `model`.
    pub action: Option<&'static str>,
}

type Row = (&'static str, &'static str, Option<&'static str>);

const INTERNAL: &str = "Something failed inside MiniCore; the log has details.";

impl Failure {
    pub fn new(kind: &str, error: Option<&ModelError>) -> Self {
        match error.filter(|error| kind == "model" && !error.kind.is_empty()) {
            Some(error) => Self::model(error),
            None => match outcome_row(kind) {
                Some((title, hint, action)) => Self {
                    title: title.to_owned(),
                    hint: hint.to_owned(),
                    action,
                },
                None => Self {
                    title: spaced(kind),
                    hint: String::new(),
                    action: None,
                },
            },
        }
    }

    fn model(error: &ModelError) -> Self {
        let (title, hint, action) = match model_row(&error.kind) {
            Some((title, hint, action)) => (title.to_owned(), hint, action),
            None => (spaced(&error.kind), INTERNAL, None),
        };
        let mut hint = hint.to_owned();
        if let Some(millis) = error.retry_after_millis {
            hint.push_str(&format!(" Retry after {} s.", millis.div_ceil(1000)));
        }
        match error.delivery {
            Some(Delivery::Started) => hint.push_str(" The partial reply above is kept."),
            Some(Delivery::Unknown) if error.kind != "request_outcome_unknown" => {
                hint.push_str(" The provider may have processed it.");
            }
            _ => {}
        }
        Self {
            title,
            hint,
            action,
        }
    }
}

fn spaced(kind: &str) -> String {
    kind.replace('_', " ")
}

fn model_row(kind: &str) -> Option<Row> {
    Some(match kind {
        "rate_limited" => (
            "rate limited",
            "The provider is limiting requests. Wait a moment, then send \"continue\".",
            None,
        ),
        "quota_exceeded" => (
            "quota exceeded",
            "The account's quota or balance is used up. Check the plan or billing, or switch model with /model.",
            Some("model"),
        ),
        "auth_missing" => (
            "API key missing",
            "No API key was found. Set the variable named by api_key_env in models.toml.",
            None,
        ),
        "auth_rejected" => (
            "API key rejected",
            "The provider rejected the API key or its permissions. Check the key in api_key_env.",
            Some("model"),
        ),
        "context_overflow" => (
            "context full",
            "The conversation no longer fits the model's context. Run /compact, then continue.",
            Some("compact"),
        ),
        "timeout" => (
            "timed out",
            "The provider did not answer in time. Send \"continue\" to try again.",
            None,
        ),
        "transport_unavailable" => (
            "connection failed",
            "MiniCore could not reach the provider. Check the network or base_url.",
            None,
        ),
        "provider_unavailable" | "unavailable" => (
            "provider unavailable",
            "The provider is overloaded or down. Try again shortly or switch model.",
            Some("model"),
        ),
        "stream_interrupted" | "incomplete_response" => (
            "reply cut off",
            "The reply stopped before it finished. Send \"continue\" to resume.",
            None,
        ),
        "invalid_provider_response" => (
            "invalid reply",
            "The provider sent a reply MiniCore could not read.",
            None,
        ),
        "request_outcome_unknown" => (
            "outcome unknown",
            "The provider may have processed the request. Check before retrying.",
            None,
        ),
        "invalid_request" => (
            "request rejected",
            "The provider rejected the request; the model name, parameters or relay may not match.",
            Some("model"),
        ),
        "unexpected_tool_call" => (
            "unexpected tool call",
            "The model called a tool that was not offered.",
            None,
        ),
        "internal" | "panicked" => ("internal error", INTERNAL, None),
        _ => return None,
    })
}

fn outcome_row(kind: &str) -> Option<Row> {
    Some(match kind {
        "model" => ("model error", "", None),
        "output_limit" => (
            "output limit reached",
            "The reply hit the output token limit. Send \"continue\" for the rest.",
            None,
        ),
        "max_tool_rounds" => (
            "too many tool rounds",
            "The turn stopped at the tool-round limit. Send \"continue\" to keep going.",
            None,
        ),
        "refused" => ("the model refused", "The model declined to answer.", None),
        "policy" => ("blocked by policy", "A policy blocked this request.", None),
        "interaction" => ("interaction failed", "", None),
        "prompt" => ("prompt error", "", None),
        "internal" | "" => ("internal error", INTERNAL, None),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::LoopOutcome;
    use serde_json::json;

    fn failed(outcome: serde_json::Value) -> Failure {
        let outcome: LoopOutcome = serde_json::from_value(outcome).unwrap();
        outcome.failure().unwrap()
    }

    fn model(error: serde_json::Value) -> Failure {
        failed(json!({"type": "failed", "kind": "model", "model_error": error}))
    }

    #[test]
    fn a_model_error_gets_its_title_hint_and_action() {
        let failure = model(json!({"kind": "context_overflow"}));
        assert_eq!(failure.title, "context full");
        assert_eq!(
            failure.hint,
            "The conversation no longer fits the model's context. Run /compact, then continue."
        );
        assert_eq!(failure.action, Some("compact"));
        assert_eq!(model(json!({"kind": "timeout"})).action, None);
    }

    #[test]
    fn hint_suffixes_follow_retry_after_and_delivery() {
        let failure = model(json!({"kind": "rate_limited", "delivery": "started",
            "retry_after_millis": 11_500}));
        assert_eq!(
            failure.hint,
            "The provider is limiting requests. Wait a moment, then send \"continue\". \
             Retry after 12 s. The partial reply above is kept."
        );
        let unknown = model(json!({"kind": "timeout", "delivery": "unknown"}));
        assert!(
            unknown
                .hint
                .ends_with(" The provider may have processed it.")
        );
        let already = model(json!({"kind": "request_outcome_unknown", "delivery": "unknown"}));
        assert!(
            !already.hint.contains("may have processed it."),
            "{}",
            already.hint
        );
        let fresh = model(json!({"kind": "timeout", "delivery": "not_started",
            "retry_after_millis": null}));
        assert_eq!(fresh.hint, model(json!({"kind": "timeout"})).hint);
    }

    #[test]
    fn unknown_model_kinds_are_named_and_called_internal() {
        let failure = model(json!({"kind": "solar_flare"}));
        assert_eq!(failure.title, "solar flare");
        assert_eq!(failure.hint, INTERNAL);
    }

    #[test]
    fn turns_that_failed_outside_the_model_have_their_own_words() {
        let limit = failed(json!({"type": "failed", "kind": "output_limit"}));
        assert_eq!(limit.title, "output limit reached");
        assert!(
            limit
                .hint
                .starts_with("The reply hit the output token limit.")
        );
        let bare = failed(json!({"type": "failed", "kind": "model"}));
        assert_eq!(
            (bare.title.as_str(), bare.hint.as_str()),
            ("model error", "")
        );
        let odd = failed(json!({"type": "failed", "kind": "tool_budget"}));
        assert_eq!((odd.title.as_str(), odd.hint.as_str()), ("tool budget", ""));
    }

    #[test]
    fn a_turn_that_did_not_fail_has_no_failure() {
        assert_eq!(LoopOutcome::Completed.failure(), None);
    }
}
