// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
pub fn render_new_manifest(project_name: &str, author: &str, is_lib: bool) -> String {
    format!(
        r#"{{
    format = 3,
    name = "{name}",
    version = 0.1.0,
    targets = {targets},
    description = "{name} Project",
    author = "{author}",
    license = "Unknown",
    dependencies = []
}}
"#,
        targets = if is_lib {
            "{ lib = { output = \"object\" } }".to_string()
        } else {
            format!(
                "{{ bins = [{{ name = \"{}\", path = \"src/main.wave\" }}] }}",
                escape_wson_string(project_name)
            )
        },
        name = escape_wson_string(project_name),
        author = escape_wson_string(author)
    )
}

fn escape_wson_string(value: &str) -> String {
    let quoted = wson::quote(value);
    quoted[1..quoted.len() - 1].to_owned()
}
