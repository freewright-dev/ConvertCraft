# Formats and conversion behavior

The extension lists and targets below come from `Category::targets` and `detect` in [`src/converter.rs`](../src/converter.rs). Extensions are compared case-insensitively. Recognition does not guarantee that a malformed file or a codec unsupported by the local backend can be converted.

## Images

**Input extensions:** `png`, `jpg`, `jpeg`, `webp`, `bmp`, `gif`, `tif`, `tiff`, `ico`, `tga`, `qoi`, `pnm`, `pbm`, `pgm`, `ppm`.

**Output targets:** `png`, `jpg`, `webp`, `bmp`, `gif`, `tiff`, `ico`, `tga`, `qoi`.

The `image` crate opens the input, guesses its format, and decodes a single image.

- **JPG:** converts to 8-bit RGB and encodes at the selected quality (90 by default). Alpha is discarded, not composited over a configurable background.
- **PNG:** saves the decoded image as PNG without the explicit RGBA8 normalization used by the other non-JPEG targets.
- **ICO:** resizes images exceeding 256 pixels in either dimension to fit within 256×256 while preserving aspect ratio, using Lanczos3 filtering.
- **Other targets:** normalize to 8-bit RGBA before saving. Target encoders may further restrict color or transparency.

There are no controls for image dimensions, metadata, color profiles, WebP quality, or animation preservation. Do not assume conversion preserves metadata, original bit depth, or all animation frames.

## Structured data

**Input extensions:** `json`, `yaml`, `yml`, `toml`, `csv`.

**Output targets:** `json`, `yaml`, `toml`, `csv`.

Inputs are read as UTF-8 text and parsed into a `serde_json::Value`. The target serializer then writes that value. Original formatting, comments, and format-specific features are not preserved; conversions are not guaranteed to be lossless.

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
2. Valid signed 64-bit integer → number.
3. Valid finite floating-point number → number.
4. Exact lowercase `true` or `false` → boolean.
5. Anything else → string.

For example, `00123` becomes the number `123`; quoting a CSV field does not disable inference. Duplicate header names collapse into object keys rather than remaining separate columns.

### CSV output

The root must be one object or an array containing only objects. Headers are the union of row keys encountered during iteration. With the current `serde_json` configuration, object keys use sorted map order, not guaranteed source insertion order.

- Missing keys and nulls become empty fields.
- Strings are written directly, with CSV quoting as needed.
- Numbers and booleans use their JSON text representation.
- Nested objects and arrays become JSON text inside a field, not flattened columns.

Nested JSON text is not automatically parsed back into nested values when reading CSV. Empty strings, nulls, and missing values can become indistinguishable. CSV output is not sanitized against spreadsheet formula interpretation; treat untrusted values cautiously when opening it in a spreadsheet program.

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
ffmpeg -y -hide_banner -loglevel error -i INPUT [target preset arguments] OUTPUT
```

Paths and options are passed as separate process arguments, not interpolated into a shell command. On Windows the subprocess uses `CREATE_NO_WINDOW`.

The `-y` flag always permits FFmpeg to overwrite the planned output path. The application's output planner avoids existing paths when overwriting is disabled, but this is not an atomic filesystem guarantee. On nonzero exit, the backend attempts to delete the output and reports the last nonempty stderr line.
