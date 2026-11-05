//! Masking of obvious secrets in transcript text.

use std::sync::LazyLock;

use regex::Regex;

/// The replacement for every masked secret.
pub const MASK: &str = "[REDACTED]";

struct Rule {
    re: Regex,
    /// Replacement; `$1` keeps a captured prefix such as `password=`.
    with: &'static str,
}

/// `password=` / `"token": ` style prefixes, captured as group 1.
const CREDENTIAL_KEY: &str = r#"(?i)((?:\b|_)(?:password|passwd|pwd|token|secret|api[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret)"?\s*[=:]\s*)"#;

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let rule = |re: &str, with: &'static str| Rule {
        re: Regex::new(re).expect("valid redaction regex"),
        with,
    };
    vec![
        // PEM private keys, also when the END line was cut off.
        rule(
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
            "[REDACTED PRIVATE KEY]",
        ),
        // Authorization headers, whatever the scheme.
        rule(
            r#"(?i)(\bauthorization"?\s*[:=]\s*"?)(?:(?:bearer|basic|token|bot)\s+)?[^\s"',;]+"#,
            "${1}[REDACTED]",
        ),
        rule(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{8,}", "${1}[REDACTED]"),
        // Anthropic / OpenAI style keys (sk-…, sk-ant-…, sk-proj-…).
        rule(r"\bsk-[A-Za-z0-9_-]{16,}", MASK),
        // GitHub tokens.
        rule(r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}", MASK),
        rule(r"\bgithub_pat_[A-Za-z0-9_]{20,}", MASK),
        // AWS access key ids.
        rule(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", MASK),
        // Slack tokens.
        rule(r"\bxox[abposr]-[A-Za-z0-9-]{10,}", MASK),
        // key=value / key: value credentials, also as JSON keys; quotes
        // around the value are kept. A quoted value may contain escaped
        // quotes and may be cut off (no closing quote): then everything to
        // the end of the text is masked.
        rule(
            &format!(r#"{CREDENTIAL_KEY}"(?:[^"\\]|\\.)*(?:"|\\?\z)"#),
            r#"${1}"[REDACTED]""#,
        ),
        rule(
            &format!(r#"{CREDENTIAL_KEY}'(?:[^'\\]|\\.)*(?:'|\\?\z)"#),
            "${1}'[REDACTED]'",
        ),
        rule(
            &format!(r#"{CREDENTIAL_KEY}[^\s&"',;}}]+"#),
            "${1}[REDACTED]",
        ),
    ]
});

/// Mask API keys, tokens, `Authorization:` / bearer values, PEM private
/// key blocks and `password=` / `token=` style values.
pub fn redact(text: &str) -> String {
    let mut out = std::borrow::Cow::Borrowed(text);
    for rule in RULES.iter() {
        if rule.re.is_match(&out) {
            out = std::borrow::Cow::Owned(rule.re.replace_all(&out, rule.with).into_owned());
        }
    }
    out.into_owned()
}

/// Object keys whose values are credentials (`password`, `GITHUB_TOKEN`,
/// `Authorization`, `api_key`…).
static CREDENTIAL_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[_-])(?:password|passwd|pwd|token|secret|api[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret|authorization|cookie)$",
    )
    .expect("valid regex")
});

/// [`redact`] applied to every string inside a JSON value, with values
/// under credential-like keys replaced by [`MASK`]. Redacting the decoded
/// strings avoids the escaped quotes of serialized JSON hiding a secret.
pub fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => Value::String(redact(s)),
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let v = if CREDENTIAL_NAME.is_match(k) && !v.is_null() {
                        Value::String(MASK.to_string())
                    } else {
                        redact_value(v)
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}
