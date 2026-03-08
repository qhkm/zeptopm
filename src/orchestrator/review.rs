//! Review decision parsing for reviewer job artifacts.

use serde::Deserialize;

/// A reviewer's decision after examining a coder's output.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewDecision {
    Approved,
    Revise { feedback: String },
    Rejected { reason: String },
}

#[derive(Deserialize)]
struct ReviewJson {
    decision: String,
    #[serde(default)]
    feedback: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// Parse a review decision from artifact text.
///
/// Tries two strategies:
/// 1. JSON block: `{"decision": "approved|revise|rejected", "feedback": "..."}`
/// 2. Keyword scan: looks for APPROVED, REVISE, REJECTED in text
pub fn parse_review_decision(text: &str) -> ReviewDecision {
    if let Some(decision) = try_parse_json(text) {
        return decision;
    }
    keyword_scan(text)
}

fn try_parse_json(text: &str) -> Option<ReviewDecision> {
    let json_str = extract_json_block(text)?;
    let parsed: ReviewJson = serde_json::from_str(&json_str).ok()?;

    match parsed.decision.to_lowercase().as_str() {
        "approved" | "approve" | "lgtm" => Some(ReviewDecision::Approved),
        "revise" | "revision" | "revision_needed" | "needs_revision" | "needs_changes" => {
            Some(ReviewDecision::Revise {
                feedback: parsed.feedback.or(parsed.reason).unwrap_or_default(),
            })
        }
        "rejected" | "reject" => Some(ReviewDecision::Rejected {
            reason: parsed.reason.or(parsed.feedback).unwrap_or_default(),
        }),
        _ => None,
    }
}

fn extract_json_block(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // Try ```json ... ```
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    // Try raw JSON object
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            let candidate = &trimmed[start..=end];
            if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

fn keyword_scan(text: &str) -> ReviewDecision {
    let upper = text.to_uppercase();

    if upper.contains("REJECTED") {
        return ReviewDecision::Rejected {
            reason: text.to_string(),
        };
    }
    if upper.contains("REVISE")
        || upper.contains("NEEDS REVISION")
        || upper.contains("NEEDS CHANGES")
        || upper.contains("REVISION NEEDED")
    {
        return ReviewDecision::Revise {
            feedback: text.to_string(),
        };
    }
    // Default: approved (includes APPROVED, LGTM, LOOKS GOOD, or unclear)
    ReviewDecision::Approved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_approved() {
        let text = r#"```json
{"decision": "approved", "feedback": "Looks great!"}
```"#;
        assert_eq!(parse_review_decision(text), ReviewDecision::Approved);
    }

    #[test]
    fn test_parse_json_revise() {
        let text = r#"{"decision": "revise", "feedback": "Missing error handling for edge cases"}"#;
        match parse_review_decision(text) {
            ReviewDecision::Revise { feedback } => {
                assert!(feedback.contains("error handling"));
            }
            other => panic!("expected Revise, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_json_rejected() {
        let text = r#"{"decision": "rejected", "reason": "Fundamentally wrong approach"}"#;
        match parse_review_decision(text) {
            ReviewDecision::Rejected { reason } => {
                assert!(reason.contains("wrong approach"));
            }
            other => panic!("expected Rejected, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_json_in_markdown() {
        let text = r#"Here is my review:

```json
{"decision": "revise", "feedback": "Add input validation"}
```

Overall the code is decent but needs this fix."#;
        match parse_review_decision(text) {
            ReviewDecision::Revise { feedback } => {
                assert!(feedback.contains("input validation"));
            }
            other => panic!("expected Revise, got {:?}", other),
        }
    }

    #[test]
    fn test_keyword_approved() {
        let text = "The implementation looks good. APPROVED.";
        assert_eq!(parse_review_decision(text), ReviewDecision::Approved);
    }

    #[test]
    fn test_keyword_revise() {
        let text = "This needs changes. Please revise the error handling.";
        match parse_review_decision(text) {
            ReviewDecision::Revise { .. } => {}
            other => panic!("expected Revise, got {:?}", other),
        }
    }

    #[test]
    fn test_keyword_rejected() {
        let text = "This approach is fundamentally flawed. REJECTED.";
        match parse_review_decision(text) {
            ReviewDecision::Rejected { .. } => {}
            other => panic!("expected Rejected, got {:?}", other),
        }
    }

    #[test]
    fn test_ambiguous_defaults_to_approved() {
        let text = "The code is fine. No issues found.";
        assert_eq!(parse_review_decision(text), ReviewDecision::Approved);
    }
}
