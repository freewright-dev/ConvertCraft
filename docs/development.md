# Development and architecture

## Build and run

The Cargo package is `convertcraft`, version `0.1.0`, using Rust edition 2024. Use a current stable Rust toolchain and native platform build tools. A graphical desktop is needed to run the application. FFmpeg is an optional runtime dependency, not a bundled Cargo dependency.

```sh
cargo run --release --locked
cargo build --release --locked
cargo test --locked
```

Build artifacts are written to `target/`, which is ignored by Git. Release executables are `target/release/convertcraft` or `target/release/convertcraft.exe`. Windows release builds use the Windows GUI subsystem, so no console window is opened for the main application.

The source includes Windows, macOS, and Unix file-manager integration. Platform-specific build dependencies and a cross-platform test matrix are not defined in this repository.

## Source layout

- `Cargo.toml`: package metadata and dependencies.
- `Cargo.lock`: resolved dependency versions.
- `src/main.rs`: native window, queue state, file import, controls, worker scheduling, progress, and file-manager integration.
- `src/converter.rs`: extension/category detection, target defaults, image/data/media backends, and unit tests.
- `src/output.rs`: batch input identity protection, destination planning, staged output publication, and filesystem regression tests.
- `assets/logos/`: transparent logo (`logo.png`) and light/dark previews (`logo-light.png`, `logo-dark.png`). Only `logo.png` is embedded and shared by the window icon, app header, empty queue, and README. The UI texture is uploaded once at startup and retained by `App`.
- `docs/`: user-facing behavior and developer reference.

The project is a single binary crate with internal `converter` and `output` modules. It does not expose a library API, HTTP server, or conversion CLI.

## Dependencies

- `eframe`/`egui`: native desktop UI and rendering.
- `rfd`: native file and folder dialogs.
- `image`: image decoding, normalization, resizing, and encoding.
- `serde_json`, `serde_yaml`, `toml`, `csv`: structured data parsing and serialization.
- `anyhow`: backend errors with context.
- `tempfile`, `same-file`: owned staging files/directories and filesystem identity checks.
- Standard-library threads, channels, locks, and subprocesses: job execution and FFmpeg integration.

## Conversion lifecycle

### Import and state

`App` holds queued `FileItem`s, output directory, conversion `Options`, startup FFmpeg availability, worker message receiver, counters, and skipped-file count. A `FileItem` contains its path, detected `Category`, target, and `Status`.

`add_path` recursively imports directories, sorts child paths, skips exact-path duplicates, and categorizes files by extension. It increments the skipped count for unsupported extensions. Paths are not canonicalized, directory read failures are not surfaced, and symlink cycles are not tracked.

### Planning

`App::start` first constructs an `OutputPlanner` protecting all queued inputs, including completed rows. It ignores completed rows for conversion and marks media rows failed when FFmpeg was unavailable at startup. For each remaining row, the planner chooses a source-adjacent or custom-directory path, adding numbered suffixes when needed, up to 10,000 attempts per job.

The planner resolves parent directories, conservatively case-folds reserved names, and keeps source file-identity handles alive across the batch. It protects input paths and hard-link identities, skips destination links/reparse points and special files, and records the identity of any existing file selected for replacement. Failure to inspect/protect batch inputs stops scheduling and reports an error. Remove stale or inaccessible rows before retrying. Directory creation failures are also reported.

### Workers and UI updates

Jobs are reversed into an `Arc<Mutex<Vec<Job>>>` so workers popping from the end begin in list order. The pool size is available parallelism capped at four, with a fallback of two if querying parallelism fails.

Each worker sends `Started(index)` and `Finished(index, result)` through an MPSC channel and requests an egui repaint. `App::poll` consumes messages without blocking, updates row status, and counts finished jobs. Channel disconnection ends the running state. Job completion order is not guaranteed.

`Status` transitions are normally `Pending` → `Running` → `Done(output)` or `Failed(error)`. Failed rows can be rescheduled; completed rows remain excluded even if their target changes. Queue mutations are disabled during execution so message indices remain valid.

There are no cancellation tokens, explicit worker join handles, per-file progress callbacks, or custom shutdown coordination. Panics inside workers are not translated into normal per-file errors.

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

The media test returns early if FFmpeg is unavailable, so a passing `cargo test` alone does not prove media conversions ran. When FFmpeg is available, its build must include the required encoders and `lavfi` test sources.

Each test owns a unique temporary directory that is cleaned up on completion. There are no automated UI tests, exhaustive malformed-input tests, or exhaustive data-fidelity checks. Platform behavior must be tested on the respective operating systems. On Windows, symlink tests print a skip diagnostic if Developer Mode/symlink privilege is unavailable; ensure a CI lane has this capability.

## Extending the converter

For a new recognized input extension, update `detect` and, if needed, `normalize_ext`. For a new output format, update `Category::targets` and the relevant image/data conversion branch or FFmpeg preset. Ensure the decoder/encoder is actually available rather than merely exposing a new menu entry.

Add tests for conversion and expected losses or failure cases, then update [formats.md](formats.md), the [README](../README.md), and [CHANGELOG.md](../CHANGELOG.md). Keep display branding consistent with `main.rs` and package naming consistent with `Cargo.toml`.

Further reliability work includes descriptor-relative operations in hostile directories, bounded/symlink-aware folder traversal, conversion resource limits, and worker panic/shutdown handling.
