# ConvertCraft Rust security and correctness audit

**Date:** 2026-10-18. **Revision:** `41a79d1afd72542a5730990db144112b0690f1d9`.

> **Assumptions to correct:** ConvertCraft is an ordinary-user, local desktop converter, not a server or privileged service. Users may open malicious downloaded files or import attacker-controlled folders. Attackers can control those files' bytes, names, directory structure, and sometimes links or concurrent changes in shared source/output directories. The installed application, Rust toolchain, and normal executable search path are trusted. Filesystem permissions still apply. Protecting existing files, keeping the UI usable, and accurately reporting conversion failures are primary security properties. Exported CSV may subsequently be opened in a spreadsheet. These assumptions determine the severity ratings below.

Repository source, manifest, and lockfile were not modified. This report is the only added repository file. Additional probes and fixtures were created under `C:\Users\pierr\AppData\Local\Temp\opencode\convertcraft-audit`; Cargo also populated ignored build artifacts and its normal caches. The requested Markdown report was written directly; no document connector was available.

## 1. Executive summary

**Overall risk: Medium for a local desktop application handling untrusted files.** The principal confirmed problems concern data integrity and resource exhaustion. No first-party unsafe Rust, confirmed memory corruption, authentication bypass, shell command injection, or remote code execution was found. This is not a proof that the substantial native/third-party dependency surface is memory-safe.

Highest-priority issues:

1. **The no-overwrite guarantee is not enforced when writing.** A file created after planning is overwritten; case-only output names collide on Windows even within one batch with overwriting disabled (CC-01).
2. **Failed conversions destroy pre-existing outputs.** FFmpeg failure deletes the destination even when input parsing failed before output creation; JPEG encoding can truncate a destination before rejecting dimensions (CC-02).
3. **Overwrite mode can overwrite another queued source, or the input through an alias.** Concurrent jobs can consequently read changed, partial, or invalid content (CC-03).
4. **Small structured inputs can produce disproportionate memory/CPU consumption.** A 22,891-byte JSON probe produced 4,012,890 bytes of CSV; a 7,116-byte YAML alias probe produced 4,108,131 bytes of JSON (CC-05).
5. **Folder import follows directory links recursively on the UI thread without visited-directory tracking or limits.** Cycles, repeated subtrees, large directories, and slow shares can stall import (CC-04).

Additional findings cover JPEG flush errors reported as success, spreadsheet formula injection, dependency health, and lossy data conversion.

The requested build and Clippy runs completed, and all four existing tests passed, including the installed FFmpeg integration test. `cargo audit` and `cargo deny check` failed. The two security advisories concern `quick-xml 0.30.0`; this version is absent from the Windows target dependency tree, and the inspected Linux dependency path parses build-time XML. A malicious conversion-file path into that XML parser was **not** established. Do not interpret the advisory severity as a demonstrated remotely exploitable application vulnerability.

## 2. Assumptions, architecture, and trust boundaries

### Project and entry points

- One Rust 2024 binary crate, Cargo package `transmute 0.1.0`; no workspace members, library target, conversion CLI, HTTP listener, or application plugin interface. `Cargo.toml:1-14` declares eight direct dependencies. `Cargo.lock` is committed.
- `src/main.rs:14-38`: loads an embedded logo, configures an eframe window, and constructs `App`.
- File dialogs call `App::add_path` at `src/main.rs:198-205,285-287`; drag-and-drop reaches it at `src/main.rs:393-398`.
- `App::start`, `src/main.rs:121-170`, plans paths and launches at most four worker threads. `src/main.rs:172-190` receives internal status messages.
- `src/converter.rs:137-145` dispatches to image decoding/encoding, structured-data deserialization/serialization, or a locally installed FFmpeg process.
- There are 881 lines in the two first-party Rust files, including tests. All were read.

### Trust boundaries

1. **File bytes:** image decoders, `serde_json::from_str`, `serde_yaml::from_str`, `toml::from_str`, `csv::Reader`, and FFmpeg receive user-selected content. Extension checks classify inputs but do not establish that their contents are safe. Image format guessing can reach enabled formats beyond the advertised input extensions.
2. **Filesystem names and topology:** file dialogs and dropped paths can designate local files, network shares, symbolic links, junctions, and changing directories. Source-relative output is the default. Planning uses lexical paths; writes later reopen those paths.
3. **User settings:** output directory, target format, JPEG quality, and overwrite mode are UI-controlled. Targets come from fixed arrays (`src/converter.rs:34-40`); queue entries store `&'static str` targets (`src/main.rs:47-51`). No user-supplied configuration parser or persistent settings file was found.
4. **Executable/environment boundary:** `Command::new("ffmpeg")` at `src/converter.rs:294` and platform file-manager commands at `src/main.rs:431-438` rely on the launch environment/executable search. FFmpeg availability is checked synchronously during `App::new` (`src/main.rs:84`). A hostile executable search path already crosses a code-execution trust boundary; simply selecting a media file does not change the process working directory or PATH.
5. **OS/IPC/native code:** eframe, windowing, graphics, native dialogs, clipboard/accessibility dependencies, and FFmpeg interact with OS/native APIs. Linux dependencies include D-Bus/accessibility/portal integrations. These are dependency-level boundaries, not a first-party RPC service.
6. **Exported data:** CSV bytes become input to spreadsheets or other downstream software. CSV escaping is not spreadsheet formula neutralization.
7. **Build:** registry dependencies, their build scripts/procedural macros, the Rust toolchain, and Cargo's advisory/index services are trusted build-time inputs. An attacker who can alter source or the build environment is outside the ordinary malicious-file model.

### Threat model and exclusions

The principal attacker supplies an attractive file/folder and persuades the user to import or convert it. A stronger local/share attacker can change paths between planning and writing. Neither requires the application to expose a network server. Attacks have the user's filesystem privileges, not automatic administrator privileges.

Normal overwrite permission is not treated as permission to corrupt another batch input or delete an old destination after an input-validation failure. Availability findings are rated for a desktop process, not a multi-tenant service. Spreadsheet behavior is an additional user interaction and depends on the spreadsheet application/settings.

No first-party authentication, authorization database, cryptographic protocol, credential store, SQL execution, HTML template rendering, or TLS client configuration exists in the reviewed source. Their absence is not a negative finding. FFmpeg itself and external spreadsheet applications were not fully audited.

## 3. Tooling results

### Environment and reproducibility

- Host: Windows, `x86_64-pc-windows-msvc`.
- Stable: `rustc 1.97.1 (8bab26f4f 2026-07-14)`, `cargo 1.97.1 (c980f4866 2026-06-30)`.
- Nightly: `rustc 1.101.0-nightly (8006091eb 2026-10-17)`; Miri and rust-src were already installed.
- Installed tools: `cargo-audit 0.22.2`, `cargo-deny 0.20.2`, `cargo-geiger 0.14.0`, Clippy, ripgrep. No missing tool required installation.
- FFmpeg: `C:\Users\pierr\ffmpeg\bin\ffmpeg.exe`, `2025-07-17-git-bc8d06d541-essentials_build-www.gyan.dev`, libavformat `62.1.103`.
- RustSec database: 1,338 advisories, revision `999ec89d5d3f10f20be14750ec8c2d8b44e0ac9b` after fetching during the audit.
- The crate declares no own feature flags. `--all-features` does not mean every feature of every dependency or every platform was exercised.

### Requested commands

| Command | Outcome | Interpretation |
| --- | --- | --- |
| `cargo check --all-targets --all-features` | Passed, exit 0; 6.78 s reported | Current Windows targets compile. |
| `cargo clippy --all-targets --all-features -- -W clippy::pedantic -W clippy::unwrap_used -W clippy::expect_used -W clippy::panic -W clippy::indexing_slicing -W clippy::arithmetic_side_effects` | Passed with warnings, exit 0; 18.47 s | Binary: 21 warnings; test target: 42 warnings, 20 duplicates. These counts are not vulnerability counts. |
| `cargo test --all-features` | Passed, exit 0 | 4 passed, 0 failed, 0 ignored; tests took 1.68 s after build. |
| `cargo audit` | Failed, exit 1 | 2 vulnerabilities in one locked version of quick-xml; 9 allowed maintenance warnings. |
| `cargo deny check` | Failed, exit 1 | `advisories FAILED, bans ok, licenses FAILED, sources ok`. No deny configuration was found; defaults were used. |
| `cargo geiger` | Failed, exit 1 | Numerous package-matching errors, then failure removing `target\debug\deps\transmute-430326184fcb3cea.exe`. It ran alongside Cargo tests; a Windows artifact lock is consistent with this failure. No trustworthy aggregate unsafe count was produced. |
| `rg --line-number '\bunsafe\b' src` | No matches, expected exit 1 | Complete textual first-party unsafe inventory: zero. Dedicated searches also found no FFI declarations/raw-pointer constructs/manual Send/Sync implementations. |
| `cargo +nightly miri test --all-features converter::tests::data_round_trips` | Blocked, exit 1 | Miri isolation rejects `GetTempPathW` in the test fixture helper, before parser coverage. |
| Same Miri command with `MIRIFLAGS=-Zmiri-disable-isolation` | Blocked, exit 1 | Windows `CreateDirectoryW` is unsupported by Miri. This is a tool/platform limitation, not detected UB. |

The four existing tests are `output_paths_never_collide`, `data_round_trips`, `image_to_every_target`, and `media_via_ffmpeg`. FFmpeg was available and the media test ran through its conversion loop; it did not take its early-success skip branch (`src/converter.rs:415-418`). The path test does not cover aliases, output-creation races, or the inputs of other jobs.

Clippy's security-relevant warnings were individually considered:

- `cat.targets()[0]` (`src/converter.rs:79`) is safe under the current nonempty fixed target arrays; no attacker can replace them.
- `self.files[i]` (`src/main.rs:176,179`) receives indices generated internally. UI add/remove/clear controls and dropped-file ingestion are disabled while the channel exists; no index-forging path was found.
- `queue.lock().unwrap()` (`src/main.rs:159`) does not hold the mutex during conversion. Queue popping itself has no input callback; no practical poison/deadlock trigger was established.
- The embedded-logo `expect` (`src/main.rs:16`) uses compile-time bytes, not a selected file.
- Progress division is guarded by `self.total > 0` (`src/main.rs:261-262`). Float precision warnings concern display accuracy; test-only `u32 as u8` casts create sample pixels.
- The output suffix counter (`src/converter.rs:118,133`) is an inferred `i32`, theoretically overflowing after billions of occupied names. The practically reachable problem is an unbounded naming loop on the UI thread; integer overflow at that extreme was not promoted to a separate finding.
- Test `unwrap`/`panic` calls and style warnings were not reported as vulnerabilities.

### Dependency scan details

`cargo audit` reported:

| Crate/version | Advisory | Kind / result |
| --- | --- | --- |
| `quick-xml 0.30.0` | [RUSTSEC-2026-0194](https://rustsec.org/advisories/RUSTSEC-2026-0194) | Quadratic duplicate-attribute checking; upstream CVSS 7.5; fixed in `>=0.41.0`. |
| `quick-xml 0.30.0` | [RUSTSEC-2026-0195](https://rustsec.org/advisories/RUSTSEC-2026-0195) | Unbounded namespace declarations in NsReader; upstream CVSS 7.5; fixed in `>=0.41.0`. |
| `paste 1.0.15` | RUSTSEC-2024-0436 | Unmaintained. |
| `ttf-parser 0.25.1` | RUSTSEC-2026-0192 | Unmaintained. |
| `unsafe-libyaml 0.2.11` | RUSTSEC-2024-0449 | Unmaintained; reached by the application's YAML parser. |
| `zbus_names 3.0.0`, `4.3.4` | RUSTSEC-2026-0371 | Standalone crate unmaintained; functionality moved to zbus 6. |
| `zvariant 4.2.0`, `5.15.0` | RUSTSEC-2026-0370 | Standalone crate unmaintained; functionality moved to successor packages. |
| `zvariant_derive 4.2.0`, `5.15.0` | RUSTSEC-2026-0372 | Standalone crate unmaintained; macros moved to zbus 6. |

`cargo deny` additionally emitted 443 license-rejection diagnostics because no license allowlist was configured, one unlicensed-root-package error, and 42 duplicate-version warnings. `Cargo.toml:1-4` has no license declaration. These are policy/metadata outcomes, not evidence that hundreds of dependencies have security vulnerabilities or legally incompatible licenses. No license was selected on the project's behalf.

`cargo tree --locked --target all -i quick-xml` initially failed because the name is ambiguous (`0.30.0` and `0.41.0` are both locked). Retrying with `quick-xml@0.30.0` succeeded. The Windows-target inverse tree was empty; the all-target tree led through `zbus_xml -> zbus-lockstep[-macros] -> atspi-common -> accesskit_unix -> eframe`. See CC-08 for reachability.

`cargo metadata --locked --offline --format-version 1` enumerated the root plus 490 registry packages, 87 packages declaring build-script targets, and 41 declaring proc-macro targets across the resolved graph. This is not a count of scripts executed on Windows. No git-source dependencies were found. A read-only check of the cached sparse registry index found entries for all 490 locked registry versions and **zero marked yanked**. This describes the fetched/local index snapshot, not future registry state.

### Additional bounded probes

An external Cargo harness used `#[path = "D:/Github/ConvertCraft/src/converter.rs"] mod converter;` to call the actual unchanged backend. It used a copied lockfile and matched direct dependencies. Commands had this form:

```powershell
cargo run --offline --manifest-path 'C:\Users\pierr\AppData\Local\Temp\opencode\convertcraft-audit\Cargo.toml' --target-dir 'D:\Github\ConvertCraft\target' --bin <probe>
```

The initial backend probe binary was `convertcraft-audit-probe`; additional binaries were `path_alias`, `flush_probe`, and `failure_probe`. `flush_probe` substitutes a failing `Write` implementation for `File` to test the exact buffering/encoder composition; it is not an injected filesystem failure in the production process. Findings below include their relevant observations.

To get useful Miri coverage without unsupported filesystem calls, an external in-memory test module exercised the **same serde_yaml-to-serde_json::Value boundary** as `src/converter.rs:196`, including serialization, valid aliases, malformed YAML, undefined aliases, cyclic aliases, and duplicate keys:

```powershell
cargo +nightly miri test --offline --manifest-path 'C:\Users\pierr\AppData\Local\Temp\opencode\convertcraft-audit\Cargo.toml' --target-dir 'D:\Github\ConvertCraft\target' --lib
```

Final result: **3 passed, 0 failed**, 2.52 s, with no Miri UB diagnostic. The first version of this external test incorrectly expected duplicate keys to be rejected; it failed on `{a: 1, a: 2}`. The corrected test verifies the observed last-value-wins behavior, and the malformed-input test separately covers actual errors. This did not require a repository change. A few successful inputs cannot establish the soundness of unsafe-libyaml or the entire dependency graph.

A separate Python probe ran the application's FFmpeg argument pattern against locally generated media fixtures and a loopback-only HTTP server, with an 8-second child timeout. HLS text renamed to `.mp4` was rejected by this FFmpeg build's extension/MIME checks; a concat file referencing `../private.wav` was rejected as an unsafe filename. **Zero loopback HTTP requests occurred.** These probes did not establish SSRF or arbitrary file disclosure. Other FFmpeg versions/demuxers were not exhaustively tested.

Raw command logs are retained by the harness under `C:\Users\pierr\.local\share\opencode\shell\e26ebfdb2c6dac4ea02396073cc47efe84c2f901\`. Key logs: `sh_14f27eb4b001l6EejyonmnkP62.out` (check), `sh_14f28065e001HgGs4Fe78vvL4R.out` (Clippy), `sh_14f280689001TTJNWgGNAihXpq.out` (tests), `sh_14f27eb60001mSJcj02sxaHE2n.out` (audit), `sh_14f27eb740016EMuqyWrjCAwD3.out` (deny), `sh_14f2806be001rjPyITgmmXstXu.out` (Geiger). Essential results are reproduced here so those local logs are not required to interpret the audit.

## 4. Findings table

Severity is contextual to the desktop threat model. **Confirmed** means the behavior follows directly from reviewed code and/or was reproduced; it does not mean every proposed impact was exercised. **Plausible** and **Needs verification** qualify individual consequences where indicated. No Critical or High finding was established under the baseline model.

| ID | Severity | Category | Location | Summary |
| --- | --- | --- | --- | --- |
| CC-01 | Medium | Filesystem / TOCTOU | `src/converter.rs:118-133,159,216,338`; `src/main.rs:132-140` | **Confirmed:** no-overwrite bypass and Windows case-alias output collisions. |
| CC-02 | Medium | Data integrity / failure handling | `src/converter.rs:159-161,336-355` | **Confirmed:** a failed conversion can delete or truncate an existing destination. |
| CC-03 | Medium | Batch correctness / file identity | `src/main.rs:122-140`; `src/converter.rs:125-128` | **Confirmed:** overwrite mode permits outputs to alias source files, including another queued input. |
| CC-04 | Medium | Availability / filesystem traversal | `src/main.rs:96-108,198-205,393-398` | **Confirmed design flaw:** unbounded recursive link-following import executes on the UI thread. |
| CC-05 | Medium | Availability / resource amplification | `src/converter.rs:192-216,220-233,265-288` | **Confirmed:** unrestricted materialization and quadratic sparse CSV expansion; YAML alias expansion. |
| CC-06 | Medium | Error propagation / Drop | `src/converter.rs:159-161,187`; `src/main.rs:162-165` | **Confirmed mechanism:** JPEG buffered-write failures can be discarded while the UI reports success. |
| CC-07 | Medium | Output injection | `src/converter.rs:275-286` | **Confirmed sink; conditional downstream impact:** CSV headers and string cells preserve spreadsheet formulas. |
| CC-08 | Low | Dependencies / build | `Cargo.lock:2878-2885,3293-3302,3832-3835`; `Cargo.toml:9,13` | **Confirmed dependency debt:** vulnerable XML version and unmaintained parsers; application exploitability not established. |
| MIN-01 | Low | Data fidelity | `src/converter.rs:195-196,226-230,236-251` | **Confirmed:** duplicate keys/headers silently collapse; large CSV integers can round through f64. |
| MIN-02 | Info | Error reporting / test isolation | `src/main.rs:98-104`; `src/converter.rs:139,364-367` | **Confirmed:** import errors can disappear; predictable test fixture paths collide across runs. |

## 5. Detailed findings

Suggested diffs are **design sketches**, not applied patches. Helper functions named in sketches need implementations and tests; they are not assumed to exist. In particular, portable atomic replacement/no-clobber semantics must be designed for Windows and Unix rather than implemented with another `exists()` check.

### CC-01 — No-overwrite is a planning hint, not an enforced write policy

**Severity:** Medium. **Status:** Confirmed. **Trigger:** output-name collisions or concurrent changes in a destination directory. Arbitrary-target damage through a replaced link is plausible where the attacker can manipulate that directory/link; that escalation was not exercised.

**Evidence — `src/converter.rs:125-131`:**

```rust
let candidate = dir.join(name);
let taken = candidate == input
    || reserved.contains(&candidate)
    || (!overwrite && candidate.exists());
if !taken {
    reserved.insert(candidate.clone());
    return candidate;
}
```

Actual writes use `File::create(output)` (`:159`), `std::fs::write(output, out)` (`:216`), image path-saving APIs (`:163,184`), or unconditional FFmpeg `-y` (`:338`). `Options::overwrite` is not enforced by any backend write. All outputs are planned before the workers start (`src/main.rs:124-158`). `HashSet<PathBuf>` does not give filesystem object identity or Windows case-insensitive comparison.

**Concrete scenarios and reproduction:**

1. Plan `race.yaml` with `overwrite=false`; another process creates that file before the worker writes. The probe wrote `SENTINEL_CREATED_AFTER_PLANNING` in this interval. Calling the actual data backend replaced it with `new: true\n` and returned success.
2. On a normal case-insensitive Windows filesystem, queue `A.json` and `a.yaml`, and select CSV for both. Before either output exists, the planner reserves both `A.csv` and `a.csv` as different paths with `overwrite=false`. The probe observed `origin\nA\n` after the first conversion and `origin\nB\n` when rereading the first output after the second. Workers may also write simultaneously, producing corruption rather than a clean last writer.
3. A malicious writer in a shared output directory can replace a checked path or parent with a link between planning and opening. The eventual writer follows filesystem resolution, not the object checked earlier. Exact consequences depend on permissions/platform link behavior.

**Impact:** silent loss/corruption despite the default non-overwrite setting. Atomic creation also matters for dangling links, for which `Path::exists()` can be false.

**Fix sketch:** make output ownership/no-clobber a writer-level guarantee. Encode to a unique same-directory temporary file, flush it, then commit with an atomic no-replace operation when overwriting is disabled. On conflict, choose another name and retry the commit. Where directories themselves are hostile, anchor operations to trusted directory handles and enforce link/reparse-point policy.

```diff
- let output = converter::plan_output(...);
+ let output = plan_output_transaction(..., opts.overwrite)?;
  // Workers encode into output's owned temporary file/handle.
- std::fs::write(output, out)?;
+ output.write_all(out.as_bytes())?;
+ output.flush()?;
+ output.commit_no_clobber_or_replace()?;
```

Do not implement this as `exists()` followed by a normal rename. Merely changing FFmpeg to `-n` cannot repair the image/data paths or every filesystem race. Test case-insensitive aliases and symlinks in addition to exact strings.

### CC-02 — Failure cleanup removes old outputs; early truncation loses them

**Severity:** Medium. **Status:** Confirmed. **Trigger:** malformed/incompatible input with overwrite enabled, or a destination created after planning under CC-01.

**Evidence — `src/converter.rs:351-355`:**

```rust
if !out.status.success() {
    let _ = std::fs::remove_file(output);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let msg = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
    bail!("ffmpeg: {msg}");
}
```

This deletion does not establish that FFmpeg created the path. FFmpeg can fail while opening the input, before touching the existing destination. JPEG creates/truncates the destination before encoding and validation (`src/converter.rs:159-161`). Other image/data writes are likewise not transactional.

**Reproduction:** create `broken.mp4` containing `not a media file`; create the chosen output `existing.wav` containing a sentinel. Conversion to WAV returns an error and `existing.wav` is gone. Probe result: `error=true, existing-output-survives=false`. This is reachable in the UI when overwriting is selected; passing default options directly also demonstrates that the backend does not enforce them.

A second probe used a valid **1,072-byte PNG of dimensions 65,536 by 1** and a pre-existing `.jpg`. JPEG's encoder rejected it with `Invalid image size (65536 x 1) to encode as JPEG`, but the destination had already been opened with `File::create`. The probe confirmed **`existing_output_bytes=0`** after the error, replacing the old sentinel with an empty file.

**Impact:** an unsuccessful conversion destroys a previously good output, including when the conversion never produced replacement data. An attacker can supply the invalid media/image, but selecting overwrite or winning a destination race is an additional precondition.

**Fix sketch:** only remove a uniquely owned temporary output. Replace the old destination after complete successful encoding and checked flushing.

```diff
- .arg(output)
+ .arg(transaction.temporary_path_with_target_extension())
  ...
- let _ = std::fs::remove_file(output);
+ transaction.discard_temporary()?;
  ...
+ transaction.commit_after_success()?;
```

For FFmpeg, retain a format-recognizable temporary suffix or pass an explicit output muxer. Keep the previous destination intact on decoder failure, encoder failure, disk-full, cancellation, and child failure. A regression test should assert sentinel bytes remain unchanged for each failure class.

### CC-03 — Output reservations do not protect all source identities

**Severity:** Medium. **Status:** Confirmed. **Trigger:** overwrite mode with related names or aliased source paths.

**Evidence — `src/main.rs:121-140`:**

```rust
let mut reserved = HashSet::new();
// For each file independently:
let output = converter::plan_output(
    &f.path, self.out_dir.as_deref(), f.target,
    self.opts.overwrite, &mut reserved,
);
```

The planner compares only `candidate == input` for the current job (`src/converter.rs:126`). It neither seeds reservations with other source paths nor compares actual file identities.

**Concrete scenarios:**

- Queue `batch.json` containing `{"origin":"A"}` and `batch.yaml` containing `origin: B`. With overwrite enabled, convert the JSON to YAML and the YAML to JSON. The actual planner chooses the other job's source for each output. The probe confirmed `a_out==b=true, b_out==a=true`; after the first conversion the second source contained `origin: A`, permanently losing B. Concurrent scheduling makes the resulting data nondeterministic.
- On Windows, converting `upper.JSON` to JSON with overwrite enabled passes the lexical self-path check because the candidate is `upper.json`. The probe confirmed lexical inequality and that the original file was rewritten. Hard links and parent-directory aliases are additional identity cases, not covered by lexical equality.

**Impact:** original batch inputs are modified unexpectedly; other workers can read overwritten or partially written data. For FFmpeg, input/output alias rejection can also interact with CC-02's unconditional cleanup. That particular combined deletion was not separately tested.

**Fix sketch:** collect and protect all source identities before planning outputs, including completed rows whose originals should remain protected. Canonicalization alone does not identify hard links and is not race-proof. Use platform file IDs/open handles where required, and make overwrite semantics explicitly exclude protected inputs.

```diff
- let mut reserved = HashSet::new();
+ let protected_inputs = collect_source_file_identities(&self.files)?;
+ let mut planner = OutputPlanner::new(protected_inputs);
  ...
- let output = converter::plan_output(...);
+ let output = planner.plan_non_source_output(...)?;
```

Test cross-format cycles, case-only aliases, hard links, symlinked parents, and multiple source directories feeding one output directory. Combine this with CC-01's transaction enforcement.

### CC-04 — Recursive folder import follows links and blocks the UI

**Severity:** Medium. **Status:** Confirmed structural issue; a stack-overflow crash was not triggered during this audit.

**Evidence — `src/main.rs:96-108`:**

```rust
if path.is_dir() {
    if let Ok(entries) = std::fs::read_dir(&path) {
        let mut children: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        children.sort();
        for child in children {
            self.add_path(child);
        }
    }
    return;
}
if self.files.iter().any(|f| f.path == path) { return; }
```

`is_dir()` follows directory links; there is no visited-directory set. The only duplicate check is for already-added files, after directory recursion, and is lexical. The whole routine runs inside the UI update's handlers (`src/main.rs:198-205,393-398,415-426`). Every directory's children are collected/sorted, and each file scans the existing queue for duplicates.

**Concrete failure scenario:** an attacker supplies a folder with links/junctions to ancestors or repeated subtrees. Import follows them rather than recognizing an already-visited directory. OS link/path-length limits may eventually stop one chain; they do not prevent combinatorial repeated traversal. A large plain folder or slow network share also blocks the UI because enumeration is synchronous. A link can import files outside the folder the user thought they selected.

**Reproduction sketch (not executed against the GUI):** on Unix, create a temporary `root` containing a small supported file and two links `a -> .`, `b -> .`; invoke folder import in an externally timed subprocess. Assert import completes within a budget and visits the real file once. Use Windows directory junctions for equivalent platform coverage. Do not run an uncapped cyclic fixture in the test runner itself. A separate deep ordinary directory tree tests stack depth without links.

**Impact:** UI hangs, excessive CPU/memory, duplicate jobs, and unexpected enumeration outside the selected tree. Stack exhaustion is plausible with sufficiently deep traversable paths, but was not demonstrated.

**Fix sketch:** background, iterative traversal; default to not following directory links/reparse points; enforce depth/file/time budgets and cancellation; track directory and file identities when links are allowed.

```diff
- self.add_path(child); // recursive, on UI thread
+ importer.enqueue(child); // iterative background traversal
  ...
+ let metadata = std::fs::symlink_metadata(&path)?;
+ if is_link_or_reparse_point(&metadata) { return Ok(()); }
+ if !visited.insert(directory_identity(&path)?) { return Ok(()); }
+ budget.check_next_entry()?;
```

On Windows, explicitly account for junction/reparse-point behavior rather than assuming all relevant objects are reported identically to Unix symlinks. Surface enumeration failures (MIN-02).

### CC-05 — Unbounded structured-data expansion and conversion work

**Severity:** Medium. **Status:** Confirmed allocation/complexity behavior; intentional full-process OOM was not attempted.

**Evidence — `src/converter.rs:192-215`:**

```rust
let text = std::fs::read_to_string(input).context("cannot read file")?;
let value: Value = match normalize_ext(&extension(input)) {
    "json" => serde_json::from_str(&text).context("invalid JSON")?,
    "yaml" => serde_yaml::from_str(&text).context("invalid YAML")?,
    // ...
};
// Complete output is also materialized as a String.
```

**Evidence — `src/converter.rs:265-286`:**

```rust
let mut headers: Vec<&str> = Vec::new();
for row in &rows {
    for key in row.keys() {
        if !headers.contains(&key.as_str()) { headers.push(key); }
    }
}
// Then every row materializes one field for every union header.
```

For N rows with one distinct key per row, input size is roughly O(N log N), header deduplication makes O(N²) string comparisons, and the CSV has N² cells. `Writer<Vec<u8>>` holds the whole output in memory. `read_csv` also holds every parsed row (`:223-233`). Four workers can independently perform such work (`src/main.rs:151-167`).

**Bounded observed reproductions:**

| Fixture | Input bytes | Output bytes | Observed conversion time |
| --- | --- | --- | --- |
| 500 JSON objects, one distinct key each | 5,391 | 252,890 | 28 ms |
| 1,000 JSON objects, one distinct key each | 10,891 | 1,005,890 | 102 ms |
| 2,000 JSON objects, one distinct key each | 22,891 | 4,012,890 | 402 ms |
| YAML: 4,096-byte anchored scalar plus 1,000 aliases | 7,116 | 4,108,131 | Not separately timed |

Times are single debug-build observations, not a rigorous benchmark. The growth in output bytes is deterministic. The sparse JSON can be generated with `[{"k0":0},{"k1":0},...]`. Its approximate N²-cell growth means 100,000 sparse rows would require on the order of 10 GB just for delimiters/cells; that size was **not run**.

The YAML case uses `anchor: &s <4096 x characters>` and `refs: [*s,*s,...]`. The locked serde_yaml has recursion and alias-jump limits (`serde_yaml-0.9.34+deprecated/src/de.rs:474-480,632-644`), so it would be inaccurate to claim completely unrestricted nesting or an unchecked classic billion-laughs expansion. Nevertheless, the demonstrated shallow scalar-alias amplification is accepted and the application has no expanded-byte/node budget.

**Related resource boundaries:**

- Images do have dependency defaults: `image 0.25.10/src/io/limits.rs:49-55` supplies a 512 MiB best-effort allocation limit, but no width/height caps. Therefore image decoding is not described here as completely unlimited. Application-level aggregate budgeting is absent; `to_rgb8`/`to_rgba8` and encoding allocate beyond the lifetime of individual decoder checks (`src/converter.rs:161,184`). Four concurrent decodes/encodes can still be expensive.
- FFmpeg is synchronously awaited with `.output()` and no timeout/cancellation (`src/converter.rs:336-342`). stdout/stderr are collected in memory. A slow/long media job can occupy a worker for an unbounded application-controlled duration; there is no cancel UI. No specific infinite-loop media sample was established.
- The MPSC channel is unbounded (`src/main.rs:150`), but successful execution sends at most two messages per queued job. It is not an independent infinite producer; bounding the queue and import size is the meaningful mitigation.

**Impact:** attacker-supplied small documents can consume disproportionate memory/CPU, stall workers, or terminate the desktop process when memory is exhausted. Huge files, special files, and very slow shares add input-reading risks because regular-file/size/time policy is absent.

**Fix sketch:** enforce input, expanded-data, row, column, cell, output-byte, and aggregate worker budgets. Stream where possible. A file-metadata size check alone is insufficient for growing files and alias/output expansion. Faster header membership alone does not fix N×K output size.

```diff
- let text = std::fs::read_to_string(input)?;
+ let text = read_utf8_limited(input, limits.input_bytes)?;
- if !headers.contains(&key.as_str()) { headers.push(key); }
+ if seen_headers.insert(key.as_str()) {
+     ensure!(headers.len() < limits.columns, "too many CSV columns");
+     headers.push(key);
+ }
+ let cells = rows.len().checked_mul(headers.len())
+     .ok_or_else(|| anyhow!("CSV size overflow"))?;
+ ensure!(cells <= limits.cells, "CSV expansion limit exceeded");
- let mut writer = csv::Writer::from_writer(Vec::new());
+ let mut writer = csv::Writer::from_writer(BoundedWriter::new(temp, limits.output_bytes));
```

Use a parser/visitor capable of enforcing budgets **during** YAML expansion, not only after `Value` has been built. Explicit image dimension/pixel limits, a total memory semaphore, bounded child stderr, deadlines, and cancellation complement these changes.

### CC-06 — JPEG final buffered-write errors are silently discarded

**Severity:** Medium. **Status:** Confirmed buffering mechanism with a fault-injected writer. Real disk-full conditions were not induced.

**Evidence — `src/converter.rs:159-161`:**

```rust
let file = BufWriter::new(File::create(output)?);
let mut enc = JpegEncoder::new_with_quality(file, opts.jpeg_quality);
enc.encode_image(&DynamicImage::ImageRgb8(img.to_rgb8()))?;
```

The encoder owns the `BufWriter`; no explicit checked flush occurs before `Ok(())` (`:187`). In the locked image encoder, `encode_image` writes its final marker and returns without flushing the underlying writer (`image-0.25.10/src/codecs/jpeg/encoder.rs:621-623`). `BufWriter`'s destructor attempts to flush but cannot return errors to this function.

**Concrete failure scenario:** creation succeeds but storage rejects the buffered data later (full filesystem/quota, disconnected share, late I/O error). A small JPEG fits entirely inside the buffer; encoding appears successful before any underlying write. The application reports Done because the converter returned success (`src/main.rs:162-165,179-181`). The file may be empty or truncated, and a user may delete the original believing conversion succeeded.

**Reproduction:** the external `flush_probe` wraps a writer whose `write()` always returns an I/O error in the same default `BufWriter`, then encodes a 1×1 image. Observed:

```text
encode success=true, underlying write attempts=0
after encoder drop: underlying write attempts=1, error not returned
```

**Fix sketch:** keep ownership of the buffer outside the encoder and propagate flushing errors; combine with transactional outputs from CC-01/02.

```diff
+ use std::io::Write;
- let file = BufWriter::new(File::create(output)?);
- let mut enc = JpegEncoder::new_with_quality(file, opts.jpeg_quality);
- enc.encode_image(&DynamicImage::ImageRgb8(img.to_rgb8()))?;
+ let mut file = BufWriter::new(File::create(output)?);
+ {
+     let mut enc = JpegEncoder::new_with_quality(&mut file, opts.jpeg_quality);
+     enc.encode_image(&DynamicImage::ImageRgb8(img.to_rgb8()))?;
+ }
+ file.flush().context("cannot flush JPEG output")?;
```

If durable-on-disk completion is promised, separately consider `sync_all` before committing; checked flushing and crash durability are distinct guarantees.

### CC-07 — CSV output carries attacker-controlled spreadsheet formulas

**Severity:** Medium. **Status:** Confirmed raw formula emission; spreadsheet evaluation/network effects are conditional and were not executed.

**Evidence — `src/converter.rs:275-286`:**

```rust
writer.write_record(&headers)?;
// ...
Some(Value::String(s)) => s.clone(),
// ...
writer.write_record(&record)?;
```

Neither object keys written as headers nor string values are checked for spreadsheet formula prefixes. CSV quoting escapes delimiters/quotes; it does not force a formula-looking cell to be interpreted as text.

**Reproduction:** convert this JSON to CSV:

```json
[{"cell":"=1+1","=2+2":"literal"}]
```

Observed output:

```csv
=2+2,cell
literal,=1+1
```

**Attacker/impact:** a file supplier can place formulas in apparently ordinary data. If the converted CSV is opened with formula interpretation enabled, it can evaluate formulas, display misleading values, and potentially access external resources depending on spreadsheet capabilities and security settings. No claim of universal code execution is made. `docs/formats.md:73` warns about this behavior, which mitigates surprise but does not prevent it.

**Fix sketch:** expose an explicit spreadsheet-safe export mode, covering headers as well as fields, with tested handling of `=`, `+`, `-`, `@` and relevant leading whitespace/control characters for supported spreadsheet applications. Preserve a separately identified raw-data mode to avoid silently changing legitimate numeric/string data.

```diff
- writer.write_record(&headers)?;
+ writer.write_record(headers.iter().map(|s| spreadsheet_text(s, policy)))?;
- Some(Value::String(s)) => s.clone(),
+ Some(Value::String(s)) => spreadsheet_text(s, policy),
```

For strong typed-cell guarantees, a typed spreadsheet format may be more appropriate than trying to make CSV behave identically across all spreadsheet importers. Test the actual applications being supported.

### CC-08 — Vulnerable locked XML version and unmaintained parser dependencies

**Severity:** Low in the established application threat model. **Status:** Confirmed dependency versions/advisories; conversion-time exploitation of the XML advisories **Needs verification** and was not established. Unmaintained does not itself mean memory-unsafe or malicious.

**Evidence — `Cargo.lock:2878-2885`:**

```toml
name = "quick-xml"
version = "0.30.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
```

Both RUSTSEC-2026-0194 and RUSTSEC-2026-0195 affect this version and recommend `>=0.41.0`. A second, fixed `quick-xml 0.41.0` entry at `Cargo.lock:2888-2894` does not replace or cure the old version.

**Reachability:** the Windows dependency tree has no `quick-xml 0.30.0`. In the all-target tree it enters through the Linux accessibility stack's zbus-lockstep tooling. Inspection of `atspi-common-0.6.0/src/cache.rs:13` shows `#[validate(...)]`; `zbus-lockstep-macros-0.4.4/src/lib.rs:107-158` resolves XML paths, reads files, and calls `zbus_xml::Node::try_from` at macro expansion. The macro documents an overriding `LOCKSTEP_XML_PATH` environment variable (`:19-20,51-52`). This establishes a build-time XML path, not a first-party document conversion or unauthenticated network path. Supplying malicious XML through build inputs/environment could expose build-time resource exhaustion if affected parser operations are reached; the exact advisory-specific operations were not proven here.

**Evidence — directly reachable unmaintained YAML backend:**

```rust
// src/converter.rs:196
"yaml" => serde_yaml::from_str(&text).context("invalid YAML")?,
```

`Cargo.toml:13` requests `serde_yaml = "0.9"`; `Cargo.lock:3293-3302` resolves `0.9.34+deprecated` and depends on `unsafe-libyaml`. `Cargo.lock:3832-3835` resolves `unsafe-libyaml 0.2.11`, which RustSec identifies as unmaintained. This is an unsafe-heavy parser directly receiving malicious-file bytes. The small Miri sample passed, but does not remove its maintenance risk. Other maintenance warnings are listed in section 3; zbus-family warnings partly describe migration into a new major package layout rather than abandonment of the whole ecosystem.

**Impact:** outstanding advisory/build-policy failures and reduced assurance that future malformed-input bugs in an archived parser will be fixed. No concrete memory-corruption payload was found; this finding is not an invented parser exploit.

**Fix sketch:** upgrade the dependency owners that retain the old XML version and retest Linux/macOS/Windows; replace the deprecated YAML backend with a maintained implementation after compatibility/resource-budget review. Merely adding a direct `quick-xml = "0.41"` dependency does not force transitive `0.30` users to change.

```diff
 [dependencies]
- eframe = "0.31"
+ eframe = "<reviewed version with repaired accessibility dependency graph>"
- serde_yaml = "0.9"
+ <maintained_yaml_backend> = "<reviewed compatible version>"
```

This placeholder sketch is deliberately not a valid manifest edit: no successor API/version was integration-tested. Commit the resulting lockfile after validation. Configure `cargo deny` with the project's actual license/source/advisory policy; use only documented, scoped, expiring advisory exceptions if reachability warrants one.

### Minor findings

#### MIN-01 — Silent data loss through duplicate keys and numeric inference

**Severity:** Low. **Status:** Confirmed.

At `src/converter.rs:226-230`, collecting `(header, value)` pairs into a `Map` silently replaces previous entries with identical names. CSV `x,x\nfirst,second\n` became `[{"x":"second"}]`. YAML deserialized directly into `serde_json::Value` similarly accepts `{a: 1, a: 2}` as `{"a":2}`; the external Miri test confirms it. JSON into this map-shaped intermediate also has last-value-wins semantics; that JSON case was reviewed through the boundary rather than separately instrumented.

At `src/converter.rs:240-245`, a CSV field first tries `i64`, then `f64`:

```rust
if let Ok(i) = s.parse::<i64>() { return Value::from(i); }
if let Ok(f) = s.parse::<f64>() {
    if f.is_finite() { return Value::from(f); }
}
```

`18446744073709551615` (u64::MAX) became `1.8446744073709552e+19`, not its exact integer value. Identifiers with leading zeroes also lose formatting by design. `docs/formats.md:54-62` documents inference and duplicate CSV headers, so these are data-fidelity risks rather than claims of an undocumented general lossless guarantee. A sender can exploit duplicate/rounded IDs to mislead downstream consumers; ordinary files are affected too.

**Fix sketch:** default to strings or offer an explicit schema; reject duplicate headers/keys where ambiguity matters; preserve unsigned integers before floating-point inference and do not silently float-convert out-of-range integer lexemes.

```diff
+ ensure!(unique_headers(&headers), "duplicate CSV headers");
  if let Ok(i) = s.parse::<i64>() { return Value::from(i); }
+ if let Ok(u) = s.parse::<u64>() { return Value::from(u); }
+ if is_integer_lexeme(s) { return Value::String(s.to_owned()); }
```

This does not solve arbitrary decimal precision; that requires an explicit representation policy. Use duplicate-rejecting visitors before map insertion if duplicate JSON/YAML keys should be errors.

#### MIN-02 — Suppressed import errors and colliding test fixtures

**Severity:** Info. **Status:** Confirmed.

- `src/main.rs:98-104` uses `if let Ok(entries)` and `entries.flatten()`: an inaccessible directory or per-entry failure disappears from the queue without an error. A user converting an unreadable subtree cannot distinguish an empty folder from failed enumeration. `src/converter.rs:139` discards `create_dir_all` errors; later file opening usually still returns an error, so this is diagnostic loss rather than a demonstrated success bypass.
- `src/converter.rs:364-367` always uses `std::env::temp_dir().join("converter-tests")`; fixed filenames are reused without cleanup. Simultaneous test runs can overwrite each other's inputs/outputs. On shared Unix temp storage, pre-created paths/links can also redirect fixtures subject to OS ownership/link protections; that cross-user scenario was not tested on Windows.

**Fix sketch:** propagate/display import and directory-creation errors; give each test/run an owned, unpredictable temporary directory.

```diff
- std::fs::create_dir_all(parent).ok();
+ std::fs::create_dir_all(parent).context("cannot create output directory")?;
- let dir = std::env::temp_dir().join("converter-tests");
+ let dir = tempfile::tempdir()?; // keep owner alive for the test
```

Use an error-reporting import result instead of flattening directory errors. No stylistic Clippy suggestions are included as findings.

## 6. Unsafe inventory and soundness review

### Complete first-party inventory

| Scope | Unsafe sites | Verdict |
| --- | --- | --- |
| `src/main.rs:1-440` | 0 | No first-party unsafe block/function/impl to classify. |
| `src/converter.rs:1-441`, including tests | 0 | No first-party unsafe block/function/impl to classify. |
| First-party `build.rs`, proc macros, FFI modules | None present | Not applicable. |

No first-party `extern "C"`, `repr(C)`, raw-pointer arithmetic, `MaybeUninit`, `transmute` invocation, `from_raw_parts`, manual `Send`/`Sync`, or custom `Drop` was found. The package name `transmute` is not a use of `std::mem::transmute`. `creation_flags` at `src/converter.rs:299` is a safe standard-library API call.

No safe first-party wrapper was demonstrated to expose undefined behavior. This conclusion is limited to the reviewed implementation and probes, not a certification of safe dependencies. Resource exhaustion, application panics, lost I/O errors, and unsafe Rust are distinct concerns.

### Sampled dependency boundary: YAML

Dependency files below are relative to the local registry source `serde_yaml-0.9.34+deprecated/`. These are a supplemental inventory of the two inspected unsafe wrapper files, **not every unsafe site in all 490 dependencies**. “Unverified” means the full lower-level invariant was not proven; it is not an assertion of unsoundness.

| Site(s) | Operation / invariant considered | Verdict |
| --- | --- | --- |
| `src/libyaml/parser.rs:70-79` | Allocate/pin parser storage, initialize native-shaped state, store borrowed/owned input, then assume initialization. Requires successful initializer to fully initialize state and correct stable input lifetime. | Unverified end-to-end; order/lifetime strategy inspected. |
| `src/libyaml/parser.rs:85-100` | Parse into MaybeUninit event, check success, read event, copy/borrow result, delete event. Depends on parser success contract and valid event tags/lengths. | Unverified. |
| `src/libyaml/parser.rs:104` | Unsafe `convert_event` entry: assumes a valid initialized tagged union and corresponding input buffer. | Unverified caller/callee contract. |
| `src/libyaml/parser.rs:114,117,118,119-121,122` | Alias/scalar union accesses; non-null scalar pointer and valid length for `from_raw_parts`; strings must remain valid until copied. | Unverified lower-level guarantees; no violation observed in probes. |
| `src/libyaml/parser.rs:137,138,142,143` | Sequence/mapping union pointer fields selected by event tag. | Unverified. |
| `src/libyaml/parser.rs:151,153,157,159` | Unsafe anchor/tag helper definitions and C-string conversions. Nulls are checked via NonNull; non-null still requires readable, terminated storage. Copies occur before event deletion. | Unverified termination/allocation invariants. |
| `src/libyaml/parser.rs:199` | Parser deletion in Drop requires initialized owned parser and exactly-once destruction. | Unverified lower-level destructor contract. |
| `src/libyaml/util.rs:16` | `NonNull::new_unchecked(Box::into_raw(boxed))`. Box's allocated pointer is non-null. | Sound for this local non-null conversion. |
| `src/libyaml/util.rs:21-27` | Unsafe `Owned::assume_init`; transfers ownership with `mem::forget` then casts pointer. Caller must have initialized T. | Unverified at all callers; parser initialization caller inspected. |
| `src/libyaml/util.rs:40` | Reference reinterpretation to transparent pointer wrapper; requires matching layout/provenance and valid aliasing. | Unverified. |
| `src/libyaml/util.rs:46` | Reconstruct Box during Drop; requires unique original allocation, correct type/layout, and exactly-once ownership. | Unverified across all callers. |

The inspected wrapper uses null checks for optional strings and copies event-owned bytes before deleting events. It contains unsafe invariants not fully documented as `SAFETY` comments; validating them would require reviewing unsafe-libyaml's allocation, scanner/parser, and failure paths. The in-memory Miri sample exercises part of this boundary but is not exhaustive. No “unsound” verdict is assigned without a demonstrated contract violation.

Native FFmpeg/graphics/OS code and unsafe internals of other image/GUI dependencies remain **unverified**. Geiger's failed run is not used to infer their safety.

## 7. Coverage and limits

### Reviewed in depth

- All of `src/converter.rs`: classification/target restrictions, filename planning, output dispatch, image decode/normalization/encoding, data parsers and serializers, CSV inference/export, FFmpeg construction/result handling, and all four tests.
- All of `src/main.rs`: initialization, dialogs/drop paths, recursive import, UI gating, planning/scheduling, mutex/channel lifetimes, counters/statuses, output reveal, and shutdown ownership.
- `Cargo.toml`, lockfile source/version/advisory relationships, README, and conversion/development documentation relevant to promised behavior.
- Targeted reproductions of CC-01/02/03/05/06/07 and MIN-01; parser-level Miri cases described above.

### Concurrency and lifecycle conclusions

- There is one first-party queue mutex. Its guard ends after the `let Some(job) = ... else ...;` statement, before conversion. No lock-order cycle, mutex-held-across-await, async runtime, or first-party interior-mutability data race was found.
- Queue entries and worker indices remain stable while running because editing/removal/import is gated. Internal channel messages are not an attacker-accessible IPC protocol. The shared queue is an Arc without a reference cycle visible in first-party code.
- Detached worker handles are not joined or cancelled. A worker panic would drop its sender without a Finished message; disconnected handling only clears `rx` (`src/main.rs:185-187`), leaving stale status/progress. No concrete file that panics a decoder was found, so this is a robustness coverage gap, not a separately confirmed exploit.
- There is no cooperative shutdown for FFmpeg children (`src/main.rs:158-167`, `src/converter.rs:337-342`). Closing the application may leave a child to continue until it exits or encounters closed pipes, depending on OS/child behavior. A close-during-conversion test was not performed. Cancellation/process-group ownership should be designed before adding cancel controls.
- Filesystem operations in import/planning and startup's FFmpeg probe run on the UI thread. Conversion I/O and child waiting run on bounded worker threads, so they are not blocking an async executor. Existing workers nevertheless lack resource/deadline controls (CC-05).

### Other security categories checked

- **Command injection:** first-party commands use `Command` argument APIs, not a shell, and encoding arguments come from fixed strings. No shell-metacharacter injection found. Input path follows `-i`. Bare executable resolution remains a trusted-environment assumption. Relative paths beginning with option-like text and all platform reveal edge cases were not dynamically exercised.
- **Path traversal:** targets are fixed UI values and the output stem comes from `Path::file_stem`; no untrusted arbitrary target string reaches the public-in-module helper through the UI. The actual problems are links, identity, races, and unintended traversal, not a demonstrated `../../` string injection into `target`.
- **Deserialization:** data is parsed into ordinary `serde_json::Value`, not executed as code or deserialized into a custom gadget/plugin system. Allocation, aliases, lossy representations, duplicate keys, and output formula interpretation remain relevant.
- **Crypto/secrets/TLS/auth/SQL/templates:** no first-party implementation or hardcoded secret was found. No timing/nonce/RNG/zeroization finding is applicable to this application's own functionality. No TLS-verification disabling was present in first-party source. This does not audit FFmpeg's network stack.
- **Drop/error handling:** JPEG buffering was a real unchecked Drop-side-effect path (CC-06). Most other fallible conversion operations use `?`; error suppression and destructive cleanup were separately identified. The converter's `pub` functions are inside a private binary module, so they are not a published semver-stable library API. They would need stronger validated types/preconditions before reuse as a public library.

### Dependency code sampled / skimmed

- `image 0.25.10` reader/decoder limits and format dispatch; JPEG encoder finalization.
- `serde_yaml 0.9.34+deprecated` recursion/alias protections, parser unsafe wrapper, and owned-storage utility.
- `atspi-common`, `zbus-lockstep`, and `zbus-lockstep-macros` for old quick-xml reachability. Macro source reads local XML and emits validation-related code; no malicious behavior was established in the inspected path.
- `rfd 0.15.4/build.rs`: platform linking/backend checks; no suspicious behavior in that file.
- Build-script/proc-macro target inventory, registry source checks, cached yank flags, and RustSec/deny results across the lockfile.

### Not reviewed or not proven

- Full source/safety audit of 490 registry dependencies, 87 build-script packages, 41 proc-macro packages, generated bindings, Rust stdlib, graphics drivers, native desktop libraries, FFmpeg codecs/demuxers/protocols, or spreadsheet software.
- Release/optimized builds, 32-bit targets, Linux/macOS runtime behavior, cross-compilation success, GUI automation, accessibility/portal IPC adversarial testing, and platform file-manager edge cases.
- Exhaustive decoder fuzzing; long-running fuzz campaigns; actual OOM/disk-full/process-kill stress tests; deliberately crashing the GUI with cyclic trees; exploitability of each upstream unsafe site/advisory.
- No newly added test or source patch was committed. Diff sketches were not compiled as fixes and should not be applied without completing their helper/API designs.

The absence of a first-party `build.rs` or git dependency is verified. The absence of suspicious behavior in every transitive build script is **not** claimed.

## 8. Recommendations

### Prioritized remediation

1. **Unify output writing under an owned transaction abstraction.** Enforce atomic no-clobber, preserve pre-existing outputs on all failures, protect all source identities, and explicitly flush before reporting Done. Handle Windows case/alias behavior and same-directory atomic replacement semantics.
2. **Bound work before and during parsing/conversion.** Add import depth/file/time limits, link policy, data expansion budgets, CSV row/column/cell bounds, strict image dimension limits, aggregate memory/concurrency control, and bounded FFmpeg output capture/deadlines.
3. **Make traversal and lifecycle cancellable.** Move recursive import/planning off the UI thread; own/join worker and child-process lifetimes; report panics/failures consistently without silently leaving Running entries.
4. **Address export fidelity and spreadsheet policy.** Reject ambiguous duplicate fields where appropriate; use explicit string/numeric schema options; provide tested spreadsheet-safe export semantics.
5. **Repair and continuously verify dependency health.** Update transitive owners of vulnerable quick-xml, evaluate a maintained YAML implementation, set an intentional deny policy, and keep the lockfile reviewed/committed.

### Fuzz targets and meaningful regression tests

- **`data_json_to_value`** — `serde_json::from_str::<Value>` at `src/converter.rs:195`: duplicate keys, deep structures, extreme numbers, malformed Unicode; assert bounded failure/success without panic.
- **`data_yaml_to_value`** — `serde_yaml::from_str::<Value>` at `:196`: aliases, repeated large scalars, cycles, undefined anchors, complex/non-string keys, tags, invalid UTF-8 at the input boundary, and malformed event transitions. Enforce expanded-node/byte budgets and run representative seeds in Miri for any retained unsafe backend.
- **`data_toml_to_value`** — `toml::from_str::<Value>` at `:197`: nested/dotted tables, duplicate definitions, huge arrays/strings, datetime and numeric representation compatibility; round-trip invariants only where the conversion contract promises them.
- **`csv_read`** — `read_csv` at `:220`: duplicate/empty headers, huge fields/records, quote/newline combinations, unequal widths, large integers, exponent forms, empty fields; assert explicit fidelity and size policies.
- **`csv_write`** — `write_csv` at `:255`: disjoint-key sparse rows, heterogeneous objects, nested values, dangerous header/value prefixes; assert cell/output budgets before large allocation.
- **`image_decode_reencode`** — `ImageReader::with_guessed_format().decode()` at `:151-155`, then normalization and target encoders: malformed headers/metadata, extreme dimensions, format-vs-extension mismatches, 16-bit inputs, and encoder-size limitations. Run in a memory/time-limited process; include the 65,536×1 PNG-to-JPEG sentinel-preservation regression.
- **`output_transaction_properties`** — exercise `plan_output`/future commit API with fake and real filesystems: case collisions, hard links, symlinked parents, dangling links, cross-job input cycles, existing files arriving after planning, concurrent instances, permission failures, full-disk/short-write/flush faults. Assert old data survives failure and no protected input is changed.
- **`folder_import_graph`** — bounded generated directory graphs and platform links/junctions for `App::add_path`/replacement importer. Assert termination, file caps, identity deduplication, cancellation, error visibility, and UI responsiveness.
- **`ffmpeg_media_boundary`** — a subprocess corpus of malformed media and format-mismatch/playlist inputs, using a pinned/tested FFmpeg build inside time/memory/network limits. Assert failure cannot delete old outputs and application shutdown/cancellation reaps children. Do not treat cargo-fuzz as an in-process memory-safety auditor for the external FFmpeg executable.

Use fresh per-test temporary directories. Prefer bounded adversarial regression cases over crashing the main test process or actually filling storage. The existing four happy-path tests do not cover these invariants.

### CI checks and lints

- Run `cargo check --locked --all-targets --all-features`, `cargo test --locked --all-features`, and Clippy with the reviewed warning set on supported platforms. Ensure at least one CI lane actually installs FFmpeg and fails if media tests would skip.
- Add `#![forbid(unsafe_code)]` to the first-party crate while it remains entirely safe Rust. This constrains first-party/generated usage, not unsafe inside dependencies.
- Enforce `unused_must_use`, and gate new production `unwrap`/`expect`/`panic`/unchecked indexing with narrow, documented exceptions for proven invariants. Do not blindly deny every pedantic/test warning; resolve or justify specific ones. Enable `unsafe_op_in_unsafe_fn` and documented unsafe-block checks if unsafe code is ever introduced under a deliberately changed policy.
- Run `cargo audit` and a configured `cargo deny check` regularly; explicitly track all-target advisories separately from confirmed runtime exposure. Do not blanket-ignore unmaintained/advisory classes.
- Keep a reproducible lockfile and use `--locked` for binary builds. Review feature growth, build/proc-macro changes, source changes, and optional decoder backends. The default `image` feature set is broader than the UI's advertised input list because content guessing is used.
- Run pure parser Miri tests on a supported platform without native GUI/filesystem dependencies where possible. Separate backend logic into a testable library only as a future development change; it was not done for this audit.
- Run sanitizer/fuzz jobs and platform filesystem integration tests on schedule. A successful compiler, Clippy, or Miri sample does not replace output-integrity/race tests.

**Review completion:** all first-party Rust modules were reviewed. Repository source/manifest/lockfile preservation was checked with Git; only this report is intended as a tracked change.
