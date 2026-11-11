//! Album artwork cache.
//!
//! Covers are stored once per distinct image as `<cache_dir>/art/<2 hex>/<16 hex>.jpg`,
//! downscaled to at most 600 px. The id is the FNV-1a hash of the embedded image bytes
//! (or of path + size + mtime for a folder image), so all tracks of an album share one
//! file. The cache is disposable: a missing file is regenerated from a track on demand.
use super::ident::{Fnv1a, fnv1a64};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits};
use std::collections::HashSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Larger inputs are ignored (the track simply has no artwork).
const MAX_INPUT_BYTES: u64 = 20 * 1024 * 1024;
/// Images with more pixels are ignored.
const MAX_PIXELS: u64 = 40_000_000;
/// Upper bound for decoder allocations (40 MP RGBA is 160 MB).
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;
/// Longest edge of cached covers.
const CACHE_EDGE_PX: u32 = 600;
const JPEG_QUALITY: u8 = 85;
/// The cache is trimmed when it grows beyond this size …
const CACHE_LIMIT_BYTES: u64 = 512 * 1024 * 1024;
/// … down to this size, oldest files first.
const CACHE_TRIM_TARGET_BYTES: u64 = CACHE_LIMIT_BYTES / 10 * 8;
/// Unreferenced files younger than this are kept by the cleanup: they may belong to
/// tracks a running scan has not committed yet.
const CLEANUP_GRACE: Duration = Duration::from_secs(600);

const FOLDER_IMAGE_STEMS: [&str; 4] = ["cover", "folder", "front", "album"];
const FOLDER_IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Decoded cover, ready to be uploaded as a texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA, row-major.
    pub rgba: Vec<u8>,
}

pub(crate) struct ArtCache {
    dir: PathBuf,
    /// Ids currently being written by some thread.
    busy: Mutex<HashSet<String>>,
}

impl ArtCache {
    pub(crate) fn new(cache_dir: &Path) -> Self {
        ArtCache {
            dir: cache_dir.join("art"),
            busy: Mutex::new(HashSet::new()),
        }
    }

    /// Location of the cached JPEG for `art_id`. Ids that are not plain hex/alphanumeric
    /// map to a path inside the cache that never exists.
    pub(crate) fn path_for(&self, art_id: &str) -> PathBuf {
        let safe: String = art_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(64)
            .collect();
        let shard = safe.get(..2).unwrap_or("__");
        if safe.len() < 2 || safe.len() != art_id.len() {
            return self.dir.join("__").join("invalid.jpg");
        }
        self.dir.join(shard).join(format!("{safe}.jpg"))
    }

    /// Caches an embedded picture and returns its id, or `None` if the bytes are not a
    /// usable image.
    pub(crate) fn store_embedded(&self, bytes: &[u8]) -> Option<String> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_INPUT_BYTES {
            return None;
        }
        let id = format!("{:016x}", fnv1a64(bytes));
        self.ensure(&id, || Some(bytes.to_vec())).then_some(id)
    }

    /// Id a folder image would get, without reading it.
    pub(crate) fn folder_image_id(image_path: &Path) -> Option<String> {
        let meta = fs::metadata(image_path).ok()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_INPUT_BYTES {
            return None;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        let mut hasher = Fnv1a::new();
        hasher.write(b"folder\0");
        hasher.write(image_path.as_os_str().as_encoded_bytes());
        hasher.write(&meta.len().to_le_bytes());
        hasher.write(&mtime.to_le_bytes());
        Some(format!("{:016x}", hasher.finish()))
    }

    /// Caches a cover image file found next to the tracks and returns its id.
    pub(crate) fn store_folder_image(&self, image_path: &Path) -> Option<String> {
        let id = Self::folder_image_id(image_path)?;
        self.ensure(&id, || fs::read(image_path).ok()).then_some(id)
    }

    /// Makes sure the cache file for `id` exists, producing it from `source` if needed.
    /// Returns false if the image cannot be decoded or written.
    fn ensure(&self, id: &str, source: impl FnOnce() -> Option<Vec<u8>>) -> bool {
        let path = self.path_for(id);
        if path.is_file() {
            return true;
        }
        {
            let mut busy = lock(&self.busy);
            if !busy.insert(id.to_string()) {
                // Another thread is writing this very image; it will be there shortly.
                return true;
            }
        }
        let written = source()
            .and_then(|bytes| make_cover_jpeg(&bytes))
            .is_some_and(|jpeg| write_cache_file(&path, &jpeg).is_ok());
        lock(&self.busy).remove(id);
        written
    }

    /// Decodes the cached cover and scales it down so its longest edge is at most
    /// `max_px`. Returns `None` if the cache file is missing or unreadable.
    pub(crate) fn load_rgba(&self, art_id: &str, max_px: u32) -> Option<ArtImage> {
        let bytes = fs::read(self.path_for(art_id)).ok()?;
        let image = image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg).ok()?;
        let max_px = max_px.max(1);
        let image = if image.width().max(image.height()) > max_px {
            image.resize(max_px, max_px, FilterType::Triangle)
        } else {
            image
        };
        let rgba = image.into_rgba8();
        Some(ArtImage {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
        })
    }

    pub(crate) fn exists(&self, art_id: &str) -> bool {
        self.path_for(art_id).is_file()
    }

    /// Deletes cache files no track refers to and trims the cache to its size limit.
    /// Returns early when `stop` is set.
    pub(crate) fn cleanup(&self, referenced: &HashSet<String>, stop: &AtomicBool) {
        struct Entry {
            path: PathBuf,
            size: u64,
            modified: SystemTime,
        }
        let now = SystemTime::now();
        let mut kept: Vec<Entry> = Vec::new();
        let mut removed = 0u64;
        let Ok(shards) = fs::read_dir(&self.dir) else {
            return;
        };
        for shard in shards.flatten() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(files) = fs::read_dir(shard.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                let Ok(meta) = file.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let modified = meta.modified().unwrap_or(now);
                let is_cover = path.extension().is_some_and(|ext| ext == "jpg");
                let id = path.file_stem().and_then(|stem| stem.to_str());
                let in_use = is_cover && id.is_some_and(|id| referenced.contains(id));
                let old = now
                    .duration_since(modified)
                    .is_ok_and(|age| age > CLEANUP_GRACE);
                if !in_use && old {
                    if fs::remove_file(&path).is_ok() {
                        removed += 1;
                    }
                    continue;
                }
                kept.push(Entry {
                    path,
                    size: meta.len(),
                    modified,
                });
            }
        }

        let mut total: u64 = kept.iter().map(|e| e.size).sum();
        if total > CACHE_LIMIT_BYTES {
            kept.sort_by_key(|e| e.modified);
            for entry in &kept {
                if total <= CACHE_TRIM_TARGET_BYTES {
                    break;
                }
                if fs::remove_file(&entry.path).is_ok() {
                    total = total.saturating_sub(entry.size);
                    removed += 1;
                }
            }
        }
        if removed > 0 {
            tracing::info!(removed, remaining_bytes = total, "artwork cache cleaned");
        }
    }
}

/// Finds `cover|folder|front|album.(jpg|jpeg|png)` in `dir`, ignoring case.
pub(crate) fn find_folder_image(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let stem = path.file_stem().and_then(|s| s.to_str());
        let ext = path.extension().and_then(|s| s.to_str());
        let (Some(stem), Some(ext)) = (stem, ext) else {
            continue;
        };
        if !FOLDER_IMAGE_EXTENSIONS
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
        {
            continue;
        }
        let Some(priority) = FOLDER_IMAGE_STEMS
            .iter()
            .position(|known| stem.eq_ignore_ascii_case(known))
        else {
            continue;
        };
        if best.as_ref().is_none_or(|(p, _)| priority < *p) && path.is_file() {
            best = Some((priority, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Decodes an image with bounded resources and re-encodes it as a cache JPEG.
fn make_cover_jpeg(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return None;
    }
    let (width, height) = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let image = if width.max(height) > CACHE_EDGE_PX {
        image.resize(CACHE_EDGE_PX, CACHE_EDGE_PX, FilterType::CatmullRom)
    } else {
        image
    };
    let rgb = image.into_rgb8();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode_image(&rgb)
        .ok()?;
    Some(jpeg)
}

fn write_cache_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no parent"))?;
    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(
        "{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb, Rgba};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(width, height, |x, y| {
            Rgba([
                (x % 256) as u8,
                (y % 256) as u8,
                ((x + y) % 256) as u8,
                255u8,
            ])
        });
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
        });
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(&img)
            .unwrap();
        out
    }

    /// A PNG with a header claiming the given size but no pixel data.
    fn png_header_only(width: u32, height: u32) -> Vec<u8> {
        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for &byte in bytes {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xedb8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            let mut chunk = kind.to_vec();
            chunk.extend_from_slice(data);
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&chunk);
            out.extend_from_slice(&crc32(&chunk).to_be_bytes());
        }
        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, no interlace
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        push_chunk(&mut out, b"IHDR", &header);
        push_chunk(&mut out, b"IDAT", &[]);
        push_chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn header_only_png_reports_dimensions() {
        // Guards the helper: the size check below must be what rejects the image.
        let dims = ImageReader::new(Cursor::new(png_header_only(8000, 6000)))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert_eq!(dims, (8000, 6000));
    }

    #[test]
    fn embedded_image_is_downscaled_and_deduplicated() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtCache::new(dir.path());
        let source = png(1500, 1000);
        let id = cache.store_embedded(&source).unwrap();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        let path = cache.path_for(&id);
        assert_eq!(
            path,
            dir.path()
                .join("art")
                .join(&id[..2])
                .join(format!("{id}.jpg"))
        );
        let full = cache.load_rgba(&id, 4096).unwrap();
        assert_eq!((full.width, full.height), (600, 400));
        assert_eq!(full.rgba.len(), 600 * 400 * 4);

        // Same bytes: same id, the file is not rewritten.
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(cache.store_embedded(&source).unwrap(), id);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);

        let thumb = cache.load_rgba(&id, 64).unwrap();
        assert_eq!((thumb.width, thumb.height), (64, 43));
        assert_eq!(thumb.rgba.len(), 64 * 43 * 4);
    }

    #[test]
    fn small_images_are_not_upscaled() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtCache::new(dir.path());
        let id = cache.store_embedded(&jpeg(120, 80)).unwrap();
        let image = cache.load_rgba(&id, 600).unwrap();
        assert_eq!((image.width, image.height), (120, 80));
    }

    #[test]
    fn undecodable_or_oversized_input_yields_no_art() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtCache::new(dir.path());
        assert_eq!(cache.store_embedded(b""), None);
        assert_eq!(cache.store_embedded(b"definitely not an image"), None);
        // A truncated PNG has a valid header but fails to decode.
        let mut truncated = png(300, 300);
        truncated.truncate(truncated.len() / 3);
        assert_eq!(cache.store_embedded(&truncated), None);
        // Too many pixels: rejected from the header alone, before any decoding.
        assert_eq!(cache.store_embedded(&png_header_only(8000, 6000)), None);
        // Too many bytes.
        let big = vec![0u8; MAX_INPUT_BYTES as usize + 1];
        assert_eq!(cache.store_embedded(&big), None);
        // Nothing was written.
        assert!(
            !dir.path().join("art").exists()
                || fs::read_dir(dir.path().join("art"))
                    .unwrap()
                    .next()
                    .is_none()
        );
    }

    #[test]
    fn missing_or_invalid_ids() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtCache::new(dir.path());
        assert!(cache.load_rgba("0123456789abcdef", 100).is_none());
        assert!(!cache.exists("0123456789abcdef"));
        // Ids cannot escape the cache directory.
        for bad in ["../../etc/passwd", "", "a", "ab/../cd", "/abs"] {
            let path = cache.path_for(bad);
            assert!(path.starts_with(dir.path().join("art")), "{bad}: {path:?}");
            assert!(cache.load_rgba(bad, 10).is_none());
        }
    }

    #[test]
    fn folder_image_lookup_and_id() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("album");
        fs::create_dir(&album).unwrap();
        assert_eq!(find_folder_image(&album), None);
        fs::write(album.join("notes.txt"), b"x").unwrap();
        fs::write(album.join("back.jpg"), jpeg(10, 10)).unwrap();
        assert_eq!(find_folder_image(&album), None);
        fs::write(album.join("Folder.JPG"), jpeg(700, 700)).unwrap();
        let found = find_folder_image(&album).unwrap();
        assert!(
            found
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .eq_ignore_ascii_case("folder.jpg")
        );

        let cache = ArtCache::new(&dir.path().join("cache"));
        let id = cache.store_folder_image(&found).unwrap();
        assert_eq!(ArtCache::folder_image_id(&found).as_deref(), Some(&*id));
        let image = cache.load_rgba(&id, 600).unwrap();
        assert_eq!((image.width, image.height), (600, 600));
        // "cover" wins over "folder".
        fs::write(album.join("cover.png"), png(20, 20)).unwrap();
        let found = find_folder_image(&album).unwrap();
        assert_eq!(found.file_name().unwrap(), "cover.png");
    }

    #[test]
    fn cleanup_removes_only_old_unreferenced_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ArtCache::new(dir.path());
        let keep = cache.store_embedded(&jpeg(50, 50)).unwrap();
        let fresh = cache.store_embedded(&jpeg(60, 60)).unwrap();
        let stale = cache.store_embedded(&jpeg(70, 70)).unwrap();
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        for id in [&keep, &stale] {
            let file = fs::File::options()
                .write(true)
                .open(cache.path_for(id))
                .unwrap();
            file.set_modified(hour_ago).unwrap();
        }
        let referenced: HashSet<String> = [keep.clone()].into_iter().collect();
        cache.cleanup(&referenced, &AtomicBool::new(false));
        assert!(cache.exists(&keep), "referenced file must stay");
        assert!(cache.exists(&fresh), "recent file must stay");
        assert!(!cache.exists(&stale), "old unreferenced file must go");
    }
}
