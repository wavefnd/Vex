//! An opt-in event stream separate from compiler and runtime stdio.
use diagnostic::Error;
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

pub struct Messages {
    file: Option<File>,
    command: String,
    sequence: u64,
    failed: bool,
}
impl Messages {
    pub fn open(path: Option<&Path>, command: &str, dry_run: bool) -> Result<Self, Error> {
        let file = if let Some(path) = path {
            if dry_run {
                match fs::symlink_metadata(path) {
                    Ok(_) => {
                        return Err(Error::environment(
                            "message file already exists; refusing to overwrite it",
                        ))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::environment(e)),
                }
                // Dry-run validates the destination without creating a file or directory.
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                if !parent.is_dir() {
                    return Err(Error::environment(
                        "message file parent must be an existing directory",
                    ));
                }
                None
            } else {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                Some(options.open(path).map_err(|e| {
                    Error::environment(format!(
                        "cannot create message file: {e}; existing files are never overwritten"
                    ))
                })?)
            }
        } else {
            None
        };
        Ok(Self {
            file,
            command: command.to_owned(),
            sequence: 0,
            failed: false,
        })
    }

    pub fn enabled(&self) -> bool {
        self.file.is_some()
    }

    pub fn emit(&mut self, mut value: Value) -> Result<(), Error> {
        if self.failed {
            return Err(Error::environment("message file is no longer writable"));
        }
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        self.sequence += 1;
        value["schema_version"] = json!(1);
        value["sequence"] = json!(self.sequence);
        value["command"] = json!(source::redact(&self.command));
        fn redact(value: &mut Value) {
            match value {
                Value::String(text) => *text = source::redact(text),
                Value::Array(items) => items.iter_mut().for_each(redact),
                Value::Object(fields) => fields.values_mut().for_each(redact),
                _ => {}
            }
        }
        redact(&mut value);
        let mut bytes = serde_json::to_vec(&value).map_err(Error::internal)?;
        bytes.push(b'\n');
        #[cfg(debug_assertions)]
        let injected = std::env::var("VEX_TEST_MESSAGE_FAIL_AT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            == Some(self.sequence)
            || std::env::var("VEX_TEST_MESSAGE_FAIL_EVENT")
                .ok()
                .is_some_and(|event| {
                    value["event"] == event
                        || (value["event"] == "status" && value["phase"] == event)
                });
        #[cfg(not(debug_assertions))]
        let injected = false;
        let result = if injected {
            Err(std::io::Error::other("injected message write failure"))
        } else {
            file.write_all(&bytes).and_then(|()| file.flush())
        };
        if let Err(error) = result {
            self.failed = true;
            return Err(Error::environment(format!(
                "cannot write message file: {error}; report is incomplete"
            )));
        }
        Ok(())
    }
    pub fn status(&mut self, phase: &str) -> Result<(), Error> {
        self.emit(json!({"event":"status", "phase":phase}))
    }
    pub fn diagnostic(&mut self, error: &Error) -> Result<(), Error> {
        let mut contexts = Vec::new();
        let mut causes = Vec::new();
        let mut current = Some(error);
        while let Some(item) = current {
            contexts.extend(
                item.context_fields
                    .iter()
                    .map(|(key, value)| json!({"key":key,"value":source::redact(value)})),
            );
            if !std::ptr::eq(item, error) {
                causes.push(source::redact(item.as_ref()));
            }
            current = item.cause.as_deref();
        }
        self.emit(json!({"context":contexts,"causes":causes,"event":"diagnostic", "severity":"error", "origin":"vex", "category":error.category.name(), "message":source::redact(error.as_ref())}))
    }
}
