//! Batch input protection and transactional output publication.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail, ensure};
use tempfile::{NamedTempFile, TempDir};

const MAX_NAME_ATTEMPTS: usize = 10_000;

/// File identity read once, without keeping the file open, so a batch holds no
/// descriptors per queued job.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileKey {
    volume: u64,
    /// Unix inode number, or the Windows 128-bit file ID. The 64-bit `nFileIndex` is
    /// not unique on ReFS and may be zero or reused on network and FAT redirectors.
    index: u128,
    /// Unix inode change time in nanoseconds (0 on Windows). Without an open handle an
    /// inode number can be reused by a new file right after deletion; the new file has
    /// a fresh change time. NTFS file IDs already carry a reuse sequence number.
    changed: i64,
}

impl FileKey {
    /// `None` when the file system reports no usable file ID (some network and FAT
    /// redirectors report 0); identity checks then fail safe.
    fn new(volume: u64, index: u128, changed: i64) -> Option<Self> {
        (index != 0 && index != u128::MAX).then_some(Self {
            volume,
            index,
            changed,
        })
    }

    /// Same volume and file ID, ignoring the change time.
    fn same_object(self, other: Self) -> bool {
        self.volume == other.volume && self.index == other.index
    }

    #[cfg(unix)]
    fn from_metadata(metadata: &Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let changed = metadata
            .ctime()
            .saturating_mul(1_000_000_000)
            .saturating_add(metadata.ctime_nsec());
        Self::new(metadata.dev(), metadata.ino().into(), changed)
    }

    /// A file system that cannot report a 128-bit file ID gets no identity, so
    /// existing files there are never replaced and identity checks fail safe.
    #[cfg(windows)]
    fn from_handle(file: &File) -> Option<Self> {
        let id = convertcraft_file_id::file_id(file).ok()?;
        Self::new(id.volume, id.id, 0)
    }

    fn of_path(path: &Path) -> io::Result<Option<Self>> {
        #[cfg(unix)]
        {
            Ok(Self::from_metadata(&fs::metadata(path)?))
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Needed to open directories as well as files.
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)?;
            Ok(Self::from_handle(&file))
        }
    }

    fn of_file(file: &File) -> io::Result<Option<Self>> {
        #[cfg(unix)]
        {
            Ok(Self::from_metadata(&file.metadata()?))
        }
        #[cfg(windows)]
        {
            Ok(Self::from_handle(file))
        }
    }
}

/// True only when both identities are known and equal.
fn same_file(a: Option<FileKey>, b: Option<FileKey>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a == b)
}

#[derive(Default)]
struct ProtectedInputs {
    paths: HashSet<OsString>,
    files: HashSet<FileKey>,
    // Encoders run concurrently; serialize publication so filesystem aliases
    // missed by name folding are rechecked after the preceding commit.
    publication: Mutex<()>,
}

impl ProtectedInputs {
    fn protect(&mut self, input: &Path) -> Result<()> {
        // Reserve the names first, so even a source that cannot be inspected is
        // never chosen as an output.
        self.paths.insert(path_key(input));
        let parent = parent_directory(input)
            .canonicalize()
            .context("cannot resolve source directory")?;
        let name = input.file_name().context("source has no filename")?;
        self.paths.insert(path_key(&parent.join(name)));
        // Protect both a symlink's own name and its resolved target.
        self.paths.insert(path_key(
            &input.canonicalize().context("cannot resolve source")?,
        ));
        ensure!(input.metadata()?.is_file(), "source is not a regular file");
        if let Some(key) = FileKey::of_path(input).context("cannot check source identity")? {
            self.files.insert(key);
        }
        Ok(())
    }

    fn contains(&self, path: &Path) -> Result<bool> {
        if self.paths.contains(&path_key(path)) {
            return Ok(true);
        }
        match FileKey::of_path(path) {
            Ok(Some(key)) => Ok(self.files.contains(&key)),
            // Existing file whose identity cannot be checked: assume it may be a source.
            Ok(None) => Ok(true),
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
    /// Protects every input and returns the ones that could not be inspected, by
    /// position. Those rows should not be converted; the others are unaffected.
    pub fn new<'a>(
        inputs: impl IntoIterator<Item = &'a Path>,
    ) -> (Self, Vec<(usize, anyhow::Error)>) {
        let mut protected = ProtectedInputs::default();
        let mut errors = Vec::new();
        for (index, input) in inputs.into_iter().enumerate() {
            if let Err(error) = protected.protect(input) {
                errors.push((index, error.context(format!("{}", input.display()))));
            }
        }
        let planner = Self {
            inputs: Arc::new(protected),
            reserved_paths: HashSet::new(),
        };
        (planner, errors)
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
        let source = FileKey::of_path(input).context("cannot check source identity")?;
        let directory = out_dir.unwrap_or_else(|| parent_directory(input));
        fs::create_dir_all(directory).context("cannot create output directory")?;
        let directory = directory
            .canonicalize()
            .context("cannot resolve output directory")?;
        let directory_key =
            FileKey::of_path(&directory).context("cannot check output directory identity")?;
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
                    let existing =
                        FileKey::of_path(&path).context("cannot check existing output identity")?;
                    // Replace only files whose identity can be rechecked at commit
                    // and that are not a batch source.
                    match existing {
                        Some(key) if !self.inputs.files.contains(&key) => Some(key),
                        _ => continue,
                    }
                }
                Err(error) if error.kind() == ErrorKind::NotFound => None,
                Err(error) => return Err(error).context("cannot inspect output path"),
            };
            self.reserved_paths.insert(key);
            return Ok(OutputPlan {
                path,
                directory_key,
                inputs: Arc::clone(&self.inputs),
                existing,
                source,
            });
        }
        bail!("no available output name after {MAX_NAME_ATTEMPTS} attempts")
    }
}

/// Only the planner can construct a destination, including its overwrite policy.
pub struct OutputPlan {
    path: PathBuf,
    directory_key: Option<FileKey>,
    inputs: Arc<ProtectedInputs>,
    existing: Option<FileKey>,
    source: Option<FileKey>,
}

impl OutputPlan {
    fn check_directory(&self) -> Result<()> {
        let current = FileKey::of_path(parent_directory(&self.path))
            .context("output directory is no longer accessible")?;
        // A directory without a usable file ID cannot be rechecked. Its change time
        // moves whenever an entry (such as our staging folder) is created, so compare
        // the object only; a reused inode would be a new directory at the same path.
        if let Some(expected) = self.directory_key {
            ensure!(
                current.is_some_and(|current| current.same_object(expected)),
                "output directory changed during conversion"
            );
        }
        Ok(())
    }

    /// Open the source for reading and check that it is still the regular file that
    /// was planned, so a file swapped in after planning is not converted.
    pub fn open_source(&self, input: &Path) -> Result<File> {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            // A FIFO swapped in for the source must not block the worker in open().
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(input).context("cannot open source")?;
        ensure!(
            file.metadata()?.is_file(),
            "source is no longer a regular file"
        );
        if self.source.is_some() {
            ensure!(
                same_file(FileKey::of_file(&file)?, self.source),
                "source changed since the batch started; retry conversion"
            );
        }
        Ok(file)
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
            .map_err(|_| anyhow!("output publication lock was poisoned"))?;
        self.plan.check_directory()?;
        ensure!(
            !self.plan.inputs.contains(&self.plan.path)?,
            "output aliases a protected source"
        );
        // Detect a replaced staged pathname, including replacement by an external encoder.
        let staged = FileKey::of_file(self.file.as_file())?;
        if staged.is_some() {
            ensure!(
                same_file(staged, FileKey::of_path(self.file.path())?),
                "staged output changed during conversion"
            );
        }
        if let Some(expected) = self.plan.existing {
            let metadata = fs::symlink_metadata(&self.plan.path)
                .context("existing destination disappeared; retry conversion")?;
            ensure!(
                metadata.is_file() && !is_link(&metadata),
                "destination is no longer a regular file"
            );
            ensure!(
                FileKey::of_path(&self.plan.path)? == Some(expected),
                "destination changed; retry conversion"
            );
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

    fn protect_all<'a>(inputs: impl IntoIterator<Item = &'a Path>) -> OutputPlanner {
        let (planner, errors) = OutputPlanner::new(inputs);
        assert!(errors.is_empty(), "{errors:?}");
        planner
    }

    fn publish(plan: OutputPlan, bytes: &[u8]) -> Result<PathBuf> {
        let mut staged = plan.begin()?;
        staged.file_mut().write_all(bytes)?;
        staged.commit()
    }

    #[test]
    fn unresolvable_inputs_are_reported_per_row() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.json");
        let missing = dir.path().join("missing.json");
        let folder = dir.path().join("folder.json");
        fs::write(&good, b"{}").unwrap();
        fs::create_dir(&folder).unwrap();
        let (mut planner, errors) =
            OutputPlanner::new([missing.as_path(), good.as_path(), folder.as_path()]);
        let failed: Vec<usize> = errors.iter().map(|(index, _)| *index).collect();
        assert_eq!(failed, [0, 2]);
        assert!(format!("{:#}", errors[1].1).contains("not a regular file"));
        // The remaining row still plans and publishes.
        publish(planner.plan(&good, None, "yaml", false).unwrap(), b"ok").unwrap();
        // A failed row's own name is still reserved.
        let plan = planner.plan(&good, None, "json", false).unwrap();
        assert_ne!(plan.path.file_name().unwrap(), "missing.json");
    }

    #[test]
    fn unknown_file_ids_never_compare_equal() {
        assert_eq!(FileKey::new(7, 0, 0), None);
        assert_eq!(FileKey::new(7, u128::MAX, 0), None);
        let key = FileKey::new(7, 42, 0);
        assert!(same_file(key, key));
        assert!(!same_file(None, None));
        assert!(!same_file(key, None));
        let touched = FileKey::new(7, 42, 1).unwrap();
        assert!(!same_file(key, Some(touched)));
        assert!(key.unwrap().same_object(touched));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn planning_holds_no_file_descriptors() {
        let open_fds = || fs::read_dir("/proc/self/fd").unwrap().count();
        let dir = tempfile::tempdir().unwrap();
        let inputs: Vec<PathBuf> = (0..300)
            .map(|i| {
                let path = dir.path().join(format!("{i}.json"));
                fs::write(&path, b"{}").unwrap();
                path
            })
            .collect();
        let before = open_fds();
        let mut planner = protect_all(inputs.iter().map(PathBuf::as_path));
        let plans: Vec<_> = inputs
            .iter()
            .map(|input| planner.plan(input, None, "yaml", false).unwrap())
            .collect();
        // Other test threads may hold a few descriptors; 300 jobs used to hold 600.
        assert!(open_fds() < before + 20, "{} -> {}", before, open_fds());
        drop(plans);
    }

    #[test]
    fn source_replaced_after_planning_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        fs::write(&input, b"original").unwrap();
        let mut planner = protect_all([input.as_path()]);
        let plan = planner.plan(&input, None, "yaml", false).unwrap();
        assert!(plan.open_source(&input).is_ok());
        let moved = dir.path().join("moved.json");
        fs::rename(&input, &moved).unwrap();
        fs::write(&input, b"swapped").unwrap();
        let error = plan.open_source(&input).unwrap_err();
        assert!(format!("{error:#}").contains("source changed"), "{error:#}");
    }

    #[test]
    fn source_deleted_and_recreated_after_planning_is_not_read() {
        // Unix file systems may give the new file the deleted file's inode number.
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        fs::write(&input, b"original").unwrap();
        let mut planner = protect_all([input.as_path()]);
        let plan = planner.plan(&input, None, "yaml", false).unwrap();
        fs::remove_file(&input).unwrap();
        fs::write(&input, b"swapped").unwrap();
        let error = plan.open_source(&input).unwrap_err();
        assert!(format!("{error:#}").contains("source changed"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn fifo_swapped_in_for_source_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.json");
        fs::write(&input, b"{}").unwrap();
        let mut planner = protect_all([input.as_path()]);
        let plan = planner.plan(&input, None, "yaml", false).unwrap();
        fs::remove_file(&input).unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(&input)
            .status()
            .unwrap();
        assert!(status.success());
        let error = plan.open_source(&input).unwrap_err();
        assert!(format!("{error:#}").contains("regular file"), "{error:#}");
    }

    #[test]
    fn new_destination_after_planning_is_not_overwritten() {
        for overwrite in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("input.json");
            let output = dir.path().join("input.yaml");
            fs::write(&input, b"{}").unwrap();
            let mut planner = protect_all([input.as_path()]);
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
                protect_all([input.as_path()])
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
        let mut planner = protect_all([json.as_path(), yaml.as_path()]);
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
        let mut planner = protect_all([upper.as_path(), lower.as_path()]);
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
        let mut planner = protect_all([input.as_path()]);
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
        let mut planner = protect_all([input.as_path()]);
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
        let mut planner = protect_all([input.as_path()]);
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
        let mut planner = protect_all([input.as_path()]);
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
        let mut planner = protect_all([input.as_path()]);
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
        let mut planner = protect_all([input.as_path()]);
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
        let mut planner = protect_all([input.as_path()]);
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
        let mut planner = protect_all([input.as_path()]);
        let plan = planner.plan(&input, None, "yaml", false).unwrap();
        if !file_symlink(&victim, &dir.path().join("input.yaml")) {
            return;
        }
        assert!(publish(plan, b"replacement").is_err());
        assert_eq!(fs::read(victim).unwrap(), b"must survive");
    }
}
