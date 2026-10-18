# User guide

## Run the application

Install a current stable Rust toolchain and native build tools, then run from the repository root:

```sh
cargo run --release --locked
```

The package uses Rust edition 2024, requiring at least Rust 1.85; dependencies may impose additional toolchain or platform requirements. The source includes file-manager integration for Windows, macOS, and other Unix desktops, but this does not establish that every platform has been tested.

For audio/video support, install FFmpeg with the codecs listed in the [format reference](formats.md), add its executable directory to `PATH`, and verify `ffmpeg -version`. The top bar shows **ffmpeg ✔** or **ffmpeg missing**. This check happens once at startup, so restart after installing FFmpeg or changing the launch environment.

## Add files and folders

- **Add files…** opens a multiple-file picker.
- **Add folder…** recursively visits the selected directory.
- Drag files or folders onto the window, or click the empty drop area to browse.

Directory entries are sorted before recursive processing. Files with unrecognized extensions are skipped, and a skipped-file count is shown alongside category controls when the queue is nonempty. Adding an identical path already in the queue has no effect. Paths are not canonicalized, so alternate paths to the same file are not reliably deduplicated.

Folder traversal follows paths reported as directories; it has no explicit symlink-cycle protection. Avoid importing directory trees with cyclic directory links. Unreadable directories or entries may be silently omitted.

## Select targets and options

Each row provides a **Convert to** menu with targets for its category. **Convert all** provides a separate menu for each category present in the queue. It changes targets, not conversion status.

The initial target is the first supported target that differs from the input extension, accounting for the `jpeg`/`jpg`, `tif`/`tiff`, and `yml`/`yaml` aliases.

- **Next to source file:** write into each input's parent directory (the default).
- **Choose folder…:** put all outputs directly in one folder; the source directory hierarchy is not reproduced.
- **JPEG quality:** 10–100, default 90; affects JPG output only.
- **Overwrite existing files:** disabled by default. Enable only if replacing existing output files is acceptable.

With overwriting disabled, an existing `photo.png` leads to names such as `photo (1).png`, `photo (2).png`, and so on. The planner reserves names conservatively across the batch and protects every queued input, including case and hard-link aliases. Destination links and special files are skipped even when overwriting is enabled.

Conversions are written to temporary files on the output filesystem and published only after successful encoding and checked writes. A file that appears after planning is not overwritten: that job fails, and retrying chooses a new name. Existing destinations selected for replacement are identity-checked again. If a queued source is missing or inaccessible, remove that row or restore access before starting the batch.

Use an output directory you trust. The application checks for changed directory identities but does not fully isolate pathname operations from hostile processes replacing ancestor directories at the same instant. Atomic publication also does not guarantee durability through every power failure; keep backups of important files.

## Convert and inspect results

Click **Convert** to process pending and failed entries. Successfully completed entries are skipped. Conversion uses at most four background workers; most queue and option controls are disabled until the batch finishes, and dropped files are not added while running.

Statuses are **Pending**, **Converting…**, **Done**, or **Failed**. Hover over **Failed** for its error text. Click **Done** to select the output in Explorer or Finder; on other Unix desktops the containing folder is opened with `xdg-open`.

The progress bar counts finished jobs, including failures; it is not a byte-level or per-file encoding progress indicator. Media entries rejected because FFmpeg is missing are not included in the scheduled-job count.

- **Clear finished** removes successful rows without deleting files.
- **Clear all** removes all rows and resets the skipped-file count without deleting files.
- The row's **✖** removes that entry without deleting its source or output.

To reconvert a completed source, remove its row and add it again. Changing a completed row's target alone does not schedule another conversion. There is no cancel control, settings persistence, or saved queue.

## Troubleshooting

### FFmpeg is missing or an encoder is unavailable

Run `ffmpeg -version` in the launch environment and restart ConvertCraft. A successful availability check does not guarantee that codecs such as `libx264`, `libvpx-vp9`, `libmp3lame`, `libvorbis`, or `libopus` are installed. Inspect `ffmpeg -encoders` if conversion reports a missing encoder. Audio extraction also requires an audio stream in the source.

### A file is skipped or fails to decode

Category selection uses the filename extension, not content inspection. Check the [recognized extensions](formats.md) and make sure the contents match a supported format. Renaming an extension alone does not convert a file.

### TOML or CSV conversion fails or changes values

TOML cannot represent null values. CSV output requires an object or an array of objects. CSV input infers numbers and booleans, treats empty fields as null, and can lose leading zeroes in identifiers. See the [data rules](formats.md#structured-data).

### Output cannot be written

Check destination permissions, available space, and whether another application has locked the output. Directory creation and write/flush errors are reported. Failed conversions clean up their staging files rather than deleting or truncating an existing destination. If the error says a destination appeared or changed, retry to replan the output name. Forced termination can leave a `.convertcraft-*` staging directory in the destination folder.

### An animation becomes a still image

Image conversion decodes one image and does not preserve animation frames. Use a recognized video input with GIF output for the FFmpeg animation path; GIF inputs themselves are categorized as images.
