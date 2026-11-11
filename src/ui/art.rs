//! Cover art textures. Decoding happens on a worker thread; the UI only uploads pixels.
use crate::library::{ArtImage, Library};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Edge length (px) of thumbnails used in lists and grids.
const THUMB_PX: u32 = 256;
/// Edge length (px) of the large image in the info panel.
const LARGE_PX: u32 = 640;
const MAX_THUMBS: usize = 320;
const MAX_LARGE: usize = 4;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    id: String,
    large: bool,
}

struct Entry {
    texture: egui::TextureHandle,
    last_used: u64,
}

pub struct ArtCache {
    textures: HashMap<Key, Entry>,
    pending: HashSet<Key>,
    /// Ids that could not be loaded; retried when the library changes.
    failed: HashSet<Key>,
    requests: Sender<Key>,
    results: Receiver<(Key, Option<ArtImage>)>,
    frame: u64,
}

impl ArtCache {
    pub fn new(library: Arc<Library>, ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = channel::<Key>();
        let (res_tx, res_rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("oneum-art".into())
            .spawn(move || {
                while let Ok(key) = req_rx.recv() {
                    let image =
                        library.load_art_rgba(&key.id, if key.large { LARGE_PX } else { THUMB_PX });
                    if res_tx.send((key, image)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            tracing::error!("cannot start the artwork thread: {e}");
        }
        Self {
            textures: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            requests: req_tx,
            results: res_rx,
            frame: 0,
        }
    }

    /// Call once per frame: uploads finished images and evicts old textures.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        while let Ok((key, image)) = self.results.try_recv() {
            self.pending.remove(&key);
            let valid = image.filter(|i| {
                i.width > 0 && i.height > 0 && i.rgba.len() == (i.width * i.height * 4) as usize
            });
            match valid {
                Some(image) => {
                    let pixels = egui::ColorImage::from_rgba_unmultiplied(
                        [image.width as usize, image.height as usize],
                        &image.rgba,
                    );
                    let name = format!("art-{}-{}", key.id, key.large);
                    let texture = ctx.load_texture(name, pixels, egui::TextureOptions::LINEAR);
                    self.textures.insert(
                        key,
                        Entry {
                            texture,
                            last_used: self.frame,
                        },
                    );
                }
                None => {
                    self.failed.insert(key);
                }
            }
        }
        for (large, cap) in [(false, MAX_THUMBS), (true, MAX_LARGE)] {
            let count = self.textures.keys().filter(|k| k.large == large).count();
            if count > cap {
                let mut used: Vec<(u64, Key)> = self
                    .textures
                    .iter()
                    .filter(|(k, _)| k.large == large)
                    .map(|(k, e)| (e.last_used, k.clone()))
                    .collect();
                used.sort_by_key(|(t, _)| *t);
                for (_, key) in used.into_iter().take(count - cap) {
                    self.textures.remove(&key);
                }
            }
        }
    }

    /// Forget failures so missing images are retried (the library may have regenerated them).
    pub fn library_changed(&mut self) {
        self.failed.clear();
    }

    /// Texture for `art_id`, requesting it in the background when not loaded yet.
    pub fn get(&mut self, art_id: Option<&str>, large: bool) -> Option<egui::TextureId> {
        let key = Key {
            id: art_id?.to_string(),
            large,
        };
        if let Some(entry) = self.textures.get_mut(&key) {
            entry.last_used = self.frame;
            return Some(entry.texture.id());
        }
        if !self.pending.contains(&key)
            && !self.failed.contains(&key)
            && self.requests.send(key.clone()).is_ok()
        {
            self.pending.insert(key.clone());
        }
        // While the large image loads, fall back to the thumbnail if we have it.
        if large {
            let thumb = Key {
                id: key.id,
                large: false,
            };
            return self.textures.get(&thumb).map(|e| e.texture.id());
        }
        None
    }
}
