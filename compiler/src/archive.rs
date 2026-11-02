// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Target-aware static archives. Native/system libraries remain separate inputs.
use diagnostic::Error;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
#[derive(Clone, Debug)]
pub struct ArchivePlan {
    pub output: PathBuf,
    pub program: PathBuf,
    pub format: String,
    pub target: String,
    pub identity: String,
    pub version: String,
    llvm: bool,
}
fn executable(name: &Path) -> Option<PathBuf> {
    let candidates = if name.is_absolute() || name.components().count() > 1 {
        vec![name.into()]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|d| d.join(name))
            .collect()
    };
    for candidate in candidates {
        #[cfg(windows)]
        let candidate = if candidate.extension().is_none() {
            candidate.with_extension("exe")
        } else {
            candidate
        };
        if !candidate.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if fs::metadata(&candidate).ok()?.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return std::env::current_dir().ok().map(|cwd| cwd.join(candidate));
    }
    None
}
impl ArchivePlan {
    pub fn select(
        requested: Option<&Path>,
        compiler: &Path,
        output: PathBuf,
        target: &str,
        format: &str,
    ) -> Result<Self, Error> {
        if !matches!(format, "elf" | "macho" | "coff") {
            return Err(Error::resolution(
                "static output requires an ELF, Mach-O, or COFF target",
            ));
        }
        let name = if format == "coff" {
            "llvm-lib"
        } else {
            "llvm-ar"
        };
        let mut candidates = Vec::new();
        if let Some(requested) = requested {
            candidates.push(requested.into());
        } else {
            if let Some(parent) = compiler.parent() {
                for base in [
                    parent.to_owned(),
                    parent.join("llvm/bin"),
                    parent.join("../llvm/bin"),
                ] {
                    candidates.push(base.join(name));
                }
            }
            candidates.push(PathBuf::from(name));
            if format == "elf" {
                candidates.push(PathBuf::from("ar"));
            }
        }
        let program = candidates
            .iter()
            .find_map(|p| executable(p))
            .ok_or_else(|| {
                Error::environment(format!(
                    "static output requires {name}; install it or configure build.archiver"
                ))
            })?;
        let response = process::output(
            Command::new(&program).arg(if format == "coff" { "/?" } else { "--version" }),
            Duration::from_secs(60),
        )
        .map_err(Error::environment)?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&response.stdout),
            String::from_utf8_lossy(&response.stderr)
        );
        let llvm = text.contains("LLVM");
        let supported = llvm
            || (format == "elf" && text.contains("GNU ar"))
            || (format == "coff" && text.contains("Library Manager"));
        if !response.status.success() || !supported {
            return Err(Error::environment(
                "archiver cannot prove support for the requested deterministic archive format",
            ));
        }
        let identity =
            crate::identity::file_digest(&program.canonicalize().map_err(Error::environment)?)?;
        Ok(Self {
            output,
            program,
            format: format.into(),
            target: target.into(),
            identity,
            version: text.trim().into(),
            llvm,
        })
    }
    pub fn value(&self, members: &[PathBuf]) -> Value {
        json!({"program":self.program,"sha256":self.identity,"version":self.version,"format":self.format,"target":self.target,"output":self.output,"members":members,"deterministic":true,"symbol_index":true})
    }
    pub fn execute(&self, members: &[PathBuf], output: &Path) -> Result<(), Error> {
        if members.is_empty() {
            return Err(Error::compiler("static output has no Wave objects"));
        }
        if crate::identity::file_digest(&self.program.canonicalize().map_err(Error::environment)?)?
            != self.identity
        {
            return Err(Error::environment("archiver changed after planning"));
        }
        state::ensure_tree(
            output
                .parent()
                .ok_or_else(|| Error::internal("archive parent is missing"))?,
        )
        .map_err(Error::environment)?;
        let cwd = std::env::current_dir().map_err(Error::environment)?;
        let output = cwd.join(output);
        let members_absolute = members.iter().map(|p| cwd.join(p)).collect::<Vec<_>>();
        let mut common = members_absolute[0]
            .parent()
            .ok_or_else(|| Error::internal("archive member lacks parent"))?;
        while members_absolute.iter().any(|p| !p.starts_with(common)) {
            common = common
                .parent()
                .ok_or_else(|| Error::internal("archive members have no common directory"))?;
        }
        let arguments = members_absolute
            .iter()
            .map(|p| {
                p.strip_prefix(common)
                    .map(Path::to_path_buf)
                    .map_err(Error::internal)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut command = Command::new(&self.program);
        command.current_dir(common);
        if self.format == "coff" {
            if !self.llvm {
                command.arg("/Brepro");
            }
            command.arg(format!("/OUT:{}", output.display()));
        } else {
            if self.llvm {
                command.arg(if self.format == "macho" {
                    "--format=darwin"
                } else {
                    "--format=gnu"
                });
            }
            command.arg("rcsD").arg(&output);
        }
        #[cfg(unix)]
        for member in members {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(member, fs::Permissions::from_mode(0o644))
                .map_err(Error::environment)?;
        }
        command.args(arguments);
        let status = process::status(&mut command, Some(Duration::from_secs(300)), false)
            .map_err(Error::environment)?;
        if !status.success() {
            return Err(Error::compiler(format!("archiver failed with {status}")));
        }
        self.verify(members, &output)
    }
    fn verify(&self, members: &[PathBuf], output: &Path) -> Result<(), Error> {
        state::reject_link(output).map_err(Error::environment)?;
        let data = fs::read(output).map_err(Error::environment)?;
        if !data.starts_with(b"!<arch>\n") {
            return Err(Error::compiler(
                "archiver produced an invalid or thin archive",
            ));
        }
        let mut remaining = std::collections::BTreeMap::new();
        for member in members {
            *remaining
                .entry(crate::identity::file_digest(member)?)
                .or_insert(0usize) += 1;
        }
        let mut offset = 8;
        let mut index = false;
        while offset < data.len() {
            let header = data
                .get(offset..offset + 60)
                .ok_or_else(|| Error::compiler("truncated archive member"))?;
            if &header[58..60] != b"`\n" {
                return Err(Error::compiler("invalid archive member header"));
            }
            let number = |part: &[u8]| {
                std::str::from_utf8(part)
                    .ok()
                    .and_then(|s| s.trim().parse::<usize>().ok())
            };
            let size = number(&header[48..58])
                .ok_or_else(|| Error::compiler("invalid archive member length"))?;
            let end = offset
                .checked_add(60)
                .and_then(|n| n.checked_add(size))
                .ok_or_else(|| Error::compiler("archive length overflow"))?;
            if number(&header[16..28]).unwrap_or(0) != 0 {
                return Err(Error::compiler(
                    "archiver did not produce deterministic member timestamps",
                ));
            }
            let mut body = data
                .get(offset + 60..end)
                .ok_or_else(|| Error::compiler("truncated archive member bytes"))?;
            let name = std::str::from_utf8(&header[..16])
                .map_err(Error::compiler)?
                .trim();
            let mut symbol = name == "/" || name == "/SYM64/" || name.starts_with("__.SYMDEF");
            if let Some(n) = name.strip_prefix("#1/") {
                let n = n.trim().parse::<usize>().map_err(Error::compiler)?;
                let long = body
                    .get(..n)
                    .ok_or_else(|| Error::compiler("invalid BSD archive member name"))?;
                symbol = long.starts_with(b"__.SYMDEF");
                body = body
                    .get(n..)
                    .ok_or_else(|| Error::compiler("invalid BSD member payload"))?;
            }
            if symbol {
                index = true;
            } else if name != "//" {
                let valid = match self.format.as_str() {
                    "elf" => {
                        let machine = if self.target.starts_with("x86_64") {
                            62u16
                        } else if self.target.starts_with("aarch64") {
                            183
                        } else if self.target.starts_with("riscv64") {
                            243
                        } else if self.target.starts_with("loongarch64") {
                            258
                        } else {
                            0
                        };
                        body.starts_with(b"\x7fELF")
                            && body.get(4..6) == Some(&[2, 1][..])
                            && body.get(18..20) == Some(&machine.to_le_bytes()[..])
                    }
                    "macho" => {
                        let cpu = if self.target.starts_with("aarch64") {
                            0x0100000cu32
                        } else {
                            0x01000007u32
                        };
                        body.starts_with(b"\xcf\xfa\xed\xfe")
                            && body.get(4..8) == Some(&cpu.to_le_bytes()[..])
                    }
                    "coff" => body.starts_with(if self.target.starts_with("aarch64") {
                        b"\x64\xaa"
                    } else {
                        b"\x64\x86"
                    }),
                    _ => false,
                };
                if !valid {
                    return Err(Error::compiler(
                        "archive contains an object for the wrong target format",
                    ));
                }
                use sha2::Digest;
                let digest = sha2::Sha256::digest(body)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                let count = remaining
                    .get_mut(&digest)
                    .filter(|n| **n > 0)
                    .ok_or_else(|| {
                        Error::compiler("archive contains an unexpected or duplicate object")
                    })?;
                *count -= 1;
            }
            offset = end + (end % 2);
        }
        if !index || remaining.values().any(|n| *n != 0) {
            return Err(Error::compiler(
                "archive lacks its complete Wave objects or symbol index",
            ));
        }
        Ok(())
    }
}
