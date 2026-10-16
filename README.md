# Transmute

A local desktop file converter built with Rust and egui/eframe. This repository is named **ConvertCraft**; the application, Cargo package, and executable are named **Transmute** (`transmute`).

Convert images, structured data, audio, and video in batches without uploading files to a service. Image and data conversions run in Rust; audio and video conversions use a locally installed FFmpeg.

## Features

- Add files, recursively add a folder, or drag files and folders onto the window.
- Choose output formats per file or apply a target to an entire category.
- Run conversions on up to four background workers, with per-file status and batch progress.
- Save beside each source or send all outputs to a chosen folder.
- Avoid existing output names by default, with numbered suffixes for collisions.
- Adjust JPEG quality from 10 to 100 (default: 90).
- Reveal completed files in the system file manager.

## Getting started

Install a Rust toolchain with support for the 2024 edition (Rust 1.85 or newer; current stable recommended) and your platform's native build tools. Linux builds may also need desktop development libraries for eframe and native file dialogs.

From the repository directory:

```sh
cargo run --release --locked
```

For audio or video, install FFmpeg separately and make sure this works in the environment used to launch Transmute:

```sh
ffmpeg -version
```

Restart Transmute after changing `PATH`: FFmpeg availability is checked at startup. FFmpeg is optional for image and data conversions.

## Supported conversions

- **Images:** PNG, JPEG, WebP, BMP, GIF, TIFF, ICO, TGA, QOI, and PNM-family input; PNG, JPG, WebP, BMP, GIF, TIFF, ICO, TGA, and QOI output.
- **Data:** JSON, YAML, TOML, and CSV input/output.
- **Audio:** MP3, WAV, FLAC, OGG, M4A, AAC, Opus, WMA, and AIFF input; MP3, WAV, FLAC, OGG, M4A, and Opus output.
- **Video:** MP4, MKV, WebM, AVI, MOV, WMV, FLV, M4V, MPEG, and TS input; MP4, MKV, WebM, AVI, MOV, GIF, or MP3/WAV/M4A audio extraction.

Files are categorized by extension. Actual decoding and media encoding depend on file contents and the installed FFmpeg build. This is not an arbitrary any-format-to-any-format converter: targets are restricted to each category.

See [formats and conversion behavior](docs/formats.md) for exact extensions, encoding presets, and data compatibility rules.

## Basic workflow

1. Select **Add files…** or **Add folder…**, or drop files onto the window.
2. Choose each file's **Convert to** format, or use a category menu under **Convert all**.
3. Choose the output location and, optionally, JPEG quality or overwriting.
4. Click **Convert**. Hover over **Failed** for an error; click **Done** to reveal the output.

Successful entries are skipped on later runs. To convert one again, remove it from the list and add it again. Settings and the queue are not saved between launches.

## Documentation

- [Documentation index](docs/README.md)
- [User guide and troubleshooting](docs/user-guide.md)
- [Supported formats and conversion details](docs/formats.md)
- [Architecture, development, and testing](docs/development.md)
- [Changelog](CHANGELOG.md)

## Development

```sh
cargo build --release --locked
cargo test --locked
```

The release executable is `target/release/transmute` (`transmute.exe` on Windows). This is a desktop application; no conversion CLI is implemented. See the development guide for test coverage and limitations.
