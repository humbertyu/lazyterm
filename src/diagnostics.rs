const SECRET_MARKERS: [&str; 14] = [
    "password=",
    "password:",
    "passwd=",
    "passwd:",
    "passphrase=",
    "passphrase:",
    "secret=",
    "secret:",
    "token=",
    "token:",
    "api_key=",
    "apikey=",
    "authorization:",
    "authorization: ",
];

pub fn redact_sensitive_text(value: &str) -> String {
    let mut output = value.to_string();
    for marker in SECRET_MARKERS {
        redact_marker(&mut output, marker);
    }
    redact_uri_credentials(&output)
}

fn redact_marker(value: &mut String, marker: &str) {
    let lower = value.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(found) = lower[offset..].find(marker) {
        let after_marker = offset + found + marker.len();
        let start = after_marker
            + value[after_marker..]
                .chars()
                .take_while(|character| character.is_whitespace())
                .map(char::len_utf8)
                .sum::<usize>();
        let end = value[start..]
            .find(|character: char| {
                character.is_whitespace()
                    || matches!(
                        character,
                        '&' | ';' | ',' | '"' | '\'' | '@' | '/' | '?' | '#'
                    )
            })
            .map_or(value.len(), |length| start + length);
        if start < end {
            value.replace_range(start..end, "[REDACTED]");
        }
        offset = start + "[REDACTED]".len();
        if offset >= value.len() {
            break;
        }
    }
}

fn redact_uri_credentials(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(scheme_end) = remaining.find("://") {
        let authority_start = scheme_end + 3;
        output.push_str(&remaining[..authority_start]);
        let tail = &remaining[authority_start..];
        let authority_end = tail.find(['/', '?', '#', ' ', '\n']).unwrap_or(tail.len());
        let authority = &tail[..authority_end];
        if let Some(at) = authority.rfind('@')
            && let Some(colon) = authority[..at].find(':')
        {
            output.push_str(&authority[..colon + 1]);
            output.push_str("[REDACTED]");
            output.push_str(&authority[at..]);
        } else {
            output.push_str(authority);
        }
        remaining = &tail[authority_end..];
    }
    output.push_str(remaining);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_password_like_fields_and_uri_credentials() {
        assert_eq!(
            redact_sensitive_text(
                "password=hunter2 token=abc authorization: BearerXYZ https://user:p%40ss@example.test/x"
            ),
            "password=[REDACTED] token=[REDACTED] authorization: [REDACTED] https://user:[REDACTED]@example.test/x"
        );
    }

    #[test]
    fn preserves_diagnostics_without_sensitive_fields() {
        let detail = "Permission denied (publickey).\nHost key fingerprint SHA256:abc";
        assert_eq!(redact_sensitive_text(detail), detail);
    }
}
