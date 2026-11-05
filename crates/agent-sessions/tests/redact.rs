use agent_sessions::redact;

fn masked(input: &str, secret: &str) {
    let out = redact(input);
    assert!(!out.contains(secret), "{secret} survived in {out:?}");
    assert!(out.contains("[REDACTED"), "{out:?}");
}

#[test]
fn openai_and_anthropic_keys() {
    masked(
        "key sk-abcdefghijklmnopqrstuv end",
        "sk-abcdefghijklmnopqrstuv",
    );
    masked(
        "ANTHROPIC_API_KEY=sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv-xyz",
        "AbCdEfGhIjKlMnOpQrStUv",
    );
    masked("sk-proj-0123456789abcdefABCDEF", "0123456789abcdefABCDEF");
}

#[test]
fn github_tokens() {
    masked("ghp_0123456789abcdefghijABCDEFGHIJ", "0123456789abcdefghij");
    masked("gho_0123456789abcdefghijABCDEFGHIJ", "0123456789abcdefghij");
    masked(
        "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
        "11ABCDEFG0123456789",
    );
}

#[test]
fn aws_access_key_id() {
    masked("aws AKIAIOSFODNN7EXAMPLE here", "AKIAIOSFODNN7EXAMPLE");
}

#[test]
fn slack_tokens() {
    masked("xoxb-1234567890-abcdefghij", "1234567890-abcdefghij");
    masked("xoxp-1234567890-abcdefghij", "1234567890-abcdefghij");
}

#[test]
fn authorization_headers_and_bearer() {
    assert_eq!(
        redact("Authorization: Bearer abc.def.ghi"),
        "Authorization: [REDACTED]"
    );
    assert_eq!(
        redact(r#"{"authorization": "Basic dXNlcjpwYXNz"}"#),
        r#"{"authorization": "[REDACTED]"}"#
    );
    masked(
        "curl -H 'X: y' bearer eyJhbGciOiJIUzI1NiJ9.payload",
        "eyJhbGciOiJIUzI1NiJ9",
    );
}

#[test]
fn pem_private_keys() {
    let pem = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nIBAAKC\n-----END RSA PRIVATE KEY-----\nafter";
    assert_eq!(redact(pem), "before\n[REDACTED PRIVATE KEY]\nafter");
    let cut = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk";
    assert_eq!(redact(cut), "[REDACTED PRIVATE KEY]");
    let public = "-----BEGIN PUBLIC KEY-----\nMFkw\n-----END PUBLIC KEY-----";
    assert_eq!(redact(public), public);
}

#[test]
fn password_and_token_values() {
    assert_eq!(redact("password=hunter2 next"), "password=[REDACTED] next");
    assert_eq!(
        redact("db?user=a&token=xyz&x=1"),
        "db?user=a&token=[REDACTED]&x=1"
    );
    assert_eq!(redact(r#"{"token": "abc"}"#), r#"{"token": "[REDACTED]"}"#);
    assert_eq!(redact("PASSWORD: 'p a s s'"), "PASSWORD: '[REDACTED]'");
    assert_eq!(redact("api_key=123456"), "api_key=[REDACTED]");
    assert_eq!(redact("OPENAI_API_KEY=abc123"), "OPENAI_API_KEY=[REDACTED]");
    assert_eq!(
        redact("export GITHUB_TOKEN=xyz"),
        "export GITHUB_TOKEN=[REDACTED]"
    );
}

#[test]
fn ordinary_text_is_untouched() {
    for text in [
        "tokens_input: 5",
        "the skill reads SKILL.md",
        "ask-me later",
        "Use a password manager.",
    ] {
        assert_eq!(redact(text), text);
    }
}

#[test]
fn escaped_and_unterminated_quoted_values() {
    assert_eq!(
        redact(r#"password="ab\"cd" rest"#),
        r#"password="[REDACTED]" rest"#
    );
    assert_eq!(redact(r#"token='a\'b' x"#), "token='[REDACTED]' x");
    // Cut off before the closing quote: masked to the end.
    assert_eq!(redact("secret=\"abcdef"), "secret=\"[REDACTED]\"");
    assert_eq!(redact("secret='abcdef\\"), "secret='[REDACTED]'");
}

#[test]
fn long_values_are_masked_whole() {
    let secret = "q".repeat(9_000);
    let out = redact(&format!("password=\"{secret}\" tail"));
    assert_eq!(out, "password=\"[REDACTED]\" tail");
    let out = redact(&format!("password=\"{secret}"));
    assert!(!out.contains("qqqq"), "{}", &out[..out.len().min(80)]);
}

#[test]
fn json_values_are_redacted_before_serializing() {
    use agent_sessions::redact::redact_value;
    let v = serde_json::json!({
        "command": "curl -d \"password=hunter2\" https://x.invalid",
        "env": {"GITHUB_TOKEN": "abc", "PATH": "/usr/bin"},
        "headers": [{"Authorization": "Basic Zm9vOmJhcg=="}],
        "n": 3
    });
    let out = redact_value(&v).to_string();
    assert!(!out.contains("hunter2"), "{out}");
    assert!(!out.contains("\"abc\""), "{out}");
    assert!(!out.contains("Zm9vOmJhcg"), "{out}");
    assert!(out.contains("/usr/bin") && out.contains("\"n\":3"), "{out}");
}
