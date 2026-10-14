/*! Native macOS bits: Notification Center banners (clicking one reports its chat back to the app), the
Dock badge, and bouncing the Dock icon. */

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
use objc2_foundation::{NSError, NSString, NSUserNotification, NSUserNotificationCenter, NSUserNotificationCenterDelegate};
use std::sync::atomic::{AtomicBool, Ordering};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::sync::OnceLock;

static CLICKS: OnceLock<flume::Sender<String>> = OnceLock::new();
/** Notification Center refuses ad-hoc-signed apps; then banners go through the older NSUserNotification API. */
static MODERN: AtomicBool = AtomicBool::new(false);

/** Notification Center throws for a bare binary; only an app bundle may use it. */
fn bundled() -> bool {
    std::env::current_exe().map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/")).unwrap_or(false)
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AgentBoxNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(&self, _: &UNUserNotificationCenter, _: &UNNotification, done: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>) {
            done.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::Sound | UNNotificationPresentationOptions::List,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(&self, _: &UNUserNotificationCenter, response: &UNNotificationResponse, done: &block2::DynBlock<dyn Fn()>) {
            let id = response.notification().request().identifier().to_string();
            if let Some(tx) = CLICKS.get() {
                let _ = tx.send(id);
            }
            done.call(());
        }
    }

    unsafe impl NSUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:didActivateNotification:))]
        fn did_activate(&self, _: &NSUserNotificationCenter, n: &NSUserNotification) {
            if let (Some(tx), Some(id)) = (CLICKS.get(), n.identifier()) {
                let _ = tx.send(id.to_string());
            }
        }

        #[unsafe(method(userNotificationCenter:shouldPresentNotification:))]
        fn should_present(&self, _: &NSUserNotificationCenter, _: &NSUserNotification) -> bool {
            true
        }
    }
);

/** Installs the delegate and asks for permission once; returns clicked notifications' ids. */
pub fn init() -> Option<flume::Receiver<String>> {
    let mtm = MainThreadMarker::new()?;
    if !bundled() {
        return None;
    }
    let (tx, rx) = flume::unbounded();
    CLICKS.set(tx).ok()?;
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    #[allow(deprecated)]
    unsafe {
        NSUserNotificationCenter::defaultUserNotificationCenter().setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }
    std::mem::forget(delegate);
    let done = RcBlock::new(|granted: Bool, err: *mut NSError| {
        let why = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
        MODERN.store(granted.as_bool(), Ordering::SeqCst);
        if !granted.as_bool() {
            log(&format!("Notification Center refused ({why}); using NSUserNotification"));
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge, &done);
    Some(rx)
}

/** Posts a banner; `id` comes back from `init`'s receiver when it's clicked. */
pub fn notify(id: &str, title: &str, body: &str, sound: bool) {
    if !bundled() {
        return;
    }
    if !MODERN.load(Ordering::SeqCst) {
        #[allow(deprecated)]
        {
            let n = NSUserNotification::new();
            n.setTitle(Some(&NSString::from_str(title)));
            n.setInformativeText(Some(&NSString::from_str(body)));
            n.setIdentifier(Some(&NSString::from_str(id)));
            if sound {
                n.setSoundName(Some(unsafe { objc2_foundation::NSUserNotificationDefaultSoundName }));
            }
            NSUserNotificationCenter::defaultUserNotificationCenter().deliverNotification(&n);
        }
        return;
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    if sound {
        content.setSound(Some(&UNNotificationSound::defaultSound()));
    }
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(id), &content, None);
    let done = RcBlock::new(|err: *mut NSError| {
        if let Some(e) = unsafe { err.as_ref() } {
            log(&format!("notify failed: {}", e.localizedDescription()));
        }
    });
    UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, Some(&done));
}

/** Notification Center problems (denied permission, failed posts) go to ~/.agent-box/notify.log. */
fn log(line: &str) {
    use std::io::Write;
    let path = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".agent-box/notify.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

pub fn set_badge(label: Option<&str>) {
    let Some(mtm) = MainThreadMarker::new() else {
        log("set_badge off the main thread");
        return;
    };
    let tile = NSApplication::sharedApplication(mtm).dockTile();
    tile.setBadgeLabel(label.map(NSString::from_str).as_deref());
    tile.display();
}

/** The main window's number, for `screencapture -l`. */
pub fn window_number() -> Option<i64> {
    let mtm = MainThreadMarker::new()?;
    let windows = NSApplication::sharedApplication(mtm).windows();
    windows.iter().find(|w| w.isVisible()).map(|w| w.windowNumber() as i64)
}

pub fn bounce() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    NSApplication::sharedApplication(mtm).requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
}
