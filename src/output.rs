//! Batch input protection and transactional output publication.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail, ensure};
use same_file::Handle;
use tempfile::{NamedTempFile, TempDir};

const MAX_NAME_ATTEMPTS: usize = 10_000;

#[derive(Default)]
struct ProtectedInputs {
    paths: HashSet<OsString>,
    // Keep handles alive so file IDs cannot be recycled during the batch.
    files: HashSet<Handle>,
    // Encoders run concurrently; serialize publication so filesystem aliases
    // missed by name folding are rechecked after the preceding commit.
    publication: Mutex<()>,
}

impl ProtectedInputs {
    fn contains(&self, path: &Path) -> Result<bool> {
        if self.paths.contains(&path_key(path)) {
            return Ok(true);
        }
        match Handle::from_path(path) {
            Ok(file) => Ok(self.files.contains(&file)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).context("cannot check destination file identity"),
        }
    }
}

/// One planner per batch, including inputs belonging to completed rows.
pub struct OutputPlanner {
    inputs: Arc<ProtectedInputs>,
    reserved_paths: HashSet<OsString>,
}

impl OutputPlanner {
    pub fn new<'a>(inputs: impl IntoIterator<Item = &'a Path>) -> Result<Self> {
        let mut protected = ProtectedInputs::default();
        for input in inputs {
            let parent = parent_directory(input)
                .canonicalize()
                .with_context(|| format!("cannot resolve source directory: {}", input.display()))?;
            let name = input.file_name().context("source has no filename")?;
            protected.paths.insert(path_key(&parent.join(name)));
            // Protect both a symlink's own name and its resolved target.
            protected.paths.insert(path_key(
                &input
                    .canonicalize()
                    .with_context(|| format!("cannot resolve source: {}", input.display()))?,
            ));
            ensure!(
                input.metadata()?.is_file(),
                "source is not a regular file: {}",
                input.display()
            );
            protected.files.insert(
                Handle::from_path(input)
                    .with_context(|| format!("cannot protect source: {}", input.display()))?,
            );
        }
        Ok(Self {
            inputs: Arc::new(protected),
            reserved_paths: HashSet::new(),
        })
    }

    pub fn plan(
        &mut self,
        input: &Path,
        out_dir: Option<&Path>,
        target: &str,
        overwrite: bool,
    ) -> Result<OutputPlan> {
        ensure!(
            !target.is_empty() && target.bytes().all(|byte| byte.is_ascii_alphanumeric()),
            "invalid output extension"
        );
        let directory = out_dir.unwrap_or_else(|| parent_directory(input));
        fs::create_dir_all(directory).context("cannot create output directory")?;
        let directory = directory
            .canonicalize()
            .context("cannot resolve output directory")?;
        let directory_handle =
            Handle::from_path(&directory).context("cannot check output directory identity")?;
        let stem = input.file_stem().context("source has no file stem")?;

        for suffix in 0..MAX_NAME_ATTEMPTS {
            let mut name = stem.to_os_string();
            if suffix != 0 {
                name.push(format!(" ({suffix})"));
            }
            name.push(format!(".{target}"));
            let path = directory.join(name);
            let key = path_key(&path);
            if self.reserved_paths.contains(&key) || self.inputs.paths.contains(&key) {
                continue;
            }
            let existing = match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    // Never replace links, junctions, directories, or other special files.
                    if !overwrite || !metadata.is_file() || is_link(&metadata) {
                        continue;
                    }
                    let handle = Handle::from_path(&path)
                        .context("cannot check existing output identity")?;
                    if self.inputs.files.contains(&handle) {
                        continue;
                    }
                    Some(handle)
                }
                Err(error) if error.kind() == ErrorKind::NotFound => None,
                Err(error) => return Err(error).context("cannot inspect output path"),
            };
            self.reserved_paths.insert(key);
            return Ok(OutputPlan {
                path,
                directory_handle,
                inputs: Arc::clone(&self.inputs),
                existing,
            });
        }
        bail!("no available output name after {MAX_NAME_ATTEMPTS} attempts")
    }
}

/// Only the planner can construct a destination, including its overwrite policy.
pub struct OutputPlan {
    path: PathBuf,
    directory_handle: Handle,
    inputs: Arc<ProtectedInputs>,
    existing: Option<Handle>,
}

impl OutputPlan {
    fn check_directory(&self) -> Result<()> {
        let current = Handle::from_path(parent_directory(&self.path))
            .context("output directory is no longer accessible")?;
        ensure!(
            current == self.directory_handle,
            "output directory changed during conversion"
        );
        Ok(())
    }

    pub fn begin(self) -> Result<StagedOutput> {
        self.check_directory()?;
        let directory = tempfile::Builder::new()
            .prefix(".convertcraft-")
            .tempdir_in(parent_directory(&self.path))
            .context("cannot create staging directory")?;
        // FFmpeg infers its muxer from this suffix. The actual file stays open for
        // Rust writers; only FFmpeg needs to reopen the staged pathname.
        let suffix = format!(
            ".{}",
            self.path
                .extension()
                .and_then(|s| s.to_str())
                .context("output has no valid extension")?
        );
        let file = tempfile::Builder::new()
            .prefix("output-")
            .suffix(&suffix)
            .tempfile_in(directory.path())
            .context("cannot create staged output")?;
        Ok(StagedOutput {
            file,
            plan: self,
            _directory: directory,
        })
    }
}

pub struct StagedOutput {
    // Drop the file before its containing directory on all failure paths.
    file: NamedTempFile,
    plan: OutputPlan,
    _directory: TempDir,
}

impl StagedOutput {
    pub fn file_mut(&mut self) -> &mut File {
        self.file.as_file_mut()
    }

    pub fn path(&self) -> &Path {
        self.file.path()
    }

    pub fn commit(self) -> Result<PathBuf> {
        self.file
            .as_file()
            .sync_all()
            .context("cannot flush staged output to disk")?;
        let _publication = self
            .plan
            .inputs
            .publication
            .lock()
            .map_err(|_| anyhow::anyhow!("output publication lock was poisoned"))?;
        self.plan.check_directory()?;
        ensure!(
            !self.plan.inputs.contains(&self.plan.path)?,
            "output aliases a protected source"
        );
        // Detect a replaced staged pathname, including replacement by an external encoder.
        ensure!(
            Handle::from_file(self.file.as_file().try_clone()?)?
                == Handle::from_path(self.file.path())?,
            "staged output changed during conversion"
        );
        if let Some(expected) = self.plan.existing {
            let metadata = fs::symlink_metadata(&self.plan.path)
                .context("existing destination disappeared; retry conversion")?;
            ensure!(
                metadata.is_file() && !is_link(&metadata),
                "destination is no longer a regular file"
            );
            let current = Handle::from_path(&self.plan.path)?;
            ensure!(current == expected, "destination changed; retry conversion");
            // Windows replacement can reject an open destination handle. Finish
            // identity checks and close our destination handles before renaming.
            drop(current);
            drop(expected);
            self.file
                .persist(&self.plan.path)
                .map_err(|error| error.error)
                .context("cannot replace destination")?;
        } else {
            // Even overwrite-enabled jobs use no-clobber when no file existed at
            // planning time. This also protects unexpected filesystem case aliases.
            self.file
                .persist_noclobber(&self.plan.path)
                .map_err(|error| error.error)
                .context(
                    "cannot publish output without overwriting an existing path; retry conversion",
                )?;
        }
        Ok(self.plan.path)
    }
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

// Conservative case folding avoids ordinary case aliases even on filesystems
// that are case-insensitive on Unix. Filesystem no-clobber remains authoritative.
fn path_key(path: &Path) -> OsString {
    path.as_os_str().to_string_lossy().to_lowercase().into()
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn publish(plan: OutputPlan, bytes: &[u8]) -> Result<PathBuf> {
        let mut staged = plan.begin()?;
        staged.file_mut().write_all(bytes)?;
        staged.commit()
    }

    #[test]
    fn new_destination_after_planning_is_not_overwritten() {
        for overwrite in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("input.json");
            let output = dir.path().join("input.yaml");
            fs::write(&input, b"{}").unwrap();
            let mut planner = OutputPlanner::new([input.as_path()]).unwrap();
            let plan = planner.plan(&input, None, "yaml", overwrite).unwrap();
            fs::write(&output, b"created after planning").unwrap();
            assert!(publish(plan, b"replacement").is_err());
            assert_eq!(fs::read(output).unwrap(), b"created after planning");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        }
    }

    #[test]
    fn independent_planners_cannot_publish_over_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        fs::write(&input, b"{}").unwrap();
        let plans: Vec<_> = (0..2)
            .map(|_| {
                OutputPlanner::new([input.as_path()])
                    .unwrap()
                    .plan(&input, None, "yaml", false)
                    .unwrap()
            })
            .collect();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = plans
            .into_iter()
            .enumerate()
            .map(|(index, plan)| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut staged = plan.begin().unwrap();
                    staged
                        .file_mut()
                        .write_all(index.to_string().as_bytes())
                        .unwrap();
                    barrier.wait();
                    staged.commit()
                })
            })
            .collect();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let contents = fs::read_to_string(dir.path().join("input.yaml")).unwrap();
        assert!(contents == "0" || contents == "1");
    }

    #[test]
    fn batch_sources_and_case_aliases_are_reserved() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("batch.json");
        let yaml = dir.path().join("batch.yaml");
        fs::write(&json, b"json source").unwrap();
        fs::write(&yaml, b"yaml source").unwrap();
        let mut planner = OutputPlanner::new([json.as_path(), yaml.as_path()]).unwrap();
        let first = planner.plan(&json, None, "yaml", true).unwrap();
        let second = planner.plan(&yaml, None, "json", true).unwrap();
        assert_eq!(first.path.file_name().unwrap(), "batch (1).yaml");
        assert_eq!(second.path.file_name().unwrap(), "batch (1).json");
        publish(first, b"converted json").unwrap();
        publish(second, b"converted yaml").unwrap();
        assert_eq!(fs::read(&json).unwrap(), b"json source");
        assert_eq!(fs::read(&yaml).unwrap(), b"yaml source");

        let upper = dir.path().join("A.json");
        let lower = dir.path().join("a.yaml");
        fs::write(&upper, b"upper source").unwrap();
        fs::write(&lower, b"lower source").unwrap();
        let mut planner = OutputPlanner::new([upper.as_path(), lower.as_path()]).unwrap();
        let first = planner.plan(&upper, None, "csv", false).unwrap();
        let second = planner.plan(&lower, None, "csv", false).unwrap();
        let a = publish(first, b"first").unwrap();
        let b = publish(second, b"second").unwrap();
        assert_eq!(fs::read(a).unwrap(), b"first");
        assert_eq!(fs::read(b).unwrap(), b"second");
    }

    #[test]
    fn hard_link_to_source_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        let alias = dir.path().join("input.yaml");
        fs::write(&input, b"source").unwrap();
        fs::hard_link(&input, &alias).unwrap();
        let mut planner = OutputPlanner::new([input.as_path()]).unwrap();
        let plan = planner.plan(&input, None, "yaml", true).unwrap();
        assert_eq!(plan.path.file_name().unwrap(), "input (1).yaml");
        publish(plan, b"converted").unwrap();
        assert_eq!(fs::read(input).unwrap(), b"source");
        assert_eq!(fs::read(alias).unwrap(), b"source");
    }

    #[test]
    fn source_alias_created_after_planning_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        let output = dir.path().join("input.yaml");
        fs::write(&input, b"source").unwrap();
        let mut planner = OutputPlanner::new([input.as_path()]).unwrap();
        let plan = planner.plan(&input, None, "yaml", true).unwrap();
        fs::hard_link(&input, &output).unwrap();
        assert!(publish(plan, b"replacement").is_err());
        assert_eq!(fs::read(input).unwrap(), b"source");
        assert_eq!(fs::read(output).unwrap(), b"source");
    }

    #[test]
    fn replaced_existing_destination_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        let output = dir.path().join("input.yaml");
        fs::write(&input, b"source").unwrap();
        fs::write(&output, b"old output").unwrap();
        let mut planner = OutputPlanner::new([input.as_path()]).unwrap();
        let plan = planner.plan(&input, None, "yaml", true).unwrap();
        fs::remove_file(&output).unwrap();
        fs::write(&output, b"different file").unwrap();
        assert!(publish(plan, b"replacement").is_err());
        assert_eq!(fs::read(output).unwrap(), b"different file");
    }

    #[test]
    fn staging_is_cleaned_up_without_publishing() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        fs::write(&input, b"source").unwrap();
        let mut planner = OutputPlanner::new([input.as_path()]).unwrap();
        let plan = planner.plan(&input, None, "yaml", false).unwrap();
        let mut staged = plan.begin().unwrap();
        staged.file_mut().write_all(b"partial").unwrap();
        let staged_path = staged.path().to_owned();
        drop(staged);
        assert!(!staged_path.exists());
        assert!(!dir.path().join("input.yaml").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn replaced_output_directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        let output_dir = dir.path().join("output");
        fs::write(&input, b"source").unwrap();
        let mut planner = OutputPlanner::new([input.as_path()]).unwrap();
        let plan = planner
            .plan(&input, Some(&output_dir), "yaml", true)
            .unwrap();
        fs::rename(&output_dir, dir.path().join("moved")).unwrap();
        fs::create_dir(&output_dir).unwrap();
        assert!(plan.begin().is_err());
        assert_eq!(fs::read_dir(output_dir).unwrap().count(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn differently_cased_source_extension_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.JSON");
        fs::write(&input, b"original").unwrap();
        let mut planner = OutputPlanner::new([input.as_path()]).unwrap();
        let plan = planner.plan(&input, None, "json", true).unwrap();
        assert_eq!(plan.path.file_name().unwrap(), "source (1).json");
        publish(plan, b"converted").unwrap();
        assert_eq!(fs::read(input).unwrap(), b"original");
    }

    #[cfg(any(unix, windows))]
    fn file_symlink(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let result = std::os::windows::fs::symlink_file(target, link);
        if let Err(error) = result {
            #[cfg(windows)]
            if error.raw_os_error() == Some(1314) {
                // CI sets this so a missing privilege fails instead of skipping.
                assert!(
                    std::env::var_os("CONVERTCRAFT_REQUIRE_SYMLINKS").is_none(),
                    "CONVERTCRAFT_REQUIRE_SYMLINKS is set but symlink creation is not permitted"
                );
                eprintln!("symlink test requires Windows Developer Mode or symlink privilege");
                return false;
            }
            panic!("cannot create test symlink: {error}");
        }
        true
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn symlinks_and_aliased_parent_directories_are_handled() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        fs::write(&input, b"source").unwrap();
        if !file_symlink(&dir.path().join("missing"), &dir.path().join("input.yaml")) {
            return;
        }
        let alias = dir.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(dir.path(), &alias).unwrap();
        let mut planner = OutputPlanner::new([input.as_path()]).unwrap();
        let first = planner.plan(&input, Some(&alias), "yaml", true).unwrap();
        let second = planner
            .plan(&input, Some(dir.path()), "yaml", true)
            .unwrap();
        assert_eq!(first.path.file_name().unwrap(), "input (1).yaml");
        assert_eq!(second.path.file_name().unwrap(), "input (2).yaml");
        publish(first, b"first").unwrap();
        publish(second, b"second").unwrap();
        assert!(!dir.path().join("missing").exists());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn late_destination_symlink_does_not_modify_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        let victim = dir.path().join("victim");
        fs::write(&input, b"source").unwrap();
        fs::write(&victim, b"must survive").unwrap();
        let mut planner = OutputPlanner::new([input.as_path()]).unwrap();
        let plan = planner.plan(&input, None, "yaml", false).unwrap();
        if !file_symlink(&victim, &dir.path().join("input.yaml")) {
            return;
        }
        assert!(publish(plan, b"replacement").is_err());
        assert_eq!(fs::read(victim).unwrap(), b"must survive");
    }
}
