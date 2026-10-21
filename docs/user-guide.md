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

Imports run in the background: the top bar shows **Scanning… N files found** with a **Cancel** button, and queue controls and **Convert** are disabled until the scan finishes. Directory entries are visited in sorted order. Files with unrecognized extensions are skipped. Adding an identical path already in the queue has no effect. Paths are not canonicalized, so alternate paths to the same file are not reliably deduplicated.

Symbolic links and junctions **inside** a folder are not followed, so link cycles cannot hang the import and links cannot pull in files from elsewhere. A folder or file you pick or drop yourself is used even if it is a link. Folders nested more than 64 levels deep are skipped, and a single import stops after 100,000 files or 20,000 folders. Special files (devices, pipes, sockets) are reported as errors instead of being queued.

After an import, the top bar summarizes what was not queued: unsupported files, links not followed, folders nested too deeply, a stop message if a limit was hit, and the number of import errors. Hover the error count to see the affected paths.

## Select targets and options

Each row provides a **Convert to** menu with targets for its category. **Convert all** provides a separate menu for each category present in the queue. It changes targets, not conversion status.

The initial target is the first supported target that differs from the input extension, accounting for the `jpeg`/`jpg`, `tif`/`tiff`, and `yml`/`yaml` aliases.

- **Next to source file:** write into each input's parent directory (the default).
- **Choose folder…:** put all outputs directly in one folder; the source directory hierarchy is not reproduced.
- **JPEG quality:** 10–100, default 90; affects JPG output only.
- **Overwrite existing files:** disabled by default. Enable only if replacing existing output files is acceptable.
- **Safe CSV for spreadsheets:** enabled by default. CSV cells that start with `=`, `+`, `-`, `@`, tab, or carriage return get an apostrophe prefix so Excel, LibreOffice, or Google Sheets do not run them as formulas. Disable it when the CSV must contain those values unchanged.

With overwriting disabled, an existing `photo.png` leads to names such as `photo (1).png`, `photo (2).png`, and so on. The planner reserves names conservatively across the batch and protects every queued input, including case and hard-link aliases. Destination links and special files are skipped even when overwriting is enabled.

Conversions are written to temporary files on the output filesystem and published only after successful encoding and checked writes. A file that appears after planning is not overwritten: that job fails, and retrying chooses a new name. Existing destinations selected for replacement are identity-checked again. If a queued source is missing or inaccessible, only that row fails; the rest of the batch still converts. A source that was replaced or edited after you pressed **Convert** fails with `source changed since the batch started`; press **Convert** again to retry it. On network shares or file systems that do not report file IDs, overwriting chooses a numbered name instead of replacing an existing file.

Use an output directory you trust. The application checks for changed directory identities but does not fully isolate pathname operations from hostile processes replacing ancestor directories at the same instant. Atomic publication also does not guarantee durability through every power failure; keep backups of important files.

## Convert and inspect results

Click **Convert** to process pending and failed entries. Successfully completed entries are skipped. Conversion uses at most four background workers; most queue and option controls are disabled until the batch finishes, and dropped files are not added while running.

Statuses are **Pending**, **Converting…**, **Done**, or **Failed**. Hover over **Failed** for its error text. Click **Done** to select the output in Explorer or Finder; on other Unix desktops the containing folder is opened with `xdg-open`.

The progress bar counts finished jobs, including failures; it is not a byte-level or per-file encoding progress indicator. Media entries rejected because FFmpeg is missing are not included in the scheduled-job count.

- **Clear finished** removes successful rows without deleting files.
- **Clear all** removes all rows and resets the import summary without deleting files.
- The row's **✖** removes that entry without deleting its source or output.

To reconvert a completed source, remove its row and add it again. Changing a completed row's target alone does not schedule another conversion. Click **Cancel** next to **Convert** to stop a running batch: no new jobs start, FFmpeg jobs stop immediately, image and data jobs already running finish, and unfinished rows are marked `cancelled`. Closing the window during a batch cancels it the same way. There is no settings persistence or saved queue.

## Troubleshooting

### FFmpeg is missing or an encoder is unavailable

Run `ffmpeg -version` in the launch environment and restart ConvertCraft. A successful availability check does not guarantee that codecs such as `libx264`, `libvpx-vp9`, `libmp3lame`, `libvorbis`, or `libopus` are installed. Inspect `ffmpeg -encoders` if conversion reports a missing encoder. Audio extraction also requires an audio stream in the source.

### A file is skipped or fails to decode

Category selection uses the filename extension, not content inspection. Check the [recognized extensions](formats.md) and make sure the contents match a supported format. Renaming an extension alone does not convert a file.

Images are decoded according to their contents. `unsupported image format` means the file is really a format ConvertCraft does not read (for example OpenEXR saved as `.png`). `image is too large to convert` means it exceeds the 100-megapixel limit.

### TOML or CSV conversion fails or changes values

TOML cannot represent null values. CSV output requires an object or an array of objects. CSV input infers numbers and booleans and treats empty fields as null; integers with leading zeros or beyond 64 bits stay text. Duplicate keys or CSV headers are rejected. Files over 64 MiB, or documents that expand beyond the memory limit, are refused. See the [data rules](formats.md#structured-data).

### Output cannot be written

Check destination permissions, available space, and whether another application has locked the output. Directory creation and write/flush errors are reported. Failed conversions clean up their staging files rather than deleting or truncating an existing destination. If the error says a destination appeared or changed, retry to replan the output name. Forced termination can leave a `.convertcraft-*` staging directory in the destination folder.

### An animation becomes a still image

Image conversion decodes one image and does not preserve animation frames. Use a recognized video input with GIF output for the FFmpeg animation path; GIF inputs themselves are categorized as images.
