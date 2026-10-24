// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Staging never changes a live checkout. The durable journal is installed only
/// after the entire graph is valid, immediately before publishing any checkout.
pub(crate) struct Transaction {
    root: PathBuf,
    directory: PathBuf,
    pub stage: PathBuf,
    names: RefCell<BTreeSet<String>>,
    journaled: bool,
}

impl Transaction {
    pub fn path(&self, name: &str) -> PathBuf {
        if self.names.borrow().contains(name) {
            self.stage.join(name)
        } else {
            self.root.join(".vex/deps").join(name)
        }
    }

    pub fn prepare_from(&self, name: &str, previous: &str) -> Result<(), String> {
        self.prepare_source(name, Some(previous))
    }

    pub fn new(root: &Path) -> Result<Self, String> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let directory = root
            .join(".vex/transactions")
            .join(format!("{}-{stamp}", std::process::id()));
        let stage = directory.join("deps");
        Ok(Self {
            root: root.to_owned(),
            directory,
            stage,
            names: RefCell::new(BTreeSet::new()),
            journaled: false,
        })
    }

    pub fn prepare(&self, name: &str) -> Result<(), String> {
        self.prepare_source(name, Some(name))
    }

    pub fn prepare_fresh(&self, name: &str) -> Result<(), String> {
        self.prepare_source(name, None)
    }

    fn prepare_source(&self, name: &str, previous: Option<&str>) -> Result<(), String> {
        if self.names.borrow().contains(name) {
            return Ok(());
        }
        valid_name(name)?;
        if self.names.borrow().is_empty() {
            state::ensure_dir(self.directory.parent().unwrap())?;
            fs::create_dir(&self.directory).map_err(err)?;
            fs::create_dir(&self.stage).map_err(err)?;
            fs::create_dir(self.directory.join("backup")).map_err(err)?;
        }
        if let Some(previous) = previous {
            valid_name(previous)?;
        }
        let live = self.root.join(".vex/deps").join(previous.unwrap_or(name));
        if previous.is_some() && fs::symlink_metadata(&live).is_ok() {
            crate::paths::validate_managed_checkout_path(&live).map_err(|e| e.to_string())?;
            if !live.join(".git").is_dir() {
                return Err(format!(
                    "managed dependency path `{}` exists but is not a Git checkout",
                    live.display()
                ));
            }
            crate::git::reject_dirty_checkout(&live, name).map_err(|e| e.to_string())?;
            reject_git_metadata_links(&live.join(".git"))?;
            copy_tree(&live, &self.stage.join(name))?;
        }
        self.names.borrow_mut().insert(name.to_owned());
        Ok(())
    }

    pub fn publish(mut self, new_lock: Option<String>) -> Result<(), String> {
        if self.names.borrow().is_empty() {
            if let Some(content) = new_lock {
                atomic_text(&self.root.join("vex.lock"), &content)?;
            }
            return Ok(());
        }
        let live_root = self.root.join(".vex/deps");
        state::ensure_dir(&live_root)?;
        // Recheck dirty data at the last possible point before moving anything.
        let mut entries = Vec::new();
        for name in self.names.borrow().iter() {
            let live = live_root.join(name);
            state::reject_link(&live)?;
            if live.exists() {
                crate::git::reject_dirty_checkout(&live, name).map_err(|e| e.to_string())?;
            }
            if !self.stage.join(name).is_dir() {
                return Err(format!("candidate checkout `{name}` is missing"));
            }
            sync_tree(&self.stage.join(name))?;
            entries.push(json!({"name":name,"had_old":live.exists()}));
        }
        // Persist the newly created directory ancestry before the journal can
        // reference it and before it receives any irreplaceable live checkout.
        for directory in [
            self.stage.as_path(),
            &self.directory.join("backup"),
            self.directory.as_path(),
            self.directory.parent().unwrap(),
            live_root.as_path(),
            &self.root.join(".vex"),
            self.root.as_path(),
        ] {
            state::sync_dir(directory).map_err(err)?;
        }
        let old_lock = read_optional(&self.root.join("vex.lock"))?
            .as_deref()
            .map(fingerprint);
        let record = json!({"version":2,"directory":self.directory.file_name().unwrap().to_str().unwrap(),
            "entries":entries,"old_lock":old_lock,"new_lock":new_lock,"committed":false});
        self.journaled = true;
        atomic_text(&journal(&self.root), &record.to_string())?;
        let result = self.publish_record(record);
        if let Err(error) = result {
            return match recover(&self.root, false) {
                Ok(()) => Err(format!(
                    "dependency publication failed; recovered the transaction: {error}"
                )),
                Err(recovery) => Err(format!(
                    "dependency publication failed: {error}\nrecovery pending: {recovery}"
                )),
            };
        }
        Ok(())
    }

    fn publish_record(&self, mut record: Value) -> Result<(), String> {
        for name in self.names.borrow().iter() {
            let live = self.root.join(".vex/deps").join(name);
            if live.exists() {
                rename(&live, &self.directory.join("backup").join(name))?;
            }
            fault("after-backup")?;
            rename(&self.stage.join(name), &live)?;
            fault("after-checkout")?;
        }
        fault("before-lockfile")?;
        if let Some(content) = record["new_lock"].as_str() {
            atomic_text(&self.root.join("vex.lock"), content)?;
        }
        fault("after-lockfile")?;
        record["committed"] = json!(true);
        atomic_text(&journal(&self.root), &record.to_string())?;
        finish(&self.root, &self.directory)
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.journaled {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

pub(crate) fn recover(root: &Path, dry_run: bool) -> Result<(), String> {
    let path = journal(root);
    state::reject_link(&path)?;
    let Some(raw) = read_optional(&path)? else {
        return Ok(());
    };
    if dry_run {
        return Err("dependency recovery is pending; dry-run cannot repair state\nhelp: run `vex fetch` first".into());
    }
    let record: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("invalid recovery journal: {e}; preserve .vex for manual recovery"))?;
    if !matches!(record["version"].as_u64(), Some(1 | 2)) {
        return Err("unsupported recovery journal version".into());
    }
    let id = record["directory"]
        .as_str()
        .ok_or("missing journal directory")?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err("invalid journal directory".into());
    }
    let directory = root.join(".vex/transactions").join(id);
    for p in [
        root.join(".vex/transactions"),
        directory.clone(),
        directory.join("deps"),
        directory.join("backup"),
        root.join(".vex/deps"),
    ] {
        state::reject_link(&p)?;
    }
    let old = optional_text(&record, "old_lock")?;
    let new = optional_text(&record, "new_lock")?;
    let current = read_optional(&root.join("vex.lock"))?;
    let committed_flag = record["committed"]
        .as_bool()
        .ok_or("missing journal commit flag")?;
    let current_identity = if record["version"].as_u64() == Some(2) {
        current.as_deref().map(fingerprint)
    } else {
        current.clone()
    };
    let old_matches = current_identity.as_deref() == old;
    let new_matches = new.is_some() && current.as_deref() == new;
    let committed = committed_flag || (!old_matches && new_matches);
    if !old_matches && !new_matches {
        return Err("vex.lock differs from both transaction states; preserve .vex and restore the expected lockfile before recovery".into());
    }
    if committed_flag
        && !(if new.is_some() {
            new_matches
        } else {
            old_matches
        })
    {
        return Err("committed journal disagrees with vex.lock".into());
    }
    let entries = record["entries"]
        .as_array()
        .ok_or("missing journal entries")?;
    let mut names = BTreeSet::new();
    // Validate the complete record before the first recovery mutation.
    for entry in entries {
        let name = entry["name"]
            .as_str()
            .ok_or("missing journal package name")?;
        valid_name(name)?;
        if !names.insert(name) {
            return Err("duplicate journal package name".into());
        }
        entry["had_old"]
            .as_bool()
            .ok_or("missing journal backup flag")?;
        for p in [
            root.join(".vex/deps").join(name),
            directory.join("backup").join(name),
            directory.join("deps").join(name),
        ] {
            state::reject_link(&p)?;
        }
        let live = root.join(".vex/deps").join(name);
        let backup = directory.join("backup").join(name);
        let stage = directory.join("deps").join(name);
        if committed && (!live.is_dir() || stage.exists()) {
            return Err(format!(
                "incomplete committed checkout `{name}`; refusing to discard recovery journal"
            ));
        }
        if !committed
            && entry["had_old"].as_bool() == Some(true)
            && !live.exists()
            && !backup.exists()
        {
            return Err(format!("missing original checkout and backup for `{name}`"));
        }
        if !committed
            && live.exists()
            && stage.exists()
            && (backup.exists() || entry["had_old"].as_bool() == Some(false))
        {
            return Err(format!("ambiguous recovery state for `{name}`"));
        }
    }
    if !committed {
        for entry in entries.iter().rev() {
            let name = entry["name"].as_str().unwrap();
            let live = root.join(".vex/deps").join(name);
            let backup = directory.join("backup").join(name);
            let stage = directory.join("deps").join(name);
            if backup.exists() {
                if live.exists() {
                    crate::git::reject_dirty_checkout(&live, name).map_err(|e| e.to_string())?;
                    // Preserve the failed candidate rather than deleting a tree
                    // whose contents may have been inspected by the user.
                    if stage.exists() {
                        return Err(format!("ambiguous recovery state for `{name}`"));
                    }
                    rename(&live, &stage)?;
                }
                rename(&backup, &live)?;
            } else if !entry["had_old"].as_bool().unwrap() && live.exists() && !stage.exists() {
                crate::git::reject_dirty_checkout(&live, name).map_err(|e| e.to_string())?;
                rename(&live, &stage)?;
            }
        }
    }
    // Backups/candidates are retained; no automatic checkout GC. Removing the
    // journal is safe only after all rollback steps or the lockfile commit.
    finish(root, &directory)
}

fn finish(root: &Path, _directory: &Path) -> Result<(), String> {
    fs::remove_file(journal(root)).map_err(err)?;
    state::sync_dir(&root.join(".vex")).map_err(err)
}
fn journal(root: &Path) -> PathBuf {
    root.join(".vex/transaction.json")
}
fn err(e: std::io::Error) -> String {
    e.to_string()
}
fn optional_text<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match value.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        _ => Err(format!("invalid journal `{key}`")),
    }
}
fn read_optional(path: &Path) -> Result<Option<String>, String> {
    state::reject_link(path)?;
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(err(e)),
    }
}
fn valid_name(name: &str) -> Result<(), String> {
    let mut chars = name.bytes();
    if !chars
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        || !chars.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("invalid transaction package name".into());
    }
    Ok(())
}
pub(crate) fn unique_dir(parent: &Path) -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    loop {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = parent.join(format!(
            "{}-{time}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(err(e)),
        }
    }
}
fn atomic_text(path: &Path, content: &str) -> Result<(), String> {
    state::reject_link(path)?;
    let parent = path.parent().ok_or("missing publication parent")?;
    let temporary_dir = unique_dir(parent)?;
    let temporary = temporary_dir.join("value");
    let result = (|| {
        let mut file = fs::File::create_new(&temporary).map_err(err)?;
        file.write_all(content.as_bytes()).map_err(err)?;
        file.sync_all().map_err(err)?;
        drop(file);
        rename(&temporary, path)
    })();
    let _ = fs::remove_dir_all(&temporary_dir);
    result
}
fn rename(from: &Path, to: &Path) -> Result<(), String> {
    state::atomic_rename(from, to).map_err(err)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(from).map_err(err)?;
    if meta.file_type().is_symlink() {
        let link = fs::read_link(from).map_err(err)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, to).map_err(err)?;
        #[cfg(windows)]
        {
            if from.is_dir() {
                std::os::windows::fs::symlink_dir(link, to).map_err(err)?;
            } else {
                std::os::windows::fs::symlink_file(link, to).map_err(err)?;
            }
        }
    } else if meta.is_dir() {
        fs::create_dir(to).map_err(err)?;
        for entry in fs::read_dir(from).map_err(err)? {
            let entry = entry.map_err(err)?;
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
        // Preserve user directory permissions as well as file contents. Set
        // them after copying children so restrictive modes do not interrupt
        // construction of the unpublished candidate.
        fs::set_permissions(to, meta.permissions()).map_err(err)?;
    } else if meta.is_file() {
        fs::copy(from, to).map_err(err)?;
    } else {
        return Err(format!("unsupported file in checkout: {}", from.display()));
    }
    Ok(())
}
pub(crate) fn sync_tree(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(err)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path).map_err(err)? {
            sync_tree(&entry.map_err(err)?.path())?;
        }
        state::sync_dir(path).map_err(err)
    } else {
        sync_file(path)
    }
}

fn sync_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        fs::File::open(path).and_then(|f| f.sync_all()).map_err(err)
    }
    #[cfg(windows)]
    {
        let original = fs::metadata(path).map_err(err)?.permissions();
        if original.readonly() {
            let mut writable = original.clone();
            // Windows clears FILE_ATTRIBUTE_READONLY, not Unix mode bits.
            // Restore the original attribute below after FlushFileBuffers.
            #[allow(clippy::permissions_set_readonly_false)]
            writable.set_readonly(false);
            fs::set_permissions(path, writable).map_err(err)?;
        }
        let result = fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|f| f.sync_all());
        fs::set_permissions(path, original).map_err(err)?;
        result.map_err(err)
    }
}

fn fingerprint(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
thread_local! { static FAULT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) }; }
fn fault(_point: &str) -> Result<(), String> {
    #[cfg(test)]
    if FAULT.with(|f| f.get() == Some(_point)) {
        return Err(format!("injected {_point} failure"));
    }
    Ok(())
}

pub(crate) fn reject_git_metadata_links(path: &Path) -> Result<(), String> {
    state::reject_link(path)?;
    if path.is_dir() {
        for entry in fs::read_dir(path).map_err(err)? {
            reject_git_metadata_links(&entry.map_err(err)?.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = unique_dir(&std::env::temp_dir()).unwrap();
            fs::create_dir_all(path.join(".vex/deps/pkg")).unwrap();
            let package = path.join(".vex/deps/pkg");
            git(&package, &["init", "-q"]);
            fs::write(package.join("value"), "old").unwrap();
            commit(&package);
            fs::write(path.join("vex.lock"), "old-lock").unwrap();
            Self(path)
        }
        fn candidate(&self) -> Transaction {
            let t = Transaction::new(&self.0).unwrap();
            t.prepare("pkg").unwrap();
            fs::write(t.stage.join("pkg/value"), "new").unwrap();
            commit(&t.stage.join("pkg"));
            t
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn git(path: &Path, args: &[&str]) {
        let output = crate::git::command_in(path).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn commit(path: &Path) {
        git(path, &["add", "."]);
        git(
            path,
            &[
                "-c",
                "user.name=Vex Test",
                "-c",
                "user.email=vex@example.invalid",
                "commit",
                "-qm",
                "snapshot",
            ],
        );
    }
    #[test]
    fn publication_failures_recover_on_both_sides_of_the_commit_point() {
        for new_lock in [None, Some("new-lock")] {
            for point in [
                "after-backup",
                "after-checkout",
                "before-lockfile",
                "after-lockfile",
            ] {
                let f = Fixture::new();
                let candidate = f.candidate();
                FAULT.with(|f| f.set(Some(point)));
                let error = candidate.publish(new_lock.map(str::to_owned)).unwrap_err();
                FAULT.with(|f| f.set(None));
                assert!(error.contains("injected"), "{error}");
                let committed = new_lock.is_some() && point == "after-lockfile";
                assert_eq!(
                    fs::read_to_string(f.0.join("vex.lock")).unwrap(),
                    if committed { "new-lock" } else { "old-lock" }
                );
                assert_eq!(
                    fs::read_to_string(f.0.join(".vex/deps/pkg/value")).unwrap(),
                    if committed { "new" } else { "old" }
                );
                assert!(!journal(&f.0).exists());
                recover(&f.0, false).unwrap();
            }
        }
    }
    #[test]
    fn dry_run_does_not_recover_and_restart_rollback_is_idempotent() {
        let f = Fixture::new();
        let mut t = f.candidate();
        let record = json!({"version":1,"directory":t.directory.file_name().unwrap().to_str().unwrap(),
            "entries":[{"name":"pkg","had_old":true}],"old_lock":"old-lock","new_lock":"new-lock","committed":false});
        atomic_text(&journal(&f.0), &record.to_string()).unwrap();
        t.journaled = true;
        rename(&f.0.join(".vex/deps/pkg"), &t.directory.join("backup/pkg")).unwrap();
        assert!(recover(&f.0, true).unwrap_err().contains("dry-run"));
        assert!(!f.0.join(".vex/deps/pkg").exists());
        drop(t);
        recover(&f.0, false).unwrap();
        recover(&f.0, false).unwrap();
        assert_eq!(
            fs::read_to_string(f.0.join(".vex/deps/pkg/value")).unwrap(),
            "old"
        );
    }
    #[test]
    fn recovery_preserves_dirty_candidate_and_keeps_journal() {
        let f = Fixture::new();
        let mut t = f.candidate();
        let record = json!({"version":1,"directory":t.directory.file_name().unwrap().to_str().unwrap(),
            "entries":[{"name":"pkg","had_old":true}],"old_lock":"old-lock","new_lock":"new-lock","committed":false});
        atomic_text(&journal(&f.0), &record.to_string()).unwrap();
        t.journaled = true;
        rename(&f.0.join(".vex/deps/pkg"), &t.directory.join("backup/pkg")).unwrap();
        rename(&t.stage.join("pkg"), &f.0.join(".vex/deps/pkg")).unwrap();
        fs::write(f.0.join(".vex/deps/pkg/user-work"), "keep me").unwrap();
        assert!(recover(&f.0, false).unwrap_err().contains("local changes"));
        assert_eq!(
            fs::read_to_string(f.0.join(".vex/deps/pkg/user-work")).unwrap(),
            "keep me"
        );
        assert!(journal(&f.0).is_file());
    }

    #[test]
    fn recovery_rejects_missing_lock_when_transaction_preserves_it() {
        let f = Fixture::new();
        let mut t = f.candidate();
        let record = json!({"version":1,"directory":t.directory.file_name().unwrap().to_str().unwrap(),
            "entries":[{"name":"pkg","had_old":true}],"old_lock":"old-lock","new_lock":null,"committed":false});
        atomic_text(&journal(&f.0), &record.to_string()).unwrap();
        t.journaled = true;
        rename(&f.0.join(".vex/deps/pkg"), &t.directory.join("backup/pkg")).unwrap();
        fs::remove_file(f.0.join("vex.lock")).unwrap();
        assert!(recover(&f.0, false).unwrap_err().contains("differs"));
        assert!(journal(&f.0).is_file());
        assert!(t.directory.join("backup/pkg").is_dir());
        assert!(!f.0.join(".vex/deps/pkg").exists());
        fs::write(f.0.join("vex.lock"), "old-lock").unwrap();
        recover(&f.0, false).unwrap();
        assert_eq!(
            fs::read_to_string(f.0.join(".vex/deps/pkg/value")).unwrap(),
            "old"
        );
    }
}
