//! Conversion backends: images (pure Rust), structured data (pure Rust),
//! audio/video (delegated to an `ffmpeg` binary on PATH).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use image::{DynamicImage, ImageFormat, codecs::jpeg::JpegEncoder, imageops::FilterType};
use serde_json::{Map, Value};

use crate::output::OutputPlan;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Category {
    Image,
    Data,
    Audio,
    Video,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Image,
        Category::Data,
        Category::Audio,
        Category::Video,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Image => "Image",
            Category::Data => "Data",
            Category::Audio => "Audio",
            Category::Video => "Video",
        }
    }

    pub fn targets(self) -> &'static [&'static str] {
        match self {
            Category::Image => &[
                "png", "jpg", "webp", "bmp", "gif", "tiff", "ico", "tga", "qoi",
            ],
            Category::Data => &["json", "yaml", "toml", "csv"],
            Category::Audio => &["mp3", "wav", "flac", "ogg", "m4a", "opus"],
            Category::Video => &[
                "mp4", "mkv", "webm", "avi", "mov", "gif", "mp3", "wav", "m4a",
            ],
        }
    }

    pub fn needs_ffmpeg(self) -> bool {
        matches!(self, Category::Audio | Category::Video)
    }
}

pub fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn detect(path: &Path) -> Option<Category> {
    let cat = match extension(path).as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tif" | "tiff" | "ico" | "tga"
        | "qoi" | "pnm" | "pbm" | "pgm" | "ppm" => Category::Image,
        "json" | "yaml" | "yml" | "toml" | "csv" => Category::Data,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" | "wma" | "aiff" | "aif" => {
            Category::Audio
        }
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "ts" => {
            Category::Video
        }
        _ => return None,
    };
    Some(cat)
}

/// Pick a sensible default target: the first one that differs from the input format.
pub fn default_target(path: &Path, cat: Category) -> &'static str {
    let ext = extension(path);
    let ext = normalize_ext(&ext);
    cat.targets()
        .iter()
        .copied()
        .find(|t| *t != ext)
        .unwrap_or(cat.targets()[0])
}

fn normalize_ext(ext: &str) -> &str {
    match ext {
        "jpeg" => "jpg",
        "tif" => "tiff",
        "yml" => "yaml",
        other => other,
    }
}

#[derive(Clone)]
pub struct Options {
    pub jpeg_quality: u8,
    pub overwrite: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            jpeg_quality: 90,
            overwrite: false,
        }
    }
}

pub fn convert(
    input: &Path,
    output: OutputPlan,
    cat: Category,
    target: &str,
    opts: &Options,
) -> Result<PathBuf> {
    if !cat.targets().contains(&target) {
        bail!("unsupported {} target: {target}", cat.label());
    }
    let mut staged = output.begin()?;
    match cat {
        Category::Image => convert_image(input, staged.file_mut(), target, opts)?,
        Category::Data => convert_data(input, staged.file_mut(), target)?,
        Category::Audio | Category::Video => convert_media(input, staged.path(), target)?,
    }
    staged.commit()
}

// ---------------------------------------------------------------- images

fn convert_image(input: &Path, output: &mut File, target: &str, opts: &Options) -> Result<()> {
    let img = image::ImageReader::open(input)
        .context("cannot open image")?
        .with_guessed_format()?
        .decode()
        .context("cannot decode image")?;

    let mut writer = BufWriter::new(output);
    match target {
        "jpg" => {
            encode_jpeg(&img, &mut writer, opts.jpeg_quality)?;
        }
        "png" => img.write_to(&mut writer, ImageFormat::Png)?,
        _ => {
            let (format, img) = match target {
                "ico" => {
                    // ICO entries are limited to 256x256.
                    let img = if img.width() > 256 || img.height() > 256 {
                        img.resize(256, 256, FilterType::Lanczos3)
                    } else {
                        img
                    };
                    (ImageFormat::Ico, img)
                }
                "webp" => (ImageFormat::WebP, img),
                "bmp" => (ImageFormat::Bmp, img),
                "gif" => (ImageFormat::Gif, img),
                "tiff" => (ImageFormat::Tiff, img),
                "tga" => (ImageFormat::Tga, img),
                "qoi" => (ImageFormat::Qoi, img),
                other => bail!("unsupported image target: {other}"),
            };
            // Most encoders only accept 8-bit RGBA; normalize to it.
            DynamicImage::ImageRgba8(img.to_rgba8()).write_to(&mut writer, format)?;
        }
    }
    writer.flush().context("cannot flush image output")?;
    Ok(())
}

fn encode_jpeg(image: &DynamicImage, writer: &mut impl Write, quality: u8) -> Result<()> {
    {
        let mut encoder = JpegEncoder::new_with_quality(&mut *writer, quality);
        encoder.encode_image(&DynamicImage::ImageRgb8(image.to_rgb8()))?;
    }
    writer.flush().context("cannot flush JPEG output")?;
    Ok(())
}

// ---------------------------------------------------------------- data

fn convert_data(input: &Path, output: &mut File, target: &str) -> Result<()> {
    let text = std::fs::read_to_string(input).context("cannot read file")?;
    let value: Value = match normalize_ext(&extension(input)) {
        "json" => serde_json::from_str(&text).context("invalid JSON")?,
        "yaml" => serde_yaml::from_str(&text).context("invalid YAML")?,
        "toml" => toml::from_str(&text).context("invalid TOML")?,
        "csv" => read_csv(&text)?,
        other => bail!("unsupported data source: {other}"),
    };

    let out = match target {
        "json" => serde_json::to_string_pretty(&value)? + "\n",
        "yaml" => serde_yaml::to_string(&value)?,
        "toml" => {
            // TOML documents must be a table at the top level.
            let value = match value {
                Value::Object(_) => value,
                other => Value::Object(Map::from_iter([("items".to_string(), other)])),
            };
            toml::to_string_pretty(&value)
                .context("data cannot be represented as TOML (nulls are not allowed)")?
        }
        "csv" => write_csv(&value)?,
        other => bail!("unsupported data target: {other}"),
    };
    output.write_all(out.as_bytes())?;
    Ok(())
}

fn read_csv(text: &str) -> Result<Value> {
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let headers = reader.headers()?.clone();
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        let row: Map<String, Value> = headers
            .iter()
            .zip(record.iter())
            .map(|(h, v)| (h.to_string(), infer_scalar(v)))
            .collect();
        rows.push(Value::Object(row));
    }
    Ok(Value::Array(rows))
}

fn infer_scalar(s: &str) -> Value {
    if s.is_empty() {
        return Value::Null;
    }
    if let Ok(i) = s.parse::<i64>() {
        return Value::from(i);
    }
    if let Ok(f) = s.parse::<f64>()
        && f.is_finite()
    {
        return Value::from(f);
    }
    match s {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(s.to_string()),
    }
}

fn write_csv(value: &Value) -> Result<String> {
    let rows: Vec<&Map<String, Value>> = match value {
        Value::Array(items) => items
            .iter()
            .map(|v| {
                v.as_object()
                    .ok_or_else(|| anyhow!("CSV needs an array of objects"))
            })
            .collect::<Result<_>>()?,
        Value::Object(map) => vec![map],
        _ => bail!("CSV needs an array of objects"),
    };

    // Union of all keys, in first-seen order.
    let mut headers: Vec<&str> = Vec::new();
    for row in &rows {
        for key in row.keys() {
            if !headers.contains(&key.as_str()) {
                headers.push(key);
            }
        }
    }

    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(&headers)?;
    for row in rows {
        let record: Vec<String> = headers
            .iter()
            .map(|h| match row.get(*h) {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
            })
            .collect();
        writer.write_record(&record)?;
    }
    Ok(String::from_utf8(writer.into_inner()?)?)
}

// ---------------------------------------------------------------- audio / video

#[cfg(windows)]
fn ffmpeg_command() -> Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new("ffmpeg");
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(not(windows))]
fn ffmpeg_command() -> Command {
    Command::new("ffmpeg")
}

pub fn ffmpeg_available() -> bool {
    ffmpeg_command()
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ffmpeg_args(target: &str) -> &'static [&'static str] {
    match target {
        "mp3" => &["-vn", "-c:a", "libmp3lame", "-q:a", "2"],
        "wav" => &["-vn"],
        "flac" => &["-vn", "-c:a", "flac"],
        "ogg" => &["-vn", "-c:a", "libvorbis", "-q:a", "5"],
        "m4a" => &["-vn", "-c:a", "aac", "-b:a", "192k"],
        "opus" => &["-vn", "-c:a", "libopus", "-b:a", "128k"],
        "mp4" | "mov" => &[
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "23",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-movflags",
            "+faststart",
        ],
        "mkv" => &[
            "-c:v", "libx264", "-crf", "23", "-c:a", "aac", "-b:a", "160k",
        ],
        "webm" => &[
            "-c:v",
            "libvpx-vp9",
            "-crf",
            "32",
            "-b:v",
            "0",
            "-c:a",
            "libopus",
        ],
        "avi" => &[
            "-c:v",
            "mpeg4",
            "-q:v",
            "5",
            "-c:a",
            "libmp3lame",
            "-q:a",
            "4",
        ],
        "gif" => &[
            "-an",
            "-vf",
            "fps=12,scale='min(480,iw)':-2:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",
            "-loop",
            "0",
        ],
        _ => &[],
    }
}

fn convert_media(input: &Path, output: &Path, target: &str) -> Result<()> {
    let result = ffmpeg_command()
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(ffmpeg_args(target))
        .arg(output)
        .output();

    let out = match result {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("ffmpeg was not found on PATH — install it to convert audio/video")
        }
        Err(e) => return Err(e).context("failed to run ffmpeg"),
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("unknown error");
        bail!("ffmpeg: {msg}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputPlanner;

    /// Skipping is fine locally; CI sets `var` so a missing dependency fails the test.
    fn require_unset(var: &str, reason: &str) {
        assert!(std::env::var_os(var).is_none(), "{var} is set but {reason}");
    }

    fn convert_to(
        input: &Path,
        directory: &Path,
        cat: Category,
        target: &str,
        opts: &Options,
    ) -> Result<PathBuf> {
        let mut planner = OutputPlanner::new([input])?;
        let plan = planner.plan(input, Some(directory), target, opts.overwrite)?;
        convert(input, plan, cat, target, opts)
    }

    #[test]
    fn image_to_every_target() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.png");
        image::RgbaImage::from_fn(300, 200, |x, y| image::Rgba([x as u8, y as u8, 128, 200]))
            .save(&src)
            .unwrap();
        for &t in Category::Image.targets() {
            let out = convert_to(&src, dir.path(), Category::Image, t, &Options::default())
                .unwrap_or_else(|e| panic!("{t}: {e:#}"));
            image::open(&out).unwrap_or_else(|e| panic!("reopen {t}: {e}"));
        }
    }

    #[test]
    fn data_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.json");
        std::fs::write(
            &src,
            r#"[{"name":"a","n":1,"ok":true},{"name":"b","n":2.5,"ok":false}]"#,
        )
        .unwrap();
        for &t in Category::Data.targets() {
            let out = convert_to(
                &src,
                &dir.path().join(t),
                Category::Data,
                t,
                &Options::default(),
            )
            .unwrap_or_else(|e| panic!("{t}: {e:#}"));
            // Convert back to JSON and compare.
            let back = convert_to(
                &out,
                &dir.path().join(format!("back-{t}")),
                Category::Data,
                "json",
                &Options::default(),
            )
            .unwrap();
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&back).unwrap()).unwrap();
            let rows = if t == "toml" { v["items"].clone() } else { v };
            assert_eq!(rows[1]["n"], 2.5, "{t}");
            assert_eq!(rows[0]["ok"], true, "{t}");
        }
    }

    #[test]
    fn failed_jpeg_conversion_preserves_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("wide.png");
        let output = dir.path().join("wide.jpg");
        image::RgbImage::new(65_536, 1).save(&input).unwrap();
        std::fs::write(&output, b"existing image").unwrap();
        let opts = Options {
            overwrite: true,
            ..Options::default()
        };
        assert!(convert_to(&input, dir.path(), Category::Image, "jpg", &opts).is_err());
        assert_eq!(std::fs::read(output).unwrap(), b"existing image");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn jpeg_flush_error_is_returned() {
        struct FailedDisk;
        impl Write for FailedDisk {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("simulated disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = BufWriter::new(FailedDisk);
        let error = encode_jpeg(&DynamicImage::new_rgb8(1, 1), &mut writer, 90).unwrap_err();
        assert!(format!("{error:#}").contains("simulated disk full"));
    }

    #[test]
    fn failed_data_conversion_preserves_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("invalid.json");
        let output = dir.path().join("invalid.yaml");
        std::fs::write(&input, b"{").unwrap();
        std::fs::write(&output, b"previous data").unwrap();
        let opts = Options {
            overwrite: true,
            ..Options::default()
        };
        assert!(convert_to(&input, dir.path(), Category::Data, "yaml", &opts).is_err());
        assert_eq!(std::fs::read(output).unwrap(), b"previous data");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn successful_conversion_replaces_only_the_planned_destination() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("data.json");
        let output = dir.path().join("data.yaml");
        std::fs::write(&input, r#"{"updated":true}"#).unwrap();
        std::fs::write(&output, b"previous data").unwrap();
        let opts = Options {
            overwrite: true,
            ..Options::default()
        };
        convert_to(&input, dir.path(), Category::Data, "yaml", &opts).unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), "updated: true\n");
    }

    #[test]
    fn cross_format_batch_preserves_both_sources() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("batch.json");
        let yaml = dir.path().join("batch.yaml");
        std::fs::write(&json, r#"{"origin":"A"}"#).unwrap();
        std::fs::write(&yaml, "origin: B\n").unwrap();
        let mut planner = OutputPlanner::new([json.as_path(), yaml.as_path()]).unwrap();
        let json_plan = planner.plan(&json, None, "yaml", true).unwrap();
        let yaml_plan = planner.plan(&yaml, None, "json", true).unwrap();
        let opts = Options {
            overwrite: true,
            ..Options::default()
        };
        let a = convert(&json, json_plan, Category::Data, "yaml", &opts).unwrap();
        let b = convert(&yaml, yaml_plan, Category::Data, "json", &opts).unwrap();
        assert_eq!(std::fs::read_to_string(json).unwrap(), r#"{"origin":"A"}"#);
        assert_eq!(std::fs::read_to_string(yaml).unwrap(), "origin: B\n");
        assert_eq!(std::fs::read_to_string(a).unwrap(), "origin: A\n");
        let converted: Value = serde_json::from_str(&std::fs::read_to_string(b).unwrap()).unwrap();
        assert_eq!(converted, serde_json::json!({"origin": "B"}));
    }

    #[test]
    fn failed_ffmpeg_conversion_preserves_existing_output() {
        if !ffmpeg_available() {
            require_unset("CONVERTCRAFT_REQUIRE_FFMPEG", "ffmpeg is unavailable");
            eprintln!("skipping FFmpeg failure test: ffmpeg is unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("broken.mp4");
        let output = dir.path().join("broken.wav");
        std::fs::write(&input, b"not a media file").unwrap();
        std::fs::write(&output, b"previous audio").unwrap();
        let opts = Options {
            overwrite: true,
            ..Options::default()
        };
        assert!(convert_to(&input, dir.path(), Category::Video, "wav", &opts).is_err());
        assert_eq!(std::fs::read(output).unwrap(), b"previous audio");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn media_via_ffmpeg() {
        if !ffmpeg_available() {
            require_unset("CONVERTCRAFT_REQUIRE_FFMPEG", "ffmpeg is unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.mp4");
        let ok = ffmpeg_command()
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=320x240:rate=15",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=1",
                "-shortest",
            ])
            .arg(&src)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        for &t in Category::Video.targets() {
            let out = convert_to(
                &src,
                &dir.path().join("video"),
                Category::Video,
                t,
                &Options::default(),
            )
            .unwrap_or_else(|e| panic!("{t}: {e:#}"));
            assert!(std::fs::metadata(&out).unwrap().len() > 0, "{t}");
        }
        let wav = dir.path().join("video/src.wav");
        for &t in Category::Audio.targets() {
            convert_to(
                &wav,
                &dir.path().join("audio"),
                Category::Audio,
                t,
                &Options::default(),
            )
            .unwrap_or_else(|e| panic!("{t}: {e:#}"));
        }
    }
}
