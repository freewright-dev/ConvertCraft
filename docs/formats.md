# Formats and conversion behavior

The extension lists and targets below come from `Category::targets` and `detect` in [`src/converter.rs`](../src/converter.rs). Extensions are compared case-insensitively. Recognition does not guarantee that a malformed file or a codec unsupported by the local backend can be converted.

## Images

**Input extensions:** `png`, `jpg`, `jpeg`, `webp`, `bmp`, `gif`, `tif`, `tiff`, `ico`, `tga`, `qoi`, `pnm`, `pbm`, `pgm`, `ppm`.

**Output targets:** `png`, `jpg`, `webp`, `bmp`, `gif`, `tiff`, `ico`, `tga`, `qoi`.

The `image` crate opens the input, identifies its format from the file contents (falling back to the extension), and decodes a single image. The contents decide the decoder, so a PNG named `.jpg` still converts, but only the input formats listed above are accepted: other formats the `image` crate can identify, such as OpenEXR, HDR, or DDS, are rejected with `unsupported image format`, and their decoders are not built into ConvertCraft.

Images larger than **100 megapixels** (for example 10,000×10,000) are rejected before their pixels are decoded. Normalizing to 8-bit RGB(A) needs up to 4 bytes per pixel on top of the decoded image, so all running image jobs share an estimated memory budget of 1.5 GiB; a job waits for memory instead of running alongside the others when the budget is full.

- **JPG:** converts to 8-bit RGB and encodes at the selected quality (90 by default). Alpha is discarded, not composited over a configurable background.
- **PNG:** saves the decoded image as PNG without the explicit RGBA8 normalization used by the other non-JPEG targets.
- **ICO:** resizes images exceeding 256 pixels in either dimension to fit within 256×256 while preserving aspect ratio, using Lanczos3 filtering.
- **Other targets:** normalize to 8-bit RGBA before saving. Target encoders may further restrict color or transparency.

There are no controls for image dimensions, metadata, color profiles, WebP quality, or animation preservation. Do not assume conversion preserves metadata, original bit depth, or all animation frames.

## Structured data

**Input extensions:** `json`, `yaml`, `yml`, `toml`, `csv`.

**Output targets:** `json`, `yaml`, `toml`, `csv`.

Inputs are read as UTF-8 text and parsed into a `serde_json::Value`. The target serializer then writes that value. Original formatting, comments, and format-specific features are not preserved; conversions are not guaranteed to be lossless.

Duplicate keys in JSON or YAML objects, and duplicate CSV header names, are rejected with a `duplicate key` / `duplicate CSV header` error instead of silently keeping one value.

**Size limits.** Data files larger than 64 MiB are rejected. While parsing, the estimated in-memory size of the document is capped at 768 MiB, which stops YAML aliases or very wide CSV headers from expanding a small file into gigabytes (`document expands beyond the 768 MiB memory limit`). Output is written directly to the staged file and capped at 1 GiB. CSV input and output are limited to 16,384 columns (the Excel column limit).

### JSON and YAML

JSON output is pretty-printed and ends with a newline. YAML uses `serde_yaml` serialization. Because the intermediate value is JSON-shaped, source features outside that data model may fail or change representation.

### TOML

An object root is serialized directly. Other roots are wrapped in an object under `items` to satisfy TOML's table-root requirement. For example, this JSON:

```json
[{"name": "sample", "enabled": true}]
```

becomes:

```toml
[[items]]
enabled = true
name = "sample"
```

Converting back to JSON retains the `items` wrapper; it is not automatically removed. Nulls and other TOML-incompatible values cause serialization to fail.

### CSV input

The first record supplies headers. Each remaining record becomes an object in an array. The default CSV reader expects consistent record lengths and a comma delimiter; there is no delimiter or header configuration in the UI.

Each field is inferred in this order:

1. Empty string → null.
2. Integer with a leading zero (`00123`, `-007`) → string, so identifiers keep their formatting.
3. Valid signed or unsigned 64-bit integer → number.
4. Other integer text (too large for 64 bits) → string, so it is not rounded.
5. Valid finite floating-point number (`0.5`, `1e3`) → number.
6. Exact lowercase `true` or `false` → boolean.
7. Anything else → string.

For example, `00123` stays the string `"00123"` and `18446744073709551615` stays an exact integer. Quoting a CSV field does not disable inference.

### CSV output

The root must be one object or an array containing only objects. Headers are the union of row keys encountered during iteration. With the current `serde_json` configuration, object keys use sorted map order, not guaranteed source insertion order.

- Missing keys and nulls become empty fields.
- Strings are written directly, with CSV quoting as needed.
- Numbers and booleans use their JSON text representation.
- Nested objects and arrays become JSON text inside a field, not flattened columns.

Nested JSON text is not automatically parsed back into nested values when reading CSV. Empty strings, nulls, and missing values can become indistinguishable.

**Safe CSV for spreadsheets** (on by default) prefixes headers and string cells that begin with `=`, `+`, `-`, `@`, tab, or carriage return with an apostrophe, so spreadsheet programs show them as text instead of evaluating them as formulas. Numbers are not changed. Turn the option off to write such values unmodified, for example when the CSV is consumed by another program rather than a spreadsheet.

## Audio

**Input extensions:** `mp3`, `wav`, `flac`, `ogg`, `m4a`, `aac`, `opus`, `wma`, `aiff`, `aif`.

**Output targets:** `mp3`, `wav`, `flac`, `ogg`, `m4a`, `opus`.

All audio target presets disable video with `-vn`:

- **MP3:** `libmp3lame`, quality `2`.
- **WAV:** FFmpeg's output-format defaults; no explicit audio codec is selected.
- **FLAC:** `flac` encoder.
- **OGG:** `libvorbis`, quality `5`.
- **M4A:** AAC at `192k`.
- **Opus:** `libopus` at `128k`.

## Video

**Input extensions:** `mp4`, `mkv`, `webm`, `avi`, `mov`, `wmv`, `flv`, `m4v`, `mpg`, `mpeg`, `ts`.

**Output targets:** `mp4`, `mkv`, `webm`, `avi`, `mov`, `gif`, `mp3`, `wav`, `m4a`.

- **MP4/MOV:** `libx264`, preset `medium`, CRF `23`, pixel format `yuv420p`; AAC at `160k`; `+faststart`.
- **MKV:** `libx264`, CRF `23`; AAC at `160k`.
- **WebM:** `libvpx-vp9`, CRF `32`, video bitrate `0`; `libopus` audio.
- **AVI:** `mpeg4`, video quality `5`; `libmp3lame`, audio quality `4`.
- **GIF:** no audio; 12 fps; width capped at 480 pixels with Lanczos scaling and an automatically computed even height; palette generation/use; infinite looping.
- **MP3/WAV/M4A:** use the audio presets above to extract audio.

These presets re-encode rather than requesting stream copying. There are no UI controls for bitrate, resolution, stream selection, trimming, subtitles, or codec choice. FFmpeg's default stream-selection behavior applies because no explicit `-map` is supplied.

### FFmpeg invocation and errors

The backend executes the equivalent of:

```text
ffmpeg -y -hide_banner -loglevel error -i INPUT [target preset arguments] STAGED_OUTPUT
```

Paths and options are passed as separate process arguments, not interpolated into a shell command. On Windows the subprocess uses `CREATE_NO_WINDOW`.

The `-y` flag permits FFmpeg to write only its owned temporary output, whose suffix matches the target format. On nonzero exit, the backend reports the last nonempty stderr line and staging is cleaned up. The final destination is published after success through the same no-clobber/replacement policy as image and data conversions; failed input parsing does not delete an old destination.
