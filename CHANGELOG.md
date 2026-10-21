# Changelog

This changelog records the current implementation. `Cargo.toml` declares version `0.1.0`, but the repository has no commits or release tags from which to establish a release date or historical changes.

## Unreleased

### Changed

- Renamed the Cargo package and executable from `transmute` to `convertcraft`.
- Replaced the logo with an orange-and-blue conversion-arrow mark, shared by the window icon, app header, empty queue, and README.
- Separated branding from queue controls and rewrote the README with setup, workflow, supported formats, and conversion limitations.

### Added

- **Cancel** button for running batches; FFmpeg jobs stop immediately (#22, #23).
- CI workflow: rustfmt, Clippy with `-D warnings`, tests on Linux/Windows/macOS with FFmpeg, and `cargo deny check` with a committed `deny.toml` policy (#20).
- `#![forbid(unsafe_code)]` at the crate root.
- `CONVERTCRAFT_REQUIRE_FFMPEG` / `CONVERTCRAFT_REQUIRE_SYMLINKS` test switches that turn skipped media/symlink tests into failures.
- ConvertCraft desktop interface built with egui/eframe.
- Supplied logo embedded as the window icon and displayed in the README.
- File selection, recursive folder import, drag-and-drop, and duplicate-path filtering.
- Image conversion to PNG, JPG, WebP, BMP, GIF, TIFF, ICO, TGA, and QOI, including JPEG quality control and ICO resizing.
- JSON, YAML, TOML, and CSV conversion, including CSV scalar inference and TOML wrapping for non-object roots.
- FFmpeg-backed audio/video conversion, video-to-GIF conversion, and audio extraction from video.
- Per-file and category-wide output format selection.
- Output beside sources or in a selected folder, optional overwriting, and batch output-path reservation with numbered collision suffixes.
- Background conversion with up to four workers, per-file errors, and completed-job progress.
- Completed-file reveal through Explorer, Finder, or `xdg-open`.
- Tests for image targets, structured data round trips, output-path collisions, and FFmpeg media conversions.
- README and user, format, and development documentation.

### Fixed

- A panic during a conversion fails only that row, and rows left unfinished by exiting workers are marked failed instead of staying "Converting…" forever. Closing the window stops FFmpeg and waits briefly for workers to clean up their staging files (#22).
- FFmpeg runs with a demuxer whitelist (no HLS/ffconcat playlists), `-protocol_whitelist file`, and `-nostdin`; a job with no progress for 5 minutes is stopped; stderr capture is capped at 64 KiB (#23).
- FFmpeg is located only in absolute `PATH` entries, and Explorer is launched by absolute path, so executables placed next to ConvertCraft are not run (#19).
- Output paths are shown and passed to Explorer without the `\\?\` prefix, and the Explorer argument is quoted so names containing commas select the right file (#25).
- The FFmpeg availability check runs on a background thread instead of blocking startup (#24).
- Batches no longer keep two open handles per queued job: source and destination identities are stored as keys, so large batches no longer run into the Unix open-file limit (#17).
- A missing, unreadable, or non-regular source fails only its own row instead of the whole batch; completed rows whose sources were deleted no longer block new conversions (#16).
- File systems that report no file ID are detected, and overwrite then uses a numbered name instead of replacing a file it cannot re-identify (#26).
- Sources are opened once and re-identified before conversion, so a file or FIFO swapped in after planning is rejected instead of converted or blocking a worker (#27).
- Images over 100 megapixels are rejected before decoding, and concurrent image jobs share a 1.5 GiB memory budget; a 260 KB PNG could previously drive one conversion to 1.35 GB of memory and a 1 GiB output (#14).
- Only the advertised image input formats are decoded. The `image` crate is built without its EXR, HDR, DDS, farbfeld, and AVIF support, and files sniffed as other formats are rejected (#21).
- Data conversion is bounded: 64 MiB input limit, a 768 MiB estimated memory budget enforced while parsing (stops YAML alias and wide-CSV-header expansion), output streamed to disk with a 1 GiB limit, and at most 16,384 CSV columns. The CSV header union is no longer quadratic (#13).
- CSV output neutralizes spreadsheet formulas by default (**Safe CSV for spreadsheets** option) (#15).
- Duplicate JSON/YAML keys and CSV headers are rejected instead of silently keeping the last value; CSV integers with leading zeros or beyond 64 bits stay exact text instead of becoming rounded numbers (#28).
- Folder import no longer follows links inside folders, runs on a background thread with a Cancel button, and is bounded by depth, file, and folder limits; a folder with links back to itself used to hang the app (#18).
- Import errors (unreadable folders or entries, special files) are now shown in the top bar instead of being silently dropped (#29).
- Enforce no-clobber when publishing new outputs, including files created after planning and case-only name collisions.
- Preserve existing destinations when image/data encoding or FFmpeg fails; publish successful conversions from owned staging files.
- Protect all queued source paths and file identities from overwrite-enabled batch outputs.
- Report JPEG buffered-write failures before marking a job successful.
- Isolate conversion test fixtures in unique temporary directories and surface output-directory creation errors.

### Known limitations

- No cancellation, persistent settings, saved queue, or conversion command-line interface.
- Media conversions require an external FFmpeg installation with the required codecs.
- Image conversion uses a single decoded image rather than preserving animation.
- Structured data conversion does not preserve comments, formatting, or every source-format type.
- Output publication is pathname-based; hostile ancestor-directory replacement in the final check-to-rename interval is not fully isolated.
