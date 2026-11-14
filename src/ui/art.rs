//! Cover art textures. Decoding happens on a worker thread; the UI only uploads pixels.
use crate::library::{ArtImage, Library};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// How large a cover is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtSize {
    /// For a row of a list, and for the cells of the album grid on a desk.
    Thumb,
    /// For a cell of the album grid on a phone, where two cells fill the width of a
    /// screen with three pixels to the point.
    Grid,
    /// For the one cover that is looked at: the info panel, an album's page, the player.
    Large,
}

impl ArtSize {
    const ALL: [ArtSize; 3] = [ArtSize::Thumb, ArtSize::Grid, ArtSize::Large];

    /// Edge length in pixels.
    fn pixels(self) -> u32 {
        match self {
            ArtSize::Thumb => 256,
            ArtSize::Grid => 400,
            ArtSize::Large => 640,
        }
    }

    /// How many textures of this size are kept. Each is enough for what a screen shows
    /// at once and what was scrolled past a moment ago: a screen that needs more than
    /// is kept would load its covers again in every frame.
    fn kept(self) -> usize {
        match self {
            ArtSize::Thumb => 320,
            ArtSize::Grid => 48,
            ArtSize::Large => 4,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    id: String,
    size: ArtSize,
}

struct Entry {
    texture: egui::TextureHandle,
    last_used: u64,
}

/// Which textures go so that no more than `cap` are kept: those that were not on screen
/// for longest. `used` says for each when it was last asked for, `frame` is the frame
/// that begins.
///
/// One that is on screen (asked for in the frame before, or in this one) never goes,
/// even above `cap`: it would be asked for again at once, loaded, and thrown out again,
/// frame after frame.
fn to_evict<K>(mut used: Vec<(u64, K)>, frame: u64, cap: usize) -> Vec<K> {
    let over = used.len().saturating_sub(cap);
    if over == 0 {
        return Vec::new();
    }
    used.retain(|(last_used, _)| last_used + 1 < frame);
    used.sort_by_key(|(last_used, _)| *last_used);
    used.into_iter().take(over).map(|(_, key)| key).collect()
}

pub struct ArtCache {
    textures: HashMap<Key, Entry>,
    pending: HashSet<Key>,
    /// Ids that could not be loaded; retried when the library changes.
    failed: HashSet<Key>,
    requests: Sender<Key>,
    results: Receiver<(Key, Option<ArtImage>)>,
    frame: u64,
    /// How many images were made into textures so far (for verification dumps: a
    /// screen at rest must not keep loading).
    uploads: u64,
}

impl ArtCache {
    pub fn new(library: Arc<Library>, ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = channel::<Key>();
        let (res_tx, res_rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("oneum-art".into())
            .spawn(move || {
                while let Ok(key) = req_rx.recv() {
                    let image = library.load_art_rgba(&key.id, key.size.pixels());
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
            uploads: 0,
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
                    let name = format!("art-{}-{:?}", key.id, key.size);
                    let texture = ctx.load_texture(name, pixels, egui::TextureOptions::LINEAR);
                    self.uploads += 1;
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
        for size in ArtSize::ALL {
            let used: Vec<(u64, Key)> = self
                .textures
                .iter()
                .filter(|(k, _)| k.size == size)
                .map(|(k, e)| (e.last_used, k.clone()))
                .collect();
            for key in to_evict(used, self.frame, size.kept()) {
                self.textures.remove(&key);
            }
        }
    }

    /// Textures held, images being loaded, and images loaded so far.
    pub fn counts(&self) -> (usize, usize, u64) {
        (self.textures.len(), self.pending.len(), self.uploads)
    }

    /// Forget failures so missing images are retried (the library may have regenerated them).
    pub fn library_changed(&mut self) {
        self.failed.clear();
    }

    /// Texture for `art_id`, requesting it in the background when not loaded yet.
    pub fn get(&mut self, art_id: Option<&str>, large: bool) -> Option<egui::TextureId> {
        let size = if large {
            ArtSize::Large
        } else {
            ArtSize::Thumb
        };
        self.get_sized(art_id, size)
    }

    /// Texture for `art_id` in `size`, requesting it in the background when not loaded
    /// yet. Until it is there, the cover in another size is shown if that is loaded.
    pub fn get_sized(&mut self, art_id: Option<&str>, size: ArtSize) -> Option<egui::TextureId> {
        let key = Key {
            id: art_id?.to_string(),
            size,
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
        let frame = self.frame;
        ArtSize::ALL
            .into_iter()
            .filter(|other| *other != size)
            .find_map(|other| {
                let stand_in = Key {
                    id: key.id.clone(),
                    size: other,
                };
                let entry = self.textures.get_mut(&stand_in)?;
                // It is on screen too: it must not be thrown out from under it.
                entry.last_used = frame;
                Some(entry.texture.id())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_goes_while_there_is_room() {
        let used = vec![(1, "a"), (2, "b"), (3, "c")];
        assert!(to_evict(used, 10, 3).is_empty());
    }

    #[test]
    fn the_ones_not_seen_for_longest_go_first_and_only_as_many_as_are_over() {
        let used = vec![(7, "c"), (2, "a"), (9, "e"), (5, "b"), (8, "d")];
        assert_eq!(to_evict(used, 20, 3), vec!["a", "b"]);
    }

    /// What made the album grid of a phone load its covers again in every frame: more
    /// covers on screen than are kept. They all stay.
    #[test]
    fn what_is_on_screen_stays_even_above_the_limit() {
        // Asked for in the frame before (9) and in this one (10).
        let used: Vec<(u64, u32)> = (0..12).map(|i| (9 + u64::from(i % 2), i)).collect();
        assert!(to_evict(used, 10, 4).is_empty());
    }

    #[test]
    fn above_the_limit_only_what_is_off_screen_goes() {
        // Six on screen, three scrolled away: the limit of four cannot be met, and the
        // three that are away are all that go.
        let mut used: Vec<(u64, &str)> = vec![(3, "old1"), (5, "old2"), (4, "old3")];
        used.extend([
            (9, "s1"),
            (10, "s2"),
            (10, "s3"),
            (9, "s4"),
            (10, "s5"),
            (10, "s6"),
        ]);
        assert_eq!(to_evict(used, 10, 4), vec!["old1", "old3", "old2"]);
    }
}
