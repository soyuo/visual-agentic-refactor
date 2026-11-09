use serde::{Deserialize, Serialize};

/// Provider-neutral finding categories accepted from structured AI responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FindingCategory {
    Correctness,
    Security,
    Performance,
    Reliability,
    Maintainability,
    Testability,
    Style,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Critical,
    High,
    Medium,
    Low,
    Informational,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingConfidence {
    High,
    Medium,
    Low,
}

/// Provider-neutral shape decoded before source and snapshot validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewFinding {
    pub title: String,
    pub description: String,
    pub category: FindingCategory,
    pub severity: FindingSeverity,
    pub confidence: FindingConfidence,
    pub evidence: Vec<FindingEvidence>,
    pub affected_symbols: Vec<String>,
    pub suggestion: String,
    pub test_suggestion: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewFindings {
    pub findings: Vec<ReviewFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FindingEvidence {
    pub relative_path: String,
    /// One-based Unicode-scalar position; the end position is exclusive.
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    /// Exact source text between the claimed start and end positions.
    pub excerpt: String,
}

#[cfg(test)]
mod tests {
    use super::{FindingCategory, FindingConfidence, FindingSeverity, ReviewFindings};
    use serde_json::json;

    #[test]
    fn deserializes_provider_neutral_finding_contract() {
        let response = serde_json::from_value::<ReviewFindings>(json!({
            "findings": [{
                "title": "Check null handling",
                "description": "The value is dereferenced without a guard.",
                "category": "correctness",
                "severity": "high",
                "confidence": "medium",
                "evidence": [{
                    "relativePath": "src/Cart.kt",
                    "startLine": 3,
                    "startColumn": 5,
                    "endLine": 3,
                    "endColumn": 16,
                    "excerpt": "item.price"
                }],
                "affectedSymbols": ["Cart.total"],
                "suggestion": "Guard the value before dereferencing it.",
                "testSuggestion": "Add a test for a missing item."
            }]
        }))
        .expect("provider contract fixture should deserialize");

        let finding = &response.findings[0];
        assert_eq!(finding.category, FindingCategory::Correctness);
        assert_eq!(finding.severity, FindingSeverity::High);
        assert_eq!(finding.confidence, FindingConfidence::Medium);
        assert_eq!(finding.evidence[0].start_column, 5);
        assert_eq!(finding.evidence[0].end_column, 16);
    }
}
