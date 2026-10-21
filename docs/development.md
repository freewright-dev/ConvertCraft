# Development and architecture

## Build and run

The Cargo package is `convertcraft`, version `0.1.0`, using Rust edition 2024. Use a current stable Rust toolchain and native platform build tools. A graphical desktop is needed to run the application. FFmpeg is an optional runtime dependency, not a bundled Cargo dependency.

```sh
cargo run --release --locked
cargo build --release --locked
cargo test --locked --workspace
```

Build artifacts are written to `target/`, which is ignored by Git. Release executables are `target/release/convertcraft` or `target/release/convertcraft.exe`. Windows release builds use the Windows GUI subsystem, so no console window is opened for the main application.

The source includes Windows, macOS, and Unix file-manager integration. Platform-specific build dependencies and a cross-platform test matrix are not defined in this repository.

## Source layout

- `Cargo.toml`: package metadata, dependencies, and the workspace.
- `Cargo.lock`: resolved dependency versions.
- `src/main.rs`: native window, queue state, file import, controls, worker scheduling, progress, and file-manager integration.
- `src/converter.rs`: extension/category detection, target defaults, image/data/media backends, and unit tests.
- `src/output.rs`: batch input identity protection, destination planning, staged output publication, and filesystem regression tests.
- `crates/file-id/`: `convertcraft-file-id`, a small Windows-only crate that safely wraps the one Win32 call std does not expose (`GetFileInformationByHandleEx` with `FileIdInfo`). It holds the project's only `unsafe` code, so the application crate can forbid it.
- `assets/logos/`: transparent logo (`logo.png`) and light/dark previews (`logo-light.png`, `logo-dark.png`). Only `logo.png` is embedded and shared by the window icon, app header, empty queue, and README. The UI texture is uploaded once at startup and retained by `App`.
- `docs/`: user-facing behavior and developer reference.

The project is a binary crate with internal `converter` and `output` modules, plus the `convertcraft-file-id` helper crate in the same workspace. It does not expose a library API, HTTP server, or conversion CLI.

## Dependencies

- `eframe`/`egui`: native desktop UI and rendering.
- `rfd`: native file and folder dialogs.
- `image`: image decoding, normalization, resizing, and encoding.
- `serde_json`, `serde-saphyr`, `toml`, `csv`: structured data parsing and serialization.
- `anyhow`: backend errors with context.
- `tempfile`: owned staging files/directories. `convertcraft-file-id` (Windows) and `libc` (Unix): file identity keys and non-blocking source opens.
- Standard-library threads, channels, locks, and subprocesses: job execution and FFmpeg integration.

## Conversion lifecycle

### Import and state

`App` holds queued `FileItem`s, output directory, conversion `Options`, worker message receiver, batch cancel flag and batch thread handle, counters, the running import (if any), and the import summary. A `FileItem` contains its path, detected `Category`, target, and `Status`.

`App::add_paths` starts `import::scan` on a background thread and receives its events over a channel; `poll_import` drains them each frame. The scan (`src/import.rs`) is an iterative depth-first walk in sorted order. Picked roots are followed even when they are links; links found inside folders (`FileType::is_symlink`, which on Windows covers symlinks and junctions but not cloud-file placeholders) are reported and skipped. `import::Limits` bounds depth (64), files (100,000), and folders (20,000). Unsupported extensions, skipped links, too-deep folders, read errors, and special files are reported as events and shown in the top-bar summary. Exact-path duplicates are dropped with a `HashSet`; paths are not canonicalized.

### Planning

`App::start` snapshots the queue into a `Batch` and runs it on a background thread, so slow network shares or crowded output folders never freeze the window; the queue stays locked from that point. The batch first constructs an `OutputPlanner` protecting all queued inputs, including completed rows. It ignores completed rows for conversion and fails media rows when FFmpeg was not found; rows that cannot be planned finish immediately with their error. FFmpeg is located once on a background thread at startup (`converter::ffmpeg_path`); **Convert** stays disabled until that check finishes. For each remaining row, the planner chooses a source-adjacent or custom-directory path, adding numbered suffixes when needed, up to 10,000 attempts per job.

The planner resolves parent directories, conservatively case-folds reserved names, and records each source's `FileKey` (device and inode on Unix; volume serial and 128-bit file ID on Windows). Keys are read and the file closed immediately, so a batch holds no open file per queued job. It protects input paths and hard-link identities, skips destination links/reparse points and special files, and records the key of any existing file selected for replacement.

`OutputPlanner::new` returns per-input errors instead of failing the batch: a source that cannot be inspected fails only its own row, and is ignored if that row is already done. Its names are still reserved so no output can take them.

Windows identities use the 128-bit `FILE_ID_INFO` ID rather than the 64-bit `nFileIndex`, which is not unique on ReFS. SMB shares backed by NTFS pass the same 128-bit ID through. A file system that cannot report a 128-bit ID, or reports 0 (some network and FAT redirectors), gives an unknown identity. Checks then fail safe: an existing destination with unknown identity is never replaced (a numbered name is used instead), and an existing file of unknown identity is treated as a possible source.

Before converting, `OutputPlan::open_source` opens the source once (non-blocking on Unix, so a FIFO cannot hang the worker), checks it is still a regular file with the planned identity, and the image/data backends read from that handle. FFmpeg reopens the path itself immediately after the same check. Directory creation failures are also reported.

### Workers and UI updates

Once planning finishes, the batch thread starts scoped workers and joins them. Jobs are reversed into a `Mutex<Vec<Job>>` so workers popping from the end begin in list order. The pool size is available parallelism capped at four, with a fallback of two if querying parallelism fails.

Each worker sends `Started(index)` and `Finished(index, result)` through an MPSC channel and requests an egui repaint. `App::poll` consumes messages without blocking, updates row status, and counts finished jobs. Channel disconnection ends the running state. Job completion order is not guaranteed.

`Status` transitions are normally `Pending` → `Running` → `Done(output)` or `Failed(error)`. Failed rows can be rescheduled; completed rows remain excluded even if their target changes. Queue mutations are disabled during execution so message indices remain valid.

Each batch has a cancel flag. **Cancel** sets it: workers stop taking jobs, running FFmpeg processes are killed within about 100 ms, and image/data jobs already running finish normally. Rows that never finished are marked `cancelled` (or `conversion did not finish` if workers exited for another reason) when the channel disconnects. A panic inside a conversion is caught with `catch_unwind` and reported as that row's `internal error`. Closing the window cancels the batch and waits up to 5 seconds for workers, so FFmpeg is stopped and staging files are cleaned up; an image or data job still running after that may leave a staging directory behind. There are no per-file progress callbacks.

### Backend dispatch

`convert` consumes an `OutputPlan`, creates an owned staging directory on the output filesystem, and dispatches by category:

- Images decode into `DynamicImage` and encode to the open staging file, with checked buffer flushing (including JPEG).
- Data passes through a `serde_json::Value` intermediate representation and writes to the open staging file.
- Audio/video invokes FFmpeg with fixed target-specific arguments and a staged filename retaining the target extension.

The planner captures overwrite policy in the plan; the backend cannot publish to an arbitrary destination path. Successful conversion returns the committed output path. All backends synchronize the staged file before publication. Failed conversion or publication drops staging files/directories and leaves previously published output intact.

Publication uses `persist_noclobber` when no existing destination was selected, even if overwriting was enabled. If a file appears after planning, the job fails safely; retrying replans the name. Existing destinations selected for replacement are identity-checked again, then replaced atomically. Batch publication is serialized so aliases missed by case folding are rechecked after preceding commits. Output directory identity is checked before staging and publication.

These pathname-based checks are not a capability sandbox: an attacker able to replace ancestor directories in the final check-to-rename interval can still race resolution. Existing-destination identity checking and replacement are not an OS-level compare-and-swap. Use trusted output directories; descriptor-relative publication for hostile directories remains follow-up hardening. File synchronization does not promise crash-durable directory entries on every filesystem, and forced termination may leave staging directories behind.

## Tests

Tests live in `src/converter.rs` and `src/output.rs`:

- `image_to_every_target`: generates a 300×200 RGBA image, converts it to every image target, and reopens each result.
- `data_round_trips`: converts representative JSON rows to every data target and back, checking selected numeric/boolean values and accounting for the TOML `items` wrapper.
- Filesystem regression tests cover late destination creation, competing publishers, batch source protection, case aliases, hard links, destination/directory replacement, staging cleanup, symbolic links, and aliased parent directories.
- `media_via_ffmpeg`: generates a one-second video/audio fixture, converts every video target, then converts extracted WAV audio to every audio target.
- Backend regressions verify sentinel preservation on malformed data/media and unsupported JPEG dimensions, successful replacement, cross-format batches, and propagation of buffered JPEG write errors.

The media tests return early if FFmpeg is unavailable, so a passing `cargo test` alone does not prove media conversions ran. Set `CONVERTCRAFT_REQUIRE_FFMPEG=1` to make them fail instead of skipping. When FFmpeg is available, its build must include the required encoders and `lavfi` test sources.

Each test owns a unique temporary directory that is cleaned up on completion. There are no automated UI tests, exhaustive malformed-input tests, or exhaustive data-fidelity checks. Platform behavior must be tested on the respective operating systems. On Windows, symlink tests print a skip diagnostic if Developer Mode/symlink privilege is unavailable; set `CONVERTCRAFT_REQUIRE_SYMLINKS=1` to make that a failure.

## Continuous integration

`.github/workflows/ci.yml` runs on pushes to `master` and on pull requests:

- `cargo fmt --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings` on Linux.
- `cargo test --locked --workspace` on Linux, Windows, and macOS with FFmpeg installed and both `CONVERTCRAFT_REQUIRE_*` variables set, so media and symlink tests cannot skip silently.
- `cargo deny check` using [`deny.toml`](../deny.toml): RustSec advisories, license allowlist, crates.io-only sources. Each ignored advisory names its reason.

The application crate root declares `#![forbid(unsafe_code)]`; the only `unsafe` code is the documented Win32 call in `crates/file-id`.

## Extending the converter

For a new recognized input extension, update `detect` and, if needed, `normalize_ext`. For a new output format, update `Category::targets` and the relevant image/data conversion branch or FFmpeg preset. Ensure the decoder/encoder is actually available rather than merely exposing a new menu entry.

Add tests for conversion and expected losses or failure cases, then update [formats.md](formats.md), the [README](../README.md), and [CHANGELOG.md](../CHANGELOG.md). Keep display branding consistent with `main.rs` and package naming consistent with `Cargo.toml`.

Further reliability work includes descriptor-relative operations in hostile directories.
