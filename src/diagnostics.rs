use regex::Regex;
use std::{
    io::{self, BufRead, Read},
    sync::LazyLock,
};

static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
    )
    .unwrap()
});
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?im)(\b(?:authorization|proxy-authorization|cookie|set-cookie)\b["']?\s*:\s*)[^\r\n]+"#,
    )
    .unwrap()
});
static FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r#"(?i)(\b(?:password|passwd|passphrase|secret|token|api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret)\b["']?\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,;&]+)"#
).unwrap()
});
static FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r#"(?i)(--(?:password|passwd|passphrase|secret|token|api-key|access-token|client-secret)(?:=|[ \t]+))(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s]+)"#
).unwrap()
});
static URI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([A-Za-z][A-Za-z0-9+.-]*://[^\s/?#@:]+:)[^\s/?#]*@").unwrap());

pub fn redact_sensitive_text(value: &str) -> String {
    let value = PRIVATE_KEY.replace_all(value, "[PRIVATE KEY REDACTED]");
    let value = URI.replace_all(&value, "${1}[REDACTED]@");
    let value = HEADER.replace_all(&value, "${1}[REDACTED]");
    let value = FIELD.replace_all(&value, "${1}[REDACTED]");
    FLAG.replace_all(&value, "${1}[REDACTED]").into_owned()
}

/// Redact complete diagnostic lines, so read boundaries cannot split secret markers.
pub struct RedactingReader<R> {
    inner: io::BufReader<R>,
    pending: Vec<u8>,
    offset: usize,
    private_key: bool,
}

impl<R: Read> RedactingReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            inner: io::BufReader::new(reader),
            pending: Vec::new(),
            offset: 0,
            private_key: false,
        }
    }

    fn next_line(&mut self) -> io::Result<bool> {
        let mut line = Vec::new();
        let mut overflow = false;
        loop {
            let buffer = self.inner.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let size = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1);
            let ended = buffer[size - 1] == b'\n';
            if !overflow && line.len() + size <= 16 * 1024 {
                line.extend_from_slice(&buffer[..size]);
            } else {
                overflow = true;
            }
            self.inner.consume(size);
            if ended {
                break;
            }
        }
        if line.is_empty() && !overflow {
            return Ok(false);
        }
        let text = String::from_utf8_lossy(&line);
        let begins_key = text.contains("-----BEGIN ") && text.contains("PRIVATE KEY-----");
        let ends_key = text.contains("-----END ") && text.contains("PRIVATE KEY-----");
        let hidden = self.private_key || begins_key;
        self.private_key = (self.private_key || begins_key) && !ends_key;
        self.pending = if overflow {
            self.private_key = true;
            b"[oversized diagnostic omitted]\n".to_vec()
        } else if hidden {
            b"[private key material omitted]\n".to_vec()
        } else {
            redact_sensitive_text(&text).into_bytes()
        };
        self.offset = 0;
        Ok(true)
    }
}

impl<R: Read> Read for RedactingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.offset == self.pending.len() && !self.next_line()? {
            return Ok(0);
        }
        let size = buffer.len().min(self.pending.len() - self.offset);
        buffer[..size].copy_from_slice(&self.pending[self.offset..self.offset + size]);
        self.offset += size;
        Ok(size)
    }
}

pub fn support_report() -> String {
    use std::io::IsTerminal;
    format!(
        "LazyTerm {}\nOS: {}\nArchitecture: {}\nTerminal input: {}\nCredential backend: {}\nConfiguration and credentials: not collected\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        io::stdin().is_terminal(),
        if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else {
            "experimental platform backend"
        }
    )
}

pub fn collect(reader: impl Read) -> io::Result<String> {
    let mut reader = RedactingReader::new(reader);
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let size = reader.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        let keep = size.min((16_usize * 1024).saturating_sub(output.len()));
        output.extend_from_slice(&buffer[..keep]);
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_repeated_unicode_quoted_fields_headers_urls_flags_and_private_keys() {
        let input = "password=一二三 password=abc password=long-value\n\
            {\"password\": \"with spaces\", \"api_key\":\"秘密\"}\n\
            Authorization: Bearer actual-token\nCookie: session=abc; csrf=def\n\
            --password 'cli secret'\nhttps://user:p%40ss@example.test/x?token=query-secret&ok=1\n\
            -----BEGIN OPENSSH PRIVATE KEY-----\nkey-material\n-----END OPENSSH PRIVATE KEY-----";
        let masked = redact_sensitive_text(input);
        for secret in [
            "一二三",
            "abc",
            "long-value",
            "with spaces",
            "秘密",
            "actual-token",
            "session=",
            "cli secret",
            "p%40ss",
            "query-secret",
            "key-material",
        ] {
            assert!(!masked.contains(secret), "Unmasked {secret}: {masked}");
        }
        assert!(masked.contains("example.test/x?token=[REDACTED]&ok=1"));
    }

    #[test]
    fn stream_redaction_handles_one_byte_reads_and_multiline_private_keys() {
        struct Tiny<'a>(&'a [u8]);
        impl Read for Tiny<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let size = 1.min(buffer.len()).min(self.0.len());
                buffer[..size].copy_from_slice(&self.0[..size]);
                self.0 = &self.0[size..];
                Ok(size)
            }
        }
        let mut output = String::new();
        RedactingReader::new(Tiny(b"password=split-secret\n-----BEGIN OPENSSH PRIVATE KEY-----\nprivate-material\n-----END OPENSSH PRIVATE KEY-----\nPermission denied.\n"))
            .read_to_string(&mut output).unwrap();
        assert!(!output.contains("split-secret"));
        assert!(!output.contains("private-material"));
        assert!(output.contains("Permission denied."));
    }

    #[test]
    fn oversized_diagnostic_lines_are_bounded_and_fail_closed() {
        let text = format!("{}password=secret\nmore-secret\n", "x".repeat(40_000));
        let mut output = String::new();
        RedactingReader::new(text.as_bytes())
            .read_to_string(&mut output)
            .unwrap();
        assert!(output.len() < 200);
        assert!(!output.contains("secret"));
    }

    #[test]
    fn preserves_non_sensitive_diagnostics_and_support_report_omits_user_data() {
        let detail = "Permission denied (publickey).\nHost key fingerprint SHA256:abc";
        assert_eq!(redact_sensitive_text(detail), detail);
        assert!(support_report().contains("Configuration and credentials: not collected"));
    }
}
