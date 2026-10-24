// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Dependency edits use parser byte ranges; unrelated source remains byte exact.
use super::*;
use std::ops::Range;

pub fn dependency(raw: &str, name: &str, addition: Option<&str>) -> Result<String, String> {
    let map = loads_versions(raw, "format", &[2, 3, 4])?;
    let escapes = matches!(map.get("format"), Some(WsonValue::Int(2..=4)));
    let mut p = Parser {
        raw,
        pos: 0,
        depth: 0,
        escapes: Some(escapes),
    };
    p.take(b'{')?;
    let mut array = None;
    let mut last = None;
    loop {
        p.space()?;
        if p.peek() == Some(b'}') {
            break;
        }
        let start = p.pos;
        while p
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            p.pos += 1;
        }
        let key = &raw[start..p.pos];
        p.space()?;
        p.pos += 1;
        p.space()?;
        let value_start = p.pos;
        p.value()?;
        if key == "dependencies" {
            array = Some(value_start..p.pos);
        }
        let end = p.pos;
        p.space()?;
        let comma = p.peek() == Some(b',');
        if comma {
            p.pos += 1;
        }
        last = Some((end, comma));
    }
    let root_end = p.pos;
    let Some(range) = array else {
        let text = addition.ok_or_else(|| format!("unknown direct dependency `{name}`"))?;
        let prefix = if last.is_some_and(|(_, comma)| !comma) {
            ","
        } else {
            ""
        };
        let at = last.map_or(root_end, |(end, _)| end);
        let mut result = raw.to_owned();
        if !prefix.is_empty() {
            result.insert_str(at, prefix);
        }
        result.insert_str(
            root_end + prefix.len(),
            &format!("\n    dependencies = [{text}]\n"),
        );
        return Ok(result);
    };
    p.pos = range.start;
    p.take(b'[')?;
    let mut entries: Vec<(Range<usize>, Option<usize>, String)> = Vec::new();
    loop {
        p.space()?;
        if p.peek() == Some(b']') {
            break;
        }
        let start = p.pos;
        let value = p.value()?;
        let end = p.pos;
        let WsonValue::Object(fields) = value else {
            return Err("dependency must be an object".into());
        };
        let Some(WsonValue::String(alias)) = fields.get("name") else {
            return Err("dependency name missing".into());
        };
        p.space()?;
        let comma = (p.peek() == Some(b',')).then_some(p.pos);
        if comma.is_some() {
            p.pos += 1;
        }
        entries.push((start..end, comma, alias.clone()));
    }
    let mut result = raw.to_owned();
    let found = entries.iter().position(|(_, _, alias)| alias == name);
    match (addition, found) {
        (Some(_), Some(_)) => return Err(format!("direct dependency `{name}` already exists")),
        (Some(text), None) => {
            result.insert_str(p.pos, &format!("\n        {text}\n    "));
            if let Some((range, None, _)) = entries.last() { result.insert(range.end, ','); }
        }
        (None, Some(index)) => {
            let (range, comma, _) = &entries[index];
            if let Some(comma) = comma { result.remove(*comma); }
            result.replace_range(range.clone(), "");
            if comma.is_none() && index > 0 {
                if let Some(comma) = entries[index-1].1 { result.remove(comma); }
            }
        }
        (None, None) => return Err(format!("unknown direct dependency `{name}`; transitive dependencies must be edited in their owning package")),
    }
    loads_versions(&result, "format", &[2, 3, 4])?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comments_and_string_syntax_survive() {
        let raw = "{format=4, name=\"app\", // header\n dependencies=[ /*keep*/ {name=\"one\",path=\"a,//b\"} // tail\n ], note=\"[,{}]\"}";
        let added = dependency(raw, "two", Some("{name=\"two\",path=\"../two\"}")).unwrap();
        assert!(added.contains("path=\"a,//b\"}, // tail"));
        let removed = dependency(&added, "one", None).unwrap();
        assert!(removed.contains("/*keep*/"));
        assert!(removed.contains("// tail"));
        assert!(removed.contains("note=\"[,{}]\""));
        assert!(dependency(&removed, "one", None).is_err());
        for input in ["{}", "{name=\"app\"}", "{name=\"app\", // hi\n}"] {
            let output = dependency(input, "one", Some("{name=\"one\",path=\"a\"}")).unwrap();
            assert!(loads_versions(&output, "format", &[2, 3, 4]).is_ok());
        }
    }
}
