//! Validation of user values before they become argv elements. Nothing
//! goes through a shell, but a value that looks like a flag would be
//! read as one, and shell metacharacters never belong in these values.

use crate::types::{MarketplaceError, Result};

/// Characters rejected in every value.
const SHELL_META: &[char] = &[
    ';', '&', '|', '`', '$', '(', ')', '<', '>', '"', '\'', '\\', '*', '?', '[', ']', '{', '}', '!',
];

/// What a value is, for its extra rules and the error message.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    /// Skill or agent name, `owner` or `repo` segment: no path separators.
    Name,
    /// Search word.
    Word,
    /// URL: `?` allowed for query strings.
    Url,
    /// Filesystem path.
    Path,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Name => "name",
            Kind::Word => "word",
            Kind::Url => "URL",
            Kind::Path => "path",
        }
    }
}

/// Reject empty values, values starting with `-`, whitespace, control
/// characters and shell metacharacters.
pub(crate) fn check(kind: Kind, value: &str) -> Result<()> {
    let label = kind.label();
    let invalid = |why: &str| {
        Err(MarketplaceError::Invalid(format!(
            "{label} {value:?}: {why}"
        )))
    };
    if value.is_empty() {
        return invalid("empty");
    }
    if value.starts_with('-') {
        return invalid("starts with '-'");
    }
    if let Some(c) = value.chars().find(|c| c.is_whitespace() || c.is_control()) {
        return invalid(&format!("contains whitespace or control character {c:?}"));
    }
    let allowed_here = |c: char| matches!(kind, Kind::Url) && c == '?';
    if let Some(c) = value
        .chars()
        .find(|c| SHELL_META.contains(c) && !allowed_here(*c))
    {
        return invalid(&format!("contains shell metacharacter {c:?}"));
    }
    if matches!(kind, Kind::Name) && value.contains('/') {
        return invalid("contains '/'");
    }
    Ok(())
}

/// A name, or the literal `*` wildcard where the channel accepts it.
pub(crate) fn check_name_or_all(value: &str) -> Result<()> {
    if value == "*" {
        Ok(())
    } else {
        check(Kind::Name, value)
    }
}
