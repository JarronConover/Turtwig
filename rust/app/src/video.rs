/*! Videos as backgrounds and `image` nodes: AVFoundation plays the file (muted, looping) and each frame
is painted as a GPU surface. One player per source, shared by every node showing it. */

use gpui_kit::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_av_foundation::{AVPlayer, AVPlayerActionAtItemEnd, AVPlayerItem, AVPlayerItemVideoOutput};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{kCMTimeZero, CMTime};
use objc2_core_video::{kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "mov", "m4v", "webm", "mkv", "avi"];

/** Whether a source names a video (by its extension, before any query string). */
pub fn is_video(src: &str) -> bool {
    let path = src.split(['?', '#']).next().unwrap_or(src);
    path.rsplit('.').next().is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

struct Player {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    frame: RefCell<Option<CFRetained<CVPixelBuffer>>>,
}

impl Player {
    fn open(src: &str) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        unsafe {
            let url = if src.starts_with("http://") || src.starts_with("https://") {
                NSURL::URLWithString(&NSString::from_str(src))?
            } else {
                NSURL::fileURLWithPath(&NSString::from_str(src))
            };
            let item = AVPlayerItem::playerItemWithURL(&url, mtm);
            // The renderer takes bi-planar 4:2:0 frames.
            let key: &NSString = &*(kCVPixelBufferPixelFormatTypeKey as *const _ as *const NSString);
            let format = NSNumber::numberWithUnsignedInt(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
            let attrs: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(&[key], &[&*format as &AnyObject]);
            let output = AVPlayerItemVideoOutput::initWithPixelBufferAttributes(AVPlayerItemVideoOutput::alloc(), Some(&attrs));
            item.addOutput(&output);
            let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
            player.setMuted(true);
            player.setActionAtItemEnd(AVPlayerActionAtItemEnd::None);
            player.play();
            Some(Player { player, item, output, frame: RefCell::new(None) })
        }
    }

    /** The newest frame (the last one when nothing newer is ready); at the end it starts over. */
    fn frame(&self) -> Option<CFRetained<CVPixelBuffer>> {
        unsafe {
            let now: CMTime = self.item.currentTime();
            let end: CMTime = self.item.duration();
            if end.seconds() > 0. && now.seconds() >= end.seconds() - 0.05 {
                self.player.seekToTime(kCMTimeZero);
                self.player.play();
            }
            if self.output.hasNewPixelBufferForItemTime(now) {
                if let Some(buf) = self.output.copyPixelBufferForItemTime_itemTimeForDisplay(now, std::ptr::null_mut()) {
                    let raw = Retained::into_raw(buf);
                    *self.frame.borrow_mut() = std::ptr::NonNull::new(raw).map(|p| CFRetained::from_raw(p));
                }
            }
        }
        self.frame.borrow().clone()
    }
}

thread_local! {
    static PLAYERS: RefCell<HashMap<String, Option<Rc<Player>>>> = RefCell::new(HashMap::new());
}

fn player(src: &str) -> Option<Rc<Player>> {
    PLAYERS.with(|p| p.borrow_mut().entry(src.to_string()).or_insert_with(|| Player::open(src).map(Rc::new)).clone())
}

/** A video filling its box, fitted like an image (`cover`, `contain`, `fill`, `scale_down`, `none`). */
pub fn element(src: &str, fit: ObjectFit) -> AnyElement {
    let Some(p) = player(src) else { return div().into_any_element() };
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            if let Some(buf) = p.frame() {
                let size = size(DevicePixels::from(CVPixelBufferGetWidth(&buf) as i32), DevicePixels::from(CVPixelBufferGetHeight(&buf) as i32));
                let fitted = fit.get_bounds(bounds, size);
                window.paint_surface(fitted, buf);
            }
            window.request_animation_frame();
        },
    )
    .size_full()
    .into_any_element()
}
