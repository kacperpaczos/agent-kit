//! Path-template expansion against a [`Host`].

use std::path::PathBuf;

use crate::host::Host;

/// Expand a location template.
///
/// Grammar: alternatives separated by `" || "`; each alternative may start with
/// `~/` and may contain `$VAR` or `${VAR}`. The first alternative whose
/// variables are all set (non-empty) wins; `None` when no alternative resolves.
/// A `$` not followed by a variable name is kept literally.
pub fn expand(template: &str, host: &dyn Host) -> Option<PathBuf> {
    template
        .split(" || ")
        .find_map(|alt| expand_one(alt.trim(), host))
}

fn expand_one(alt: &str, host: &dyn Host) -> Option<PathBuf> {
    let (base, rest) = match alt.strip_prefix("~/") {
        Some(rest) => (host.home(), rest),
        None => (PathBuf::new(), alt),
    };
    let mut expanded = String::new();
    expand_vars(rest, host, &mut expanded)?;
    Some(base.join(expanded))
}

/// Append `text` to `out` with `$VAR`/`${VAR}` substituted from `host`;
/// `None` when any referenced variable is unset or empty.
fn expand_vars(text: &str, host: &dyn Host, out: &mut String) -> Option<()> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            let ch = text[i..].chars().next()?;
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let (name, next): (&str, usize) = if bytes.get(i + 1) == Some(&b'{') {
            let start = i + 2;
            match text[start..].find('}') {
                Some(pos) => (&text[start..start + pos], start + pos + 1),
                None => {
                    out.push('$');
                    i += 1;
                    continue;
                }
            }
        } else {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end == start {
                out.push('$');
                i += 1;
                continue;
            }
            (&text[start..end], end)
        };
        out.push_str(&host.var(name)?);
        i = next;
    }
    Some(())
}
