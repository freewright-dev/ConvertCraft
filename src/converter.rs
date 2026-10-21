//! Conversion backends: images (pure Rust), structured data (pure Rust),
//! audio/video (delegated to an `ffmpeg` binary on PATH).

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Condvar, Mutex, PoisonError};

use anyhow::{Context, Result, anyhow, bail, ensure};
use image::{
    DynamicImage, ImageDecoder, ImageFormat, codecs::jpeg::JpegEncoder, imageops::FilterType,
};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
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
    /// Prefix CSV cells that spreadsheets would evaluate as formulas.
    pub csv_formula_guard: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            jpeg_quality: 90,
            overwrite: false,
            csv_formula_guard: true,
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
        Category::Data => convert_data(input, staged.file_mut(), target, opts)?,
        Category::Audio | Category::Video => convert_media(input, staged.path(), target)?,
    }
    staged.commit()
}

// ---------------------------------------------------------------- images

/// Largest image converted. Normalizing to 8-bit RGB(A) allocates up to 4 bytes per
/// pixel on top of the decoded buffer, which the `image` allocation limit does not cover.
const MAX_IMAGE_PIXELS: u64 = 100_000_000;
/// Estimated memory shared by all concurrent image jobs.
const IMAGE_MEMORY_BUDGET: u64 = 1536 * 1024 * 1024;
/// Decoders accepted after content sniffing; matches the extensions in `detect`.
const INPUT_IMAGE_FORMATS: [ImageFormat; 10] = [
    ImageFormat::Png,
    ImageFormat::Jpeg,
    ImageFormat::WebP,
    ImageFormat::Bmp,
    ImageFormat::Gif,
    ImageFormat::Tiff,
    ImageFormat::Ico,
    ImageFormat::Tga,
    ImageFormat::Qoi,
    ImageFormat::Pnm,
];

static IMAGE_MEMORY: MemoryBudget = MemoryBudget::new(IMAGE_MEMORY_BUDGET);

/// Counting semaphore over bytes, so concurrent workers share one memory budget.
struct MemoryBudget {
    used: Mutex<u64>,
    freed: Condvar,
    limit: u64,
}

impl MemoryBudget {
    const fn new(limit: u64) -> Self {
        Self {
            used: Mutex::new(0),
            freed: Condvar::new(),
            limit,
        }
    }

    /// Blocks until `bytes` fit. A request larger than the whole budget waits until it
    /// can run alone.
    fn reserve(&self, bytes: u64) -> Reservation<'_> {
        let bytes = bytes.min(self.limit);
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        while *used + bytes > self.limit {
            used = self
                .freed
                .wait(used)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *used += bytes;
        Reservation {
            budget: self,
            bytes,
        }
    }
}

struct Reservation<'a> {
    budget: &'a MemoryBudget,
    bytes: u64,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut used = self
            .budget
            .used
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *used -= self.bytes;
        self.budget.freed.notify_all();
    }
}

fn convert_image(input: &Path, output: &mut File, target: &str, opts: &Options) -> Result<()> {
    // Content wins over the extension (a PNG named .jpg still converts), but only
    // advertised formats are decoded.
    let reader = image::ImageReader::open(input)
        .context("cannot open image")?
        .with_guessed_format()?;
    let format = reader.format().context("unrecognized image format")?;
    ensure!(
        INPUT_IMAGE_FORMATS.contains(&format),
        "unsupported image format: {format:?}"
    );
    let decoder = reader.into_decoder().context("cannot decode image")?;
    let (width, height) = decoder.dimensions();
    let pixels = u64::from(width) * u64::from(height);
    ensure!(
        pixels <= MAX_IMAGE_PIXELS,
        "image is too large to convert ({width}×{height}; the limit is {} megapixels)",
        MAX_IMAGE_PIXELS / 1_000_000
    );
    // Decoded buffer plus the 8-bit RGB(A) copy made before encoding.
    let _memory = IMAGE_MEMORY.reserve(decoder.total_bytes().saturating_add(pixels * 4));
    let img = DynamicImage::from_decoder(decoder).context("cannot decode image")?;

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

/// Largest data file read.
const MAX_DATA_INPUT_BYTES: u64 = 64 * 1024 * 1024;
/// Estimated in-memory size of the parsed document. YAML aliases and repeated CSV
/// headers can expand a small file far beyond its own size.
const MAX_DATA_VALUE_BYTES: u64 = 768 * 1024 * 1024;
/// Largest converted data file written.
const MAX_DATA_OUTPUT_BYTES: u64 = 1024 * 1024 * 1024;
/// Excel's column limit; also bounds the rows × headers cell expansion of sparse rows.
const MAX_CSV_COLUMNS: usize = 16_384;
/// Rough per-node cost of a `serde_json::Value`, charged on top of string bytes.
const VALUE_NODE_BYTES: u64 = 64;

fn convert_data(input: &Path, output: &mut File, target: &str, opts: &Options) -> Result<()> {
    let mut text = String::new();
    File::open(input)
        .context("cannot read file")?
        .take(MAX_DATA_INPUT_BYTES + 1)
        .read_to_string(&mut text)
        .context("cannot read file")?;
    ensure!(
        text.len() as u64 <= MAX_DATA_INPUT_BYTES,
        "data file is too large (the limit is {} MiB)",
        MAX_DATA_INPUT_BYTES >> 20
    );
    let budget = ValueBudget::new(MAX_DATA_VALUE_BYTES);
    let value: Value = match normalize_ext(&extension(input)) {
        "json" => {
            let mut de = serde_json::Deserializer::from_str(&text);
            let value = budget.seed().deserialize(&mut de).context("invalid JSON")?;
            de.end().context("invalid JSON")?;
            value
        }
        "yaml" => budget
            .seed()
            .deserialize(serde_yaml::Deserializer::from_str(&text))
            .context("invalid YAML")?,
        "toml" => budget
            .seed()
            .deserialize(toml::Deserializer::new(&text))
            .context("invalid TOML")?,
        "csv" => read_csv(&text, &budget)?,
        other => bail!("unsupported data source: {other}"),
    };

    // Stream into the staged file, refusing to write past the output limit.
    let mut writer = LimitedWriter {
        inner: BufWriter::new(output),
        remaining: MAX_DATA_OUTPUT_BYTES,
    };
    match target {
        "json" => {
            serde_json::to_writer_pretty(&mut writer, &value)?;
            writer.write_all(b"\n")?;
        }
        "yaml" => serde_yaml::to_writer(&mut writer, &value)?,
        "toml" => {
            // TOML documents must be a table at the top level.
            let value = match value {
                Value::Object(_) => value,
                other => Value::Object(Map::from_iter([("items".to_string(), other)])),
            };
            let text = toml::to_string_pretty(&value)
                .context("data cannot be represented as TOML (nulls are not allowed)")?;
            writer.write_all(text.as_bytes())?;
        }
        "csv" => write_csv(&value, &mut writer, opts.csv_formula_guard)?,
        other => bail!("unsupported data target: {other}"),
    }
    writer.flush().context("cannot flush data output")?;
    Ok(())
}

/// Estimated memory charged while a document is built.
struct ValueBudget {
    remaining: Cell<u64>,
}

impl ValueBudget {
    fn new(limit: u64) -> Self {
        Self {
            remaining: Cell::new(limit),
        }
    }

    fn charge(&self, bytes: usize) -> std::result::Result<(), String> {
        let cost = VALUE_NODE_BYTES.saturating_add(bytes as u64);
        match self.remaining.get().checked_sub(cost) {
            Some(rest) => {
                self.remaining.set(rest);
                Ok(())
            }
            None => Err(format!(
                "document expands beyond the {} MiB memory limit",
                MAX_DATA_VALUE_BYTES >> 20
            )),
        }
    }

    fn seed(&self) -> ValueSeed<'_> {
        ValueSeed { budget: self }
    }
}

/// Builds a `serde_json::Value` like its own `Deserialize` impl, but charges every
/// node against a budget (so YAML aliases cannot expand without bound) and rejects
/// duplicate keys instead of keeping the last one.
#[derive(Clone, Copy)]
struct ValueSeed<'a> {
    budget: &'a ValueBudget,
}

impl ValueSeed<'_> {
    fn charge<E: de::Error>(self, bytes: usize) -> std::result::Result<(), E> {
        self.budget.charge(bytes).map_err(E::custom)
    }
}

impl<'de> DeserializeSeed<'de> for ValueSeed<'_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for ValueSeed<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("any JSON-compatible value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        self.charge(0)?;
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        self.charge(0)?;
        Ok(Value::from(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        self.charge(0)?;
        Ok(Value::from(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        self.charge(0)?;
        Ok(serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        self.charge(v.len())?;
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        self.charge(v.len())?;
        Ok(Value::String(v))
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        self.charge(0)?;
        Ok(Value::Null)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        self.charge(0)?;
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        self.deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        self.charge(0)?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(self)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        self.charge(0)?;
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            self.charge(key.len())?;
            if object.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key {key:?}")));
            }
            let value = map.next_value_seed(self)?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

fn read_csv(text: &str, budget: &ValueBudget) -> Result<Value> {
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let headers = reader.headers()?.clone();
    ensure!(
        headers.len() <= MAX_CSV_COLUMNS,
        "CSV has more than {MAX_CSV_COLUMNS} columns"
    );
    let mut seen = HashSet::new();
    for header in &headers {
        ensure!(seen.insert(header), "duplicate CSV header {header:?}");
    }
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        let mut row = Map::new();
        for (header, field) in headers.iter().zip(record.iter()) {
            // Every row stores its own copy of each header name.
            budget
                .charge(header.len() + field.len())
                .map_err(|message| anyhow!(message))?;
            row.insert(header.to_string(), infer_scalar(field));
        }
        rows.push(Value::Object(row));
    }
    Ok(Value::Array(rows))
}

fn infer_scalar(s: &str) -> Value {
    if s.is_empty() {
        return Value::Null;
    }
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    let integer_like = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    // Identifiers such as "00042" keep their leading zeros.
    let leading_zero =
        digits.len() > 1 && digits.starts_with('0') && digits.as_bytes()[1].is_ascii_digit();
    if !leading_zero {
        if let Ok(i) = s.parse::<i64>() {
            return Value::from(i);
        }
        if let Ok(u) = s.parse::<u64>() {
            return Value::from(u);
        }
        // Integers beyond 64 bits stay exact as text rather than rounding through f64.
        if !integer_like
            && let Ok(f) = s.parse::<f64>()
            && f.is_finite()
        {
            return Value::from(f);
        }
    }
    match s {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(s.to_string()),
    }
}

/// Prefix text that a spreadsheet would evaluate as a formula (OWASP CSV injection list).
fn csv_text(s: &str, formula_guard: bool) -> Cow<'_, str> {
    if formula_guard && s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        Cow::Owned(format!("'{s}"))
    } else {
        Cow::Borrowed(s)
    }
}

fn write_csv(value: &Value, output: impl Write, formula_guard: bool) -> Result<()> {
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
    let mut seen = HashSet::new();
    for row in &rows {
        for key in row.keys() {
            if seen.insert(key.as_str()) {
                ensure!(
                    headers.len() < MAX_CSV_COLUMNS,
                    "CSV would have more than {MAX_CSV_COLUMNS} columns"
                );
                headers.push(key);
            }
        }
    }

    let mut writer = csv::Writer::from_writer(output);
    writer.write_record(
        headers
            .iter()
            .map(|h| csv_text(h, formula_guard).into_owned()),
    )?;
    for row in rows {
        let record = headers.iter().map(|h| match row.get(*h) {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => csv_text(s, formula_guard).into_owned(),
            Some(other) => other.to_string(),
        });
        writer.write_record(record)?;
    }
    writer.flush()?;
    Ok(())
}

/// Fails once more than `remaining` bytes would be written.
struct LimitedWriter<W> {
    inner: W,
    remaining: u64,
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.remaining {
            return Err(std::io::Error::other(format!(
                "converted output would exceed the {} MiB limit",
                MAX_DATA_OUTPUT_BYTES >> 20
            )));
        }
        let written = self.inner.write(buf)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
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

    /// A BMP header declaring `width`x`height` pixels, with no pixel data.
    fn bmp_header(width: i32, height: i32) -> Vec<u8> {
        let mut bmp = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&54u32.to_le_bytes()); // file size (header only)
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
        bmp.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
        bmp.extend_from_slice(&width.to_le_bytes());
        bmp.extend_from_slice(&height.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
        bmp.extend_from_slice(&24u16.to_le_bytes()); // bits per pixel
        bmp.extend_from_slice(&[0; 24]); // no compression, sizes, resolution, palette
        bmp
    }

    #[test]
    fn oversized_images_are_rejected_before_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("huge.bmp");
        std::fs::write(&src, bmp_header(20_000, 20_000)).unwrap();
        let error = convert_to(
            &src,
            dir.path(),
            Category::Image,
            "png",
            &Options::default(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("too large"), "{error:#}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn undeclared_image_formats_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("photo.png");
        // OpenEXR magic number followed by junk.
        std::fs::write(&src, [0x76, 0x2f, 0x31, 0x01, 2, 0, 0, 0, 0, 0]).unwrap();
        let error = convert_to(
            &src,
            dir.path(),
            Category::Image,
            "bmp",
            &Options::default(),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("unsupported image format: OpenExr"),
            "{error:#}"
        );
    }

    #[test]
    fn sniffed_supported_format_still_converts() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("actually-png.jpg");
        image::RgbImage::new(4, 4)
            .save_with_format(&src, ImageFormat::Png)
            .unwrap();
        convert_to(
            &src,
            dir.path(),
            Category::Image,
            "bmp",
            &Options::default(),
        )
        .unwrap();
    }

    #[test]
    fn memory_budget_blocks_until_released() {
        use std::sync::mpsc;
        use std::time::Duration;
        static BUDGET: MemoryBudget = MemoryBudget::new(100);
        let first = BUDGET.reserve(60);
        let (tx, rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _second = BUDGET.reserve(60);
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        drop(first);
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        waiter.join().unwrap();
        // Oversized requests are clamped and run alone instead of deadlocking.
        drop(BUDGET.reserve(1_000));
    }

    fn convert_text(name: &str, contents: &str, target: &str, opts: &Options) -> Result<String> {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(name);
        std::fs::write(&src, contents).unwrap();
        let out = convert_to(&src, dir.path(), Category::Data, target, opts)?;
        Ok(std::fs::read_to_string(out).unwrap())
    }

    #[test]
    fn yaml_alias_expansion_is_budgeted() {
        let mut yaml = format!("anchor: &s {}\nrefs: [", "x".repeat(4096));
        yaml.push_str(&vec!["*s"; 1000].join(","));
        yaml.push_str("]\n");
        let budget = ValueBudget::new(1024 * 1024);
        let error = budget
            .seed()
            .deserialize(serde_yaml::Deserializer::from_str(&yaml))
            .unwrap_err();
        assert!(error.to_string().contains("memory limit"), "{error}");
        // The same document fits a budget large enough for its ~4 MB expansion.
        let budget = ValueBudget::new(8 * 1024 * 1024);
        let value = budget
            .seed()
            .deserialize(serde_yaml::Deserializer::from_str(&yaml))
            .unwrap();
        assert_eq!(value["refs"].as_array().unwrap().len(), 1000);
    }

    #[test]
    fn duplicate_keys_and_headers_are_rejected() {
        let opts = Options::default();
        for (name, contents) in [
            ("dup.json", r#"{"a": 1, "a": 2}"#),
            ("dup.yaml", "a: 1\na: 2\n"),
            ("dup.csv", "x,x\nfirst,second\n"),
        ] {
            let error = convert_text(name, contents, "json", &opts).unwrap_err();
            assert!(
                format!("{error:#}").contains("duplicate"),
                "{name}: {error:#}"
            );
        }
    }

    #[test]
    fn csv_scalars_keep_exact_integers_and_identifiers() {
        let csv = "v\n00042\n-007\n0\n-7\n+5\n18446744073709551615\n18446744073709551616\n0.5\n1e3\n1e400\ntrue\n";
        let json = convert_text("in.csv", csv, "json", &Options::default()).unwrap();
        let rows: Value = serde_json::from_str(&json).unwrap();
        let values: Vec<&Value> = rows.as_array().unwrap().iter().map(|r| &r["v"]).collect();
        assert_eq!(
            values,
            [
                &Value::from("00042"),
                &Value::from("-007"),
                &Value::from(0),
                &Value::from(-7),
                &Value::from(5),
                &Value::from(u64::MAX),
                &Value::from("18446744073709551616"),
                &Value::from(0.5),
                &Value::from(1000.0),
                &Value::from("1e400"),
                &Value::from(true),
            ]
        );
    }

    #[test]
    fn csv_formulas_are_neutralized_unless_disabled() {
        let json = r#"[{"=1+1": "=HYPERLINK(\"http://example.invalid\")", "n": "@SUM(1,2)", "ok": "plain", "num": -5}]"#;
        let safe = convert_text("in.json", json, "csv", &Options::default()).unwrap();
        assert_eq!(
            safe,
            "'=1+1,n,num,ok\n\"'=HYPERLINK(\"\"http://example.invalid\"\")\",\"'@SUM(1,2)\",-5,plain\n"
        );
        let raw_opts = Options {
            csv_formula_guard: false,
            ..Options::default()
        };
        let raw = convert_text("in.json", json, "csv", &raw_opts).unwrap();
        assert!(raw.starts_with("=1+1,"), "{raw}");
    }

    #[test]
    fn sparse_csv_columns_are_capped() {
        let rows: Vec<String> = (0..=MAX_CSV_COLUMNS)
            .map(|i| format!(r#"{{"k{i}":0}}"#))
            .collect();
        let json = format!("[{}]", rows.join(","));
        let error = convert_text("sparse.json", &json, "csv", &Options::default()).unwrap_err();
        assert!(format!("{error:#}").contains("columns"), "{error:#}");
    }

    #[test]
    fn data_input_and_output_sizes_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("big.json");
        let file = File::create(&src).unwrap();
        file.set_len(MAX_DATA_INPUT_BYTES + 1).unwrap();
        drop(file);
        let error = convert_to(
            &src,
            dir.path(),
            Category::Data,
            "yaml",
            &Options::default(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("too large"), "{error:#}");

        let mut writer = LimitedWriter {
            inner: Vec::new(),
            remaining: 4,
        };
        writer.write_all(b"1234").unwrap();
        let error = writer.write_all(b"5").unwrap_err();
        assert!(error.to_string().contains("limit"), "{error}");
        assert_eq!(writer.inner, b"1234");
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
