use diagnostic::Error;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One selection per Vex command. Capability results never outlive this process.
pub struct Compiler {
    pub(crate) path: PathBuf,
    targets: Option<Vec<String>>,
    version: Option<String>,
    specs: BTreeMap<Option<String>, serde_json::Value>,
}

impl Compiler {
    /// The command-local selected executable, before entering a project directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn select() -> Result<Self, Error> {
        let cwd = std::env::current_dir().map_err(Error::environment)?;
        let find = |name: &std::ffi::OsStr| {
            std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths)
                    .map(|p| cwd.join(p).join(name))
                    .find_map(|p| {
                        if p.is_file() {
                            return Some(p);
                        }
                        #[cfg(windows)]
                        if p.extension().is_none() {
                            let executable = p.with_extension("exe");
                            if executable.is_file() {
                                return Some(executable);
                            }
                        }
                        None
                    })
            })
        };
        let path = if let Some(value) = std::env::var_os("VEX_WAVEC") {
            if value.is_empty() || value.to_str().is_some_and(|s| s.trim().is_empty()) {
                return Err(Error::environment("VEX_WAVEC is explicitly empty; unset it to search PATH or provide a compiler path"));
            }
            let path = PathBuf::from(value);
            if path.components().count() == 1 && !path.is_absolute() {
                find(path.as_os_str()).unwrap_or_else(|| cwd.join(path))
            } else {
                cwd.join(path)
            }
        } else {
            let name = if cfg!(windows) { "wavec.exe" } else { "wavec" };
            find(std::ffi::OsStr::new(name))
                .or_else(toolchain::managed_wavec)
                .map(|p| cwd.join(p))
                .ok_or_else(|| {
                    Error::environment(
                        "could not find wavec; install it or set VEX_WAVEC=/path/to/wavec",
                    )
                })?
        };
        Ok(Self {
            path,
            targets: None,
            version: None,
            specs: BTreeMap::new(),
        })
    }

    pub fn validate_version(&mut self, required: Option<&str>) -> Result<(), Error> {
        let Some(required) = required else {
            return Ok(());
        };
        let version = self.version()?.to_owned();
        if version != required {
            return Err(Error::resolution(format!(
                "compiler version mismatch: project requires {required}, selected wavec is {} at {}\nhelp: install the required compiler and select it with VEX_WAVEC; capability/schema checks remain independent",
                version, self.path.display())));
        }
        Ok(())
    }

    pub fn version(&mut self) -> Result<&str, Error> {
        if self.version.is_none() {
            let output = process::output(
                Command::new(&self.path)
                    .arg("--version")
                    .env("NO_COLOR", "1"),
                Duration::from_secs(60),
            )
            .map_err(Error::environment)?;
            if !output.status.success() {
                return Err(Error::compiler(format!(
                    "cannot query compiler version at {}",
                    self.path.display()
                )));
            }
            let text = String::from_utf8(output.stdout).map_err(Error::compiler)?;
            let tokens: Vec<_> = text.split_whitespace().collect();
            if tokens.len() < 2 || tokens[0] != "wavec" {
                return Err(Error::compiler("malformed wavec version response"));
            }
            self.version = Some(tokens[1].to_owned());
        }
        Ok(self.version.as_deref().unwrap())
    }

    pub fn validate_target(&mut self, target: &str) -> Result<(), Error> {
        let supported = self.supported_targets()?;
        if !supported.iter().any(|available| available == target) {
            let available = supported.join(", ");
            return Err(Error::resolution(format!(
                "unsupported target `{target}` for wavec `{}`\navailable targets: {available}",
                self.path.display(),
            )));
        }
        Ok(())
    }

    /// Target metadata is cached only for this invocation, just like the target list.
    pub fn target_spec(&mut self, target: Option<&str>) -> Result<serde_json::Value, Error> {
        let key = target.map(str::to_owned);
        if let Some(value) = self.specs.get(&key) {
            return Ok(value.clone());
        }
        let mut command = Command::new(&self.path);
        command.args(["print", "target-spec", "--format=json"]);
        if let Some(target) = target {
            self.validate_target(target)?;
            command.arg(format!("--target={target}"));
        }
        let output =
            process::output(&mut command, Duration::from_secs(60)).map_err(Error::environment)?;
        if !output.status.success() {
            return Err(super::invocation::compiler_error(
                output.status,
                format!(
                    "wavec target-spec query failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            ));
        }
        let spec: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| Error::compiler(format!("invalid wavec target-spec JSON: {e}")))?;
        let triple = spec["triple"]
            .as_str()
            .ok_or_else(|| Error::compiler("target-spec is missing triple"))?;
        if triple.is_empty()
            || !triple
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            || triple == "."
            || triple == ".."
            || target.is_some_and(|expected| expected != triple)
            || spec["supported"] != true
            || spec["object_format"].as_str().is_none()
            || spec["os"].as_str().is_none()
        {
            return Err(Error::compiler(
                "invalid or inconsistent compiler target-spec",
            ));
        }
        self.specs.insert(key, spec.clone());
        Ok(spec)
    }

    /// Query once per invocation; doctor uses the same validated capability data
    /// as normal builds without resolving dependencies or creating project state.
    pub fn supported_targets(&mut self) -> Result<&[String], Error> {
        if self.targets.is_none() {
            let output = process::output(
                Command::new(&self.path).args(["print", "supported-targets", "--format=json"]),
                Duration::from_secs(60),
            )
            .map_err(|e| {
                Error::environment(format!("cannot query wavec `{}`: {e}", self.path.display()))
            })?;
            if !output.status.success() {
                return Err(super::invocation::compiler_error(
                    output.status,
                    format!(
                        "wavec capability query failed using `{}`: {}",
                        self.path.display(),
                        String::from_utf8_lossy(&output.stderr)
                    ),
                ));
            }
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|e| {
                Error::compiler(format!("invalid wavec supported-targets JSON: {e}"))
            })?;
            let mut targets =
                super::plan::string_array(&value, "supported-targets").map_err(Error::compiler)?;
            if targets.is_empty() || targets.iter().any(|s| s.trim().is_empty()) {
                return Err(Error::compiler(
                    "wavec supported-targets must contain nonempty target names",
                ));
            }
            targets.sort();
            targets.dedup();
            self.targets = Some(targets);
        }
        Ok(self.targets.as_deref().unwrap())
    }
}
