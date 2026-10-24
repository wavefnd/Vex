// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Git source identity and credential-safe rendering. Transport credentials are
//! ephemeral; source declarations remain in user-controlled manifests only.
pub mod graph;
use std::sync::{Mutex, OnceLock};
use url::Url;

fn secrets() -> &'static Mutex<Vec<String>> {
    static SECRETS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    SECRETS.get_or_init(|| Mutex::new(Vec::new()))
}

fn secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '_'], "");
    key.starts_with("xamz")
        || key.starts_with("xgoog")
        || key.contains("token")
        || key.contains("password")
        || key.contains("secret")
        || key.contains("credential")
        || key.contains("signature")
        || matches!(
            key.as_str(),
            "auth" | "authorization" | "key" | "apikey" | "sig" | "accesskey" | "xamzsecuritytoken"
        )
}

pub fn identity(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        // Even invalid transport input must not echo userinfo in diagnostics.
        if let Some((scheme, rest)) = raw.split_once("://") {
            if let Some((authority, suffix)) = rest.split_once('/').or(Some((rest, ""))) {
                if let Some((userinfo, host)) = authority.rsplit_once('@') {
                    remember(userinfo.split(':').map(str::to_owned).collect());
                    return format!(
                        "{scheme}://{host}{}",
                        if suffix.is_empty() {
                            String::new()
                        } else {
                            format!("/{suffix}")
                        }
                    );
                }
            }
        }
        return raw.to_owned();
    };
    if !matches!(
        url.scheme(),
        "http" | "https" | "ssh" | "git" | "ftp" | "ftps"
    ) {
        return raw.to_owned();
    }
    let mut removed = Vec::new();
    let mut authentication_changed = url.password().is_some();
    if let Some(password) = url.password() {
        removed.push(password.to_owned());
    }
    let _ = url.set_password(None);
    // SSH usernames choose an account and are part of source routing; HTTP
    // userinfo is authentication. SCP-style git@host:path is left intact.
    if matches!(url.scheme(), "http" | "https" | "ftp" | "ftps") && !url.username().is_empty() {
        authentication_changed = true;
        removed.push(url.username().to_owned());
        let _ = url.set_username("");
    }
    let mut pairs = Vec::new();
    let mut query_changed = false;
    for (key, value) in url.query_pairs() {
        if secret_key(&key) {
            if !value.is_empty() {
                removed.push(value.into_owned());
            }
            query_changed = true;
        } else {
            pairs.push((key.into_owned(), value.into_owned()));
        }
    }
    if query_changed {
        for pair in url.query().unwrap_or("").split('&') {
            if let Some((key, value)) = pair.split_once('=') {
                if secret_key(&percent_encoding::percent_decode_str(key).decode_utf8_lossy()) {
                    removed.push(value.to_owned());
                }
            }
        }
        url.set_query(None);
        if !pairs.is_empty() {
            url.query_pairs_mut().extend_pairs(pairs);
        }
    }
    remember(removed);
    // Avoid normalizing valid credential-free declarations (trailing slash,
    // escaping, host case) because stored origin identity has exact semantics.
    let candidate = url.to_string();
    if authentication_changed || query_changed {
        candidate
    } else {
        raw.to_owned()
    }
}

fn remember(removed: Vec<String>) {
    if !removed.is_empty() {
        let mut known = secrets().lock().unwrap_or_else(|e| e.into_inner());
        for value in removed {
            let decoded = percent_encoding::percent_decode_str(&value)
                .decode_utf8_lossy()
                .into_owned();
            for candidate in [value, decoded] {
                if !candidate.is_empty() && !known.contains(&candidate) {
                    known.push(candidate);
                }
            }
        }
    }
}

pub fn redact(message: &str) -> String {
    // Register and sanitize complete URLs before removing standalone secrets
    // that a Git/helper diagnostic might repeat outside a URL.
    let mut rendered = String::new();
    for word in message.split_inclusive(char::is_whitespace) {
        if let Some(start) = word
            .find("https://")
            .or_else(|| word.find("http://"))
            .or_else(|| word.find("ssh://"))
        {
            let tail = &word[start..];
            let end = tail
                .find(['`', '"', '\'', ')', '\n', '\r', ' '])
                .unwrap_or(tail.len());
            rendered.push_str(&word[..start]);
            rendered.push_str(&identity(&tail[..end]));
            rendered.push_str(&tail[end..]);
        } else {
            rendered.push_str(word);
        }
    }
    let mut known = secrets().lock().unwrap_or_else(|e| e.into_inner()).clone();
    known.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in known {
        rendered = rendered.replace(&secret, "[redacted]");
    }
    rendered
}

/// Portable repository-relative package selector. Dot/empty components and
/// platform prefixes are rejected before touching a repository.
pub fn validate_subdir(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.contains(['\\', ':', '\0'])
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(
            "subdir must be a nonempty portable relative path without dot or empty components"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_removes_authentication_but_keeps_routing_and_nonsensitive_query() {
        assert_eq!(identity("https://alice:synthetic_secret@example.invalid/repo?token=synthetic_token&ref=main"), "https://example.invalid/repo?ref=main");
        assert_eq!(
            identity("ssh://git:synthetic_secret@example.invalid/repo"),
            "ssh://git@example.invalid/repo"
        );
        assert_eq!(
            identity("git@example.invalid:repo"),
            "git@example.invalid:repo"
        );
        assert_eq!(
            identity("https://example.invalid/repo"),
            "https://example.invalid/repo"
        );
        let text = redact("failed https://alice:synthetic_secret@example.invalid/repo?token=synthetic_token ; helper echoed synthetic_secret");
        assert!(!text.contains("synthetic_secret") && !text.contains("synthetic_token"));
    }
}
