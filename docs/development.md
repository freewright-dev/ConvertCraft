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
- `src/converter.rs`: extension/category detection, target defaults, output planning, image/data/media backends, and unit tests.
- `assets/logo.png`: embedded brand mark shared by the window icon, app header, empty queue, and README. The UI texture is uploaded once at startup and retained by `App`.
- `docs/`: user-facing behavior and developer reference.

The project is a single binary crate with an internal `converter` module. It does not expose a library API, HTTP server, or conversion CLI.

## Dependencies

- `eframe`/`egui`: native desktop UI and rendering.
- `rfd`: native file and folder dialogs.
- `image`: image decoding, normalization, resizing, and encoding.
- `serde_json`, `serde_yaml`, `toml`, `csv`: structured data parsing and serialization.
- `anyhow`: backend errors with context.
- Standard-library threads, channels, locks, and subprocesses: job execution and FFmpeg integration.

## Conversion lifecycle

### Import and state

`App` holds queued `FileItem`s, output directory, conversion `Options`, startup FFmpeg availability, worker message receiver, counters, and skipped-file count. A `FileItem` contains its path, detected `Category`, target, and `Status`.

`add_path` recursively imports directories, sorts child paths, skips exact-path duplicates, and categorizes files by extension. It increments the skipped count for unsupported extensions. Paths are not canonicalized, directory read failures are not surfaced, and symlink cycles are not tracked.

### Planning

`App::start` ignores completed rows and marks media rows failed when FFmpeg was unavailable at startup. For each remaining row, `plan_output` chooses a source-adjacent or custom-directory path, adding numbered suffixes when needed.

A shared `HashSet<PathBuf>` reserves output names across the batch. The planner avoids the exact source path and, unless overwriting is enabled, existing destination paths. Reservations are lexical and batch-local: they do not detect all aliases or prevent races with external processes.

### Workers and UI updates

Jobs are reversed into an `Arc<Mutex<Vec<Job>>>` so workers popping from the end begin in list order. The pool size is available parallelism capped at four, with a fallback of two if querying parallelism fails.

Each worker sends `Started(index)` and `Finished(index, result)` through an MPSC channel and requests an egui repaint. `App::poll` consumes messages without blocking, updates row status, and counts finished jobs. Channel disconnection ends the running state. Job completion order is not guaranteed.

`Status` transitions are normally `Pending` → `Running` → `Done(output)` or `Failed(error)`. Failed rows can be rescheduled; completed rows remain excluded even if their target changes. Queue mutations are disabled during execution so message indices remain valid.

There are no cancellation tokens, explicit worker join handles, per-file progress callbacks, or custom shutdown coordination. Panics inside workers are not translated into normal per-file errors.

### Backend dispatch

`convert` attempts to create the output parent directory and dispatches by category:

- Images decode into `DynamicImage` and encode using the selected target.
- Data passes through a `serde_json::Value` intermediate representation.
- Audio/video invokes FFmpeg with fixed target-specific arguments.

The backend's `convert` function does not call the output planner. Any future caller must plan or validate safe output paths itself. `Options::overwrite` is used by the UI's planning step; it is not enforced by image/data writers or the FFmpeg process.

Files are written directly to their destinations, not through temporary files with atomic renames. FFmpeg failures attempt output cleanup; image/data failures do not provide equivalent cleanup.

## Tests

The four tests live in `src/converter.rs`:

- `image_to_every_target`: generates a 300×200 RGBA image, converts it to every image target, and reopens each result.
- `data_round_trips`: converts representative JSON rows to every data target and back, checking selected numeric/boolean values and accounting for the TOML `items` wrapper.
- `output_paths_never_collide`: checks source-path avoidance and reservations for repeated output targets.
- `media_via_ffmpeg`: generates a one-second video/audio fixture, converts every video target, then converts extracted WAV audio to every audio target.

The media test returns early if FFmpeg is unavailable, so a passing `cargo test` alone does not prove media conversions ran. When FFmpeg is available, its build must include the required encoders and `lavfi` test sources.

Tests write fixtures into a shared `converter-tests` directory under the OS temporary directory and do not clean them up. Separate simultaneous test runs can collide. There are no automated UI tests, platform integration tests, exhaustive malformed-input tests, or exhaustive data-fidelity checks.

## Extending the converter

For a new recognized input extension, update `detect` and, if needed, `normalize_ext`. For a new output format, update `Category::targets` and the relevant image/data conversion branch or FFmpeg preset. Ensure the decoder/encoder is actually available rather than merely exposing a new menu entry.

Add tests for conversion and expected losses or failure cases, then update [formats.md](formats.md), the [README](../README.md), and [CHANGELOG.md](../CHANGELOG.md). Keep display branding consistent with `main.rs` and package naming consistent with `Cargo.toml`.

For reliability improvements, pay particular attention to filesystem aliases and output races, atomic writes, symlink-aware traversal, worker panic/shutdown behavior, and preserving meaningful errors. These are limitations of the current implementation, not implemented guarantees.
