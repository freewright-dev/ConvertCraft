//! Conversion backends: images (pure Rust), structured data (pure Rust),
//! audio/video (delegated to an `ffmpeg` binary on PATH).

use std::collections::HashSet;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage, ImageFormat};
use serde_json::{Map, Value};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Category {
    Image,
    Data,
    Audio,
    Video,
}

impl Category {
    pub const ALL: [Category; 4] = [Category::Image, Category::Data, Category::Audio, Category::Video];

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
            Category::Image => &["png", "jpg", "webp", "bmp", "gif", "tiff", "ico", "tga", "qoi"],
            Category::Data => &["json", "yaml", "toml", "csv"],
            Category::Audio => &["mp3", "wav", "flac", "ogg", "m4a", "opus"],
            Category::Video => &["mp4", "mkv", "webm", "avi", "mov", "gif", "mp3", "wav", "m4a"],
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
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tif" | "tiff" | "ico" | "tga" | "qoi"
        | "pnm" | "pbm" | "pgm" | "ppm" => Category::Image,
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
        Self { jpeg_quality: 90, overwrite: false }
    }
}

/// Compute the output path, avoiding the input file itself, existing files
/// (unless overwriting), and paths already reserved by other jobs in this batch.
pub fn plan_output(
    input: &Path,
    out_dir: Option<&Path>,
    target: &str,
    overwrite: bool,
    reserved: &mut HashSet<PathBuf>,
) -> PathBuf {
    let dir = out_dir
        .map(Path::to_path_buf)
        .or_else(|| input.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");

    let mut n = 0;
    loop {
        let name = if n == 0 {
            format!("{stem}.{target}")
        } else {
            format!("{stem} ({n}).{target}")
        };
        let candidate = dir.join(name);
        let taken = candidate == input
            || reserved.contains(&candidate)
            || (!overwrite && candidate.exists());
        if !taken {
            reserved.insert(candidate.clone());
            return candidate;
        }
        n += 1;
    }
}

pub fn convert(input: &Path, output: &Path, cat: Category, target: &str, opts: &Options) -> Result<()> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    match cat {
        Category::Image => convert_image(input, output, target, opts),
        Category::Data => convert_data(input, output, target),
        Category::Audio | Category::Video => convert_media(input, output, target),
    }
}

// ---------------------------------------------------------------- images

fn convert_image(input: &Path, output: &Path, target: &str, opts: &Options) -> Result<()> {
    let img = image::ImageReader::open(input)
        .context("cannot open image")?
        .with_guessed_format()?
        .decode()
        .context("cannot decode image")?;

    match target {
        "jpg" => {
            let file = BufWriter::new(File::create(output)?);
            let mut enc = JpegEncoder::new_with_quality(file, opts.jpeg_quality);
            enc.encode_image(&DynamicImage::ImageRgb8(img.to_rgb8()))?;
        }
        "png" => img.save_with_format(output, ImageFormat::Png)?,
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
            DynamicImage::ImageRgba8(img.to_rgba8()).save_with_format(output, format)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- data

fn convert_data(input: &Path, output: &Path, target: &str) -> Result<()> {
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
            toml::to_string_pretty(&value).context("data cannot be represented as TOML (nulls are not allowed)")?
        }
        "csv" => write_csv(&value)?,
        other => bail!("unsupported data target: {other}"),
    };
    std::fs::write(output, out)?;
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
    if let Ok(f) = s.parse::<f64>() {
        if f.is_finite() {
            return Value::from(f);
        }
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
            .map(|v| v.as_object().ok_or_else(|| anyhow!("CSV needs an array of objects")))
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

fn ffmpeg_command() -> Command {
    let mut cmd = Command::new("ffmpeg");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
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
            "-c:v", "libx264", "-preset", "medium", "-crf", "23", "-pix_fmt", "yuv420p",
            "-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart",
        ],
        "mkv" => &["-c:v", "libx264", "-crf", "23", "-c:a", "aac", "-b:a", "160k"],
        "webm" => &["-c:v", "libvpx-vp9", "-crf", "32", "-b:v", "0", "-c:a", "libopus"],
        "avi" => &["-c:v", "mpeg4", "-q:v", "5", "-c:a", "libmp3lame", "-q:a", "4"],
        "gif" => &[
            "-an", "-vf",
            "fps=12,scale='min(480,iw)':-2:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",
            "-loop", "0",
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
        let _ = std::fs::remove_file(output);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        bail!("ffmpeg: {msg}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("converter-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn image_to_every_target() {
        let src = tmp("src.png");
        image::RgbaImage::from_fn(300, 200, |x, y| image::Rgba([x as u8, y as u8, 128, 200]))
            .save(&src)
            .unwrap();
        for &t in Category::Image.targets() {
            let out = tmp(&format!("img-out.{t}"));
            convert(&src, &out, Category::Image, t, &Options::default())
                .unwrap_or_else(|e| panic!("{t}: {e:#}"));
            image::open(&out).unwrap_or_else(|e| panic!("reopen {t}: {e}"));
        }
    }

    #[test]
    fn data_round_trips() {
        let src = tmp("src.json");
        std::fs::write(&src, r#"[{"name":"a","n":1,"ok":true},{"name":"b","n":2.5,"ok":false}]"#).unwrap();
        for &t in Category::Data.targets() {
            let out = tmp(&format!("data-out.{t}"));
            convert(&src, &out, Category::Data, t, &Options::default())
                .unwrap_or_else(|e| panic!("{t}: {e:#}"));
            // Convert back to JSON and compare.
            let back = tmp(&format!("data-back-{t}.json"));
            convert(&out, &back, Category::Data, "json", &Options::default()).unwrap();
            let v: Value = serde_json::from_str(&std::fs::read_to_string(&back).unwrap()).unwrap();
            let rows = if t == "toml" { v["items"].clone() } else { v };
            assert_eq!(rows[1]["n"], 2.5, "{t}");
            assert_eq!(rows[0]["ok"], true, "{t}");
        }
    }

    #[test]
    fn output_paths_never_collide() {
        let input = tmp("same.png");
        std::fs::write(&input, b"x").unwrap();
        let mut reserved = HashSet::new();
        let a = plan_output(&input, None, "png", true, &mut reserved);
        let b = plan_output(&input, None, "jpg", true, &mut reserved);
        let c = plan_output(&input, None, "jpg", true, &mut reserved);
        assert_ne!(a, input);
        assert_ne!(b, c);
    }

    #[test]
    fn media_via_ffmpeg() {
        if !ffmpeg_available() {
            return;
        }
        let src = tmp("src.mp4");
        let ok = ffmpeg_command()
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=duration=1:size=320x240:rate=15",
                   "-f", "lavfi", "-i", "sine=duration=1", "-shortest"])
            .arg(&src)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        for &t in Category::Video.targets() {
            let out = tmp(&format!("vid-out.{t}"));
            convert(&src, &out, Category::Video, t, &Options::default())
                .unwrap_or_else(|e| panic!("{t}: {e:#}"));
            assert!(std::fs::metadata(&out).unwrap().len() > 0, "{t}");
        }
        let wav = tmp("vid-out.wav");
        for &t in Category::Audio.targets() {
            let out = tmp(&format!("aud-out.{t}"));
            convert(&wav, &out, Category::Audio, t, &Options::default())
                .unwrap_or_else(|e| panic!("{t}: {e:#}"));
        }
    }
}
