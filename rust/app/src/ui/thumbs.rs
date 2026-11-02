/*! Images in chats, decoded at the size they're shown (not their full size), off the main thread, and
kept up to `chat.image_cache_mb` (the least recently shown go first, out of the GPU too). */

use crate::workspace::Workspace;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub(crate) enum Source {
    Bytes(Arc<[u8]>),
    File(std::path::PathBuf),
}

enum Thumb {
    Loading,
    Failed,
    Ready { image: Arc<RenderImage>, bytes: usize, shown: u64 },
}

#[derive(Default)]
pub(crate) struct Thumbs {
    by_key: HashMap<String, Thumb>,
    bytes: usize,
    /** The images each region (or "" the window) showed when it last drew: on screen, so never let go. */
    shown_by: HashMap<String, HashSet<String>>,
}

impl Thumbs {
    /** `owner` draws again: what it shows is noted afresh. */
    pub fn clear(&mut self, owner: &str) {
        self.shown_by.remove(owner);
    }

    /** Regions gone from the window no longer show anything. */
    pub fn keep(&mut self, live: &HashSet<String>) {
        self.shown_by.retain(|owner, _| owner.is_empty() || live.contains(owner));
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    pub fn kb(&self) -> usize {
        self.bytes / 1024
    }
}

/** `bytes` decoded and scaled down to fit `max` pixels, as BGRA (what GPUI draws). */
fn decode(bytes: &[u8], max: (u32, u32)) -> Option<RenderImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > max.0 || img.height() > max.1 { img.thumbnail(max.0, max.1) } else { img };
    let mut rgba = img.into_rgba8();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Some(RenderImage::new(smallvec::SmallVec::from_elem(image::Frame::new(rgba), 1)))
}

impl Workspace {
    /** The image under `key` at `size` points, once decoded (until then None, and the view redraws when it is). */
    pub(crate) fn thumb(&mut self, key: &str, source: impl FnOnce() -> Source, size: (f32, f32), window: &Window, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        let frame = self.render_generation;
        let owner = super::region::owner();
        self.thumbs.shown_by.entry(owner.clone()).or_default().insert(key.to_string());
        match self.thumbs.by_key.get_mut(key) {
            Some(Thumb::Ready { image, shown, .. }) => {
                *shown = frame;
                return Some(image.clone());
            }
            Some(_) => return None,
            None => {}
        }
        self.thumbs.by_key.insert(key.to_string(), Thumb::Loading);
        let scale = window.scale_factor();
        let max = ((size.0 * scale).ceil().max(1.) as u32, (size.1 * scale).ceil().max(1.) as u32);
        let source = source();
        let key = key.to_string();
        cx.spawn(async move |this, cx| {
            let decoded = cx
                .background_spawn(async move {
                    let bytes: Arc<[u8]> = match source {
                        Source::Bytes(b) => b,
                        Source::File(p) => std::fs::read(p).ok()?.into(),
                    };
                    decode(&bytes, max)
                })
                .await;
            let _ = this.update(cx, |ws, cx| {
                let thumb = match decoded {
                    Some(img) => {
                        let bytes = img.as_bytes(0).map_or(0, <[u8]>::len);
                        ws.thumbs.bytes += bytes;
                        Thumb::Ready { image: Arc::new(img), bytes, shown: ws.render_generation }
                    }
                    None => Thumb::Failed,
                };
                ws.thumbs.by_key.insert(key, thumb);
                ws.notify_owner(&owner, cx);
            });
        })
        .detach();
        None
    }

    /** Lets go of the least recently shown images past `chat.image_cache_mb`, never one on screen. */
    pub(crate) fn trim_thumbs(&mut self, window: &mut Window, cx: &mut App) {
        let budget = (self.settings.num("chat.image_cache_mb", 48.) * 1024. * 1024.) as usize;
        if self.thumbs.bytes <= budget {
            return;
        }
        let on_screen: HashSet<&String> = self.thumbs.shown_by.values().flatten().collect();
        let mut ready: Vec<(u64, String)> = self.thumbs.by_key.iter().filter(|(k, _)| !on_screen.contains(k)).filter_map(|(k, t)| match t {
            Thumb::Ready { shown, .. } => Some((*shown, k.clone())),
            _ => None,
        }).collect();
        ready.sort();
        for (_, key) in ready {
            if self.thumbs.bytes <= budget {
                break;
            }
            if let Some(Thumb::Ready { image, bytes, .. }) = self.thumbs.by_key.remove(&key) {
                self.thumbs.bytes -= bytes;
                cx.drop_image(image, Some(window));
            }
        }
    }
}
