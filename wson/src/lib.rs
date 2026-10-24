// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Token-aware WSON boundary shared by manifests and lockfiles.
//! Legacy strings retain their backslashes; only explicitly versioned input
//! decodes JSON escapes. The upstream parser is used for scalar literals only.
pub mod edit;
pub use wson_rs::{WsonMap, WsonValue};

/// Both file reads and in-memory parser entry points enforce this bound.
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

pub fn read_document(path: &std::path::Path) -> Result<String, std::io::Error> {
    use std::io::Read;
    let mut raw = String::new();
    std::fs::File::open(path)?
        .take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_string(&mut raw)?;
    if raw.len() > MAX_DOCUMENT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "WSON document exceeds 8 MiB",
        ));
    }
    Ok(raw)
}

pub fn loads(raw: &str, version_key: &str, escaped_version: i64) -> Result<WsonMap, String> {
    loads_versions(raw, version_key, &[escaped_version])
}

/// Decode escapes only for the explicitly listed format versions.
pub fn loads_versions(
    raw: &str,
    version_key: &str,
    escaped_versions: &[i64],
) -> Result<WsonMap, String> {
    if raw.len() > MAX_DOCUMENT_BYTES {
        return Err("WSON document exceeds 8 MiB".into());
    }
    let mut parser = Parser {
        raw,
        pos: 0,
        depth: 0,
        escapes: None,
    };
    let map = parser.document()?;
    let escaped =
        matches!(map.get(version_key), Some(WsonValue::Int(v)) if escaped_versions.contains(v));
    parser.pos = 0;
    parser.escapes = Some(escaped);
    parser.document()
}

pub fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string cannot fail")
}

/// Convert an already validated WSON subtree to typed machine data. Extended
/// scalar literals are rejected instead of silently changing their meaning.
pub fn json_value(value: &WsonValue) -> Result<serde_json::Value, String> {
    use serde_json::Value;
    Ok(match value {
        WsonValue::Null => Value::Null,
        WsonValue::Bool(value) => Value::Bool(*value),
        WsonValue::Int(value) => (*value).into(),
        WsonValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .ok_or("invalid JSON number")?,
        WsonValue::String(value) => Value::String(value.clone()),
        WsonValue::Array(values) => {
            Value::Array(values.iter().map(json_value).collect::<Result<_, _>>()?)
        }
        WsonValue::Object(values) => Value::Object(
            values
                .iter()
                .map(|(k, v)| Ok((k.clone(), json_value(v)?)))
                .collect::<Result<_, String>>()?,
        ),
        _ => return Err("this field requires ordinary JSON-compatible values".into()),
    })
}

/// Render protocol-shaped JSON data using WSON field names and versioned
/// string escaping. Arbitrary JSON object keys are intentionally rejected.
pub fn render_json(value: &serde_json::Value) -> Result<String, String> {
    use serde_json::Value;
    Ok(match value {
        Value::Object(values) => {
            let fields = values
                .iter()
                .map(|(key, value)| {
                    if key.is_empty()
                        || !key
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    {
                        return Err("invalid WSON field name".into());
                    }
                    Ok(format!("{key} = {}", render_json(value)?))
                })
                .collect::<Result<Vec<_>, String>>()?;
            format!("{{{}}}", fields.join(", "))
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(render_json)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        Value::String(value) => quote(value),
        other => other.to_string(),
    })
}

struct Parser<'a> {
    raw: &'a str,
    pos: usize,
    depth: usize,
    escapes: Option<bool>,
}
impl Parser<'_> {
    fn document(&mut self) -> Result<WsonMap, String> {
        let value = self.value()?;
        self.space()?;
        if self.pos != self.raw.len() {
            return Err(self.error("unexpected trailing input"));
        }
        let WsonValue::Object(map) = value else {
            return Err(self.error("WSON root must be an object"));
        };
        Ok(map)
    }
    fn error(&self, message: &str) -> String {
        let prefix = &self.raw[..self.pos];
        let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        format!("{message} (line {line}, column {column})")
    }
    fn peek(&self) -> Option<u8> {
        self.raw.as_bytes().get(self.pos).copied()
    }
    fn space(&mut self) -> Result<(), String> {
        loop {
            while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
                self.pos += 1;
            }
            let rest = &self.raw[self.pos..];
            if rest.starts_with("//") || rest.starts_with('#') {
                self.pos += rest.find('\n').unwrap_or(rest.len());
            } else if rest.starts_with("/*") {
                let end = rest
                    .find("*/")
                    .ok_or_else(|| self.error("unterminated comment"))?;
                self.pos += end + 2;
            } else {
                return Ok(());
            }
        }
    }
    fn take(&mut self, byte: u8) -> Result<(), String> {
        self.space()?;
        if self.peek() != Some(byte) {
            return Err(self.error(&format!("expected `{}`", byte as char)));
        }
        self.pos += 1;
        Ok(())
    }
    fn value(&mut self) -> Result<WsonValue, String> {
        self.space()?;
        if self.depth >= 128 {
            return Err(self.error("WSON nesting exceeds 128 levels"));
        }
        self.depth += 1;
        let result = self.inner();
        self.depth -= 1;
        result
    }
    fn inner(&mut self) -> Result<WsonValue, String> {
        match self.peek() {
            Some(b'"') => {
                self.pos += 1;
                let start = self.pos;
                let mut escaped = false;
                while let Some(b) = self.peek() {
                    if b == b'"' && !escaped {
                        let text = &self.raw[start..self.pos];
                        let value = if self.escapes == Some(true) {
                            serde_json::from_str(&self.raw[start - 1..=self.pos]).map_err(|e| {
                                self.error(&format!("invalid versioned string escape: {e}"))
                            })?
                        } else {
                            if self.escapes == Some(false) && text.contains(['\n', '\r']) {
                                return Err(self.error("ambiguous multiline legacy string; use an explicitly versioned escaped string"));
                            }
                            text.to_owned()
                        };
                        self.pos += 1;
                        return Ok(WsonValue::String(value));
                    }
                    escaped = b == b'\\' && !escaped;
                    self.pos += self.raw[self.pos..].chars().next().unwrap().len_utf8();
                }
                Err(self.error("unterminated or ambiguous string; use a versioned escaped string"))
            }
            Some(b'{') => {
                self.pos += 1;
                let mut map = WsonMap::new();
                loop {
                    self.space()?;
                    if self.peek() == Some(b'}') {
                        self.pos += 1;
                        return Ok(WsonValue::Object(map));
                    }
                    let start = self.pos;
                    while self
                        .peek()
                        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    {
                        self.pos += 1;
                    }
                    if start == self.pos {
                        return Err(self.error("expected field name"));
                    }
                    let key = self.raw[start..self.pos].to_string();
                    self.space()?;
                    if !matches!(self.peek(), Some(b'=') | Some(b':')) {
                        return Err(self.error("expected `=` or `:`"));
                    }
                    self.pos += 1;
                    let value = self.value()?;
                    if map.insert(key.clone(), value).is_some() {
                        return Err(self.error(&format!("duplicate field `{key}`")));
                    }
                    self.space()?;
                    if self.peek() != Some(b'}') {
                        self.take(b',')?;
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                let mut values = Vec::new();
                loop {
                    self.space()?;
                    if self.peek() == Some(b']') {
                        self.pos += 1;
                        return Ok(WsonValue::Array(values));
                    }
                    values.push(self.value()?);
                    self.space()?;
                    if self.peek() != Some(b']') {
                        self.take(b',')?;
                    }
                }
            }
            _ => {
                let start = self.pos;
                while self.peek().is_some_and(|b| {
                    !b.is_ascii_whitespace() && !matches!(b, b',' | b']' | b'}' | b'#' | b'/')
                }) {
                    self.pos += self.raw[self.pos..].chars().next().unwrap().len_utf8();
                }
                if start == self.pos {
                    return Err(self.error("expected value"));
                }
                let atom = &self.raw[start..self.pos];
                if atom.len() > 1024 || atom.contains(['{', '[', '\"', '=', ':', '\\']) {
                    return Err(self.error("invalid or oversized scalar"));
                }
                if atom.contains('.')
                    && atom
                        .split('.')
                        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                {
                    let parts = atom
                        .split('.')
                        .map(str::parse::<u32>)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| self.error("version component exceeds u32"))?;
                    return Ok(WsonValue::Version(parts));
                }
                let mut result = wson_rs::loads(&format!("{{value={atom}}}"))
                    .map_err(|_| self.error(&format!("invalid scalar `{atom}`")))?;
                result
                    .remove("value")
                    .ok_or_else(|| self.error("missing scalar"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn string_contents_do_not_become_syntax() {
        for text in [
            "https://host/a#b",
            "a,b={x}[y]/*hi*/",
            "__VEX_WSON_URL_SEPARATOR__",
            "__VEX_LOCK_URL_SEPARATOR__",
            "안녕",
        ] {
            let input = format!("{{format=2, value={}}}", quote(text));
            assert!(
                matches!(&loads(&input, "format", 2).unwrap()["value"], WsonValue::String(s) if s == text)
            );
        }
    }
    #[test]
    fn escapes_are_explicitly_versioned() {
        let legacy = loads(r#"{value="C:\tmp\new"}"#, "format", 2).unwrap();
        assert!(matches!(&legacy["value"], WsonValue::String(s) if s == r"C:\tmp\new"));
        for value in ["quote\"slash\\", "\t\n\r", "😀", "\0", r"C:\tmp\new"] {
            let input = format!("{{value={}, format=2}}", quote(value));
            assert!(
                matches!(&loads(&input, "format", 2).unwrap()["value"], WsonValue::String(s) if s == value)
            );
        }
        assert!(loads(r#"{format=2, value="\q"}"#, "format", 2).is_err());
        assert!(loads("{value=1,value=2}", "format", 2)
            .unwrap_err()
            .contains("duplicate"));
    }

    #[test]
    fn multiple_supported_formats_do_not_enable_unknown_format_escapes() {
        for format in [2, 3] {
            let raw = format!(r#"{{format={format},value="line\nnext"}}"#);
            let parsed = loads_versions(&raw, "format", &[2, 3]).unwrap();
            assert!(matches!(&parsed["value"], WsonValue::String(value) if value == "line\nnext"));
        }
        let parsed = loads_versions(r#"{format=4,value="line\nnext"}"#, "format", &[2, 3]).unwrap();
        assert!(matches!(&parsed["value"], WsonValue::String(value) if value == r"line\nnext"));
        // Existing single-version callers retain their exact-version boundary.
        let parsed = loads(r#"{format=3,value="line\nnext"}"#, "format", 2).unwrap();
        assert!(matches!(&parsed["value"], WsonValue::String(value) if value == r"line\nnext"));
    }
}

#[cfg(test)]
mod adversarial_tests {
    use super::*;
    #[test]
    fn bounded_mutation_corpus_never_panics() {
        let seeds = [
            r#"{format=2,name="app",dependencies=[{name="dep",git="https://host/a?x=1#y"}]}"#,
            r#"{version=3,package=[],value="\uD800"}"#,
            r#"{format=1,value="C:\legacy\new"}"#,
            "{a=[[[[[[[[[[[[]]]]]]]]]]]],b=999999999999999999999999999999999999999999}",
            "{/* comment */a=1.0.0,b=NaN,c=inf}",
        ];
        let tokens = [
            "{", "}", "[", "]", "\\", "\"", "/*", "*/", ",", "=", "\0", "한", "💥", "#", "\n",
        ];
        let mut rng = 0x456abcd123u64;
        for seed in seeds {
            for _ in 0..2000 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let mut input = seed.to_string();
                let index = (rng as usize) % (input.len() + 1);
                input.insert_str(index, tokens[(rng >> 32) as usize % tokens.len()]);
                let _ = loads(&input, "format", 2);
                let _ = loads(&input, "version", 3);
            }
        }
    }
    #[test]
    fn document_and_nesting_limits_are_enforced() {
        assert!(loads("{version=1.999999999999999999999.3}", "format", 2)
            .unwrap_err()
            .contains("version component"));
        assert!(loads(&" ".repeat(MAX_DOCUMENT_BYTES + 1), "format", 2).is_err());
        let deep = format!("{{a={}0{}}}", "[".repeat(129), "]".repeat(129));
        assert!(loads(&deep, "format", 2).unwrap_err().contains("nesting"));
    }
}
