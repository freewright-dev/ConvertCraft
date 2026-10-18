# Changelog

This changelog records the current implementation. `Cargo.toml` declares version `0.1.0`, but the repository has no commits or release tags from which to establish a release date or historical changes.

## Unreleased

### Changed

- Renamed the Cargo package and executable from `transmute` to `convertcraft`.
- Replaced the logo with an orange-and-blue conversion-arrow mark, shared by the window icon, app header, empty queue, and README.
- Separated branding from queue controls and rewrote the README with setup, workflow, supported formats, and conversion limitations.

### Added

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
