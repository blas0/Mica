//! The application: one delegate, one window per terminal, a menu.
//!
//! Windows use AppKit's native tabbing (`tabbingMode`) rather than a
//! hand-rolled tab bar. Each tab is a real `NSWindow` with its own surface,
//! which means each has its own shell, working directory, and scrollback for
//! free — and the tab bar, its overflow menu, and full-screen behaviour are
//! AppKit's problem rather than ours.

// allow lint: `objc2` moves methods between `unsafe` and safe across minor
// versions, and which ones are which is not part of its stability promise. The
// blocks below are kept because they document "this is a message send into
// AppKit", and dropping them would mean re-auditing every call site on each
// objc2 upgrade. Nothing here is unsound; the warning is about redundancy.
#![allow(unused_unsafe)]
use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DeclaredClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSMenu, NSMenuItem, NSPasteboard, NSPasteboardTypeString, NSWindow,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask, NSWindowTabbingMode,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use mica_core::session::SessionEvent;
use mica_core::settings::Settings;

use crate::surface::Surface;
use crate::view::MicaView;

/// Where a session's temporary integration directory lives.
fn integration_root(index: u64) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("mica-{}-{index}", std::process::id()))
}

pub struct AppState {
    pub settings: RefCell<Settings>,
    pub windows: RefCell<Vec<Retained<NSWindow>>>,
    pub next_session: RefCell<u64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MicaAppDelegate"]
    #[ivars = AppState]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::from(self);
            self.open_window(mtm);
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            true
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, notification: &NSNotification) {
            let Some(object) = (unsafe { notification.object() }) else { return };
            let Ok(window) = object.downcast::<NSWindow>() else { return };

            // Order matters: the view has to be detached — which stops the
            // reader thread — before the window releases it.
            if let Some(view) = unsafe { window.contentView() } {
                if let Ok(view) = view.downcast::<MicaView>() {
                    view.detach();
                }
            }
            self.ivars().windows.borrow_mut().retain(|w| **w != *window);
        }
    }

    impl AppDelegate {
        #[unsafe(method(newWindow:))]
        fn new_window(&self, _sender: Option<&NSObject>) {
            let mtm = MainThreadMarker::from(self);
            self.open_window(mtm);
        }

        #[unsafe(method(copySelection:))]
        fn copy_selection(&self, _sender: Option<&NSObject>) {
            let mtm = MainThreadMarker::from(self);
            let Some(view) = key_view(mtm) else { return };
            let Some(text) = view.selection_text() else { return };
            let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
            unsafe {
                pasteboard.clearContents();
                pasteboard.setString_forType(
                    &NSString::from_str(&text),
                    NSPasteboardTypeString,
                );
            }
        }

        #[unsafe(method(pasteText:))]
        fn paste_text(&self, _sender: Option<&NSObject>) {
            let mtm = MainThreadMarker::from(self);
            let Some(view) = key_view(mtm) else { return };
            let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
            let Some(text) =
                (unsafe { pasteboard.stringForType(NSPasteboardTypeString) })
            else {
                return;
            };
            view.paste(&text.to_string());
        }
    }
);

/// The `MicaView` of the key window, if there is one.
fn key_view(mtm: MainThreadMarker) -> Option<Retained<MicaView>> {
    let app = NSApplication::sharedApplication(mtm);
    let window = unsafe { app.keyWindow() }?;
    let view = unsafe { window.contentView() }?;
    view.downcast::<MicaView>().ok()
}

impl AppDelegate {
    pub fn new(mtm: MainThreadMarker, settings: Settings) -> Retained<AppDelegate> {
        let this = AppDelegate::alloc(mtm).set_ivars(AppState {
            settings: RefCell::new(settings),
            windows: RefCell::new(Vec::new()),
            next_session: RefCell::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }

    /// Opens a window with a fresh shell in it.
    pub fn open_window(&self, mtm: MainThreadMarker) {
        let settings = self.ivars().settings.borrow().clone();
        let index = {
            let mut next = self.ivars().next_session.borrow_mut();
            *next += 1;
            *next
        };

        // The initial size comes from the configured grid rather than from a
        // remembered frame: a terminal that opens at 118×34 every time is more
        // predictable than one that opens wherever it was last dragged.
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(
                settings.columns as f64 * 8.0,
                settings.rows as f64 * 17.0,
            ),
        );

        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe {
            window.setTitle(&NSString::from_str("Mica"));
            window.setTitlebarAppearsTransparent(true);
            // Native tabbing: every tab is a real window with its own shell,
            // working directory, and scrollback, and the tab bar is AppKit's
            // to draw.
            window.setTabbingMode(NSWindowTabbingMode::Preferred);
            window.setTabbingIdentifier(&NSString::from_str("dev.mica.terminal"));
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::FullScreenPrimary,
            );
            window.setReleasedWhenClosed(false);
            window.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(self)));
        }

        let view = MicaView::new(mtm, frame);
        unsafe {
            window.setContentView(Some(&view));
            window.makeFirstResponder(Some(&view));
        }

        let scale = unsafe { window.backingScaleFactor() } as f32;
        let bounds = view.bounds();
        let viewport = (
            (bounds.size.width * scale as f64).round() as u32,
            (bounds.size.height * scale as f64).round() as u32,
        );

        // The wakeup goes in at construction: the reader thread starts inside
        // `Session::spawn`, so installing it afterwards would mean respawning
        // the shell.
        match Surface::open(settings, viewport, scale, integration_root(index), Some(view.wakeup()))
        {
            Ok(surface) => view.attach(surface),
            Err(error) => {
                eprintln!("mica: could not open a terminal: {error}");
                return;
            }
        }

        unsafe {
            window.center();
            window.makeKeyAndOrderFront(None);
        }
        view.pump();
        self.ivars().windows.borrow_mut().push(window);
    }

    /// Acts on the events a surface produced.
    pub fn handle(&self, events: &[SessionEvent], window: &NSWindow) {
        for event in events {
            match event {
                SessionEvent::TitleChanged(title) => unsafe {
                    window.setTitle(&NSString::from_str(title));
                },
                SessionEvent::Exited(_) => unsafe {
                    window.performClose(None);
                },
                // Notifications, the bell, and clipboard writes are Phase 9
                // work; nothing here should fail silently once they land.
                _ => {}
            }
        }
    }
}

/// Builds the menu bar.
///
/// Without an explicit menu there is no Quit item and no Cmd-Q, which makes an
/// app that can only be killed from Activity Monitor.
pub fn build_menu(mtm: MainThreadMarker, delegate: &AppDelegate) -> Retained<NSMenu> {
    let menubar = NSMenu::new(mtm);

    // --- application menu --------------------------------------------------
    let app_item = NSMenuItem::new(mtm);
    let app_menu = NSMenu::new(mtm);
    unsafe {
        let about = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("About Mica"),
            Some(objc2::sel!(orderFrontStandardAboutPanel:)),
            &NSString::from_str(""),
        );
        app_menu.addItem(&about);
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));

        let quit = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Quit Mica"),
            Some(objc2::sel!(terminate:)),
            &NSString::from_str("q"),
        );
        app_menu.addItem(&quit);
        app_item.setSubmenu(Some(&app_menu));
    }
    menubar.addItem(&app_item);

    // --- shell menu --------------------------------------------------------
    let shell_item = NSMenuItem::new(mtm);
    let shell_menu = unsafe {
        NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Shell"))
    };
    unsafe {
        let new_window = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("New Window"),
            Some(objc2::sel!(newWindow:)),
            &NSString::from_str("n"),
        );
        new_window.setTarget(Some(delegate));
        shell_menu.addItem(&new_window);

        let close = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Close"),
            Some(objc2::sel!(performClose:)),
            &NSString::from_str("w"),
        );
        shell_menu.addItem(&close);
        shell_item.setSubmenu(Some(&shell_menu));
    }
    menubar.addItem(&shell_item);

    // --- edit menu ---------------------------------------------------------
    let edit_item = NSMenuItem::new(mtm);
    let edit_menu =
        unsafe { NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Edit")) };
    unsafe {
        // Copy and paste target the delegate rather than the responder chain:
        // the terminal's selection is not an `NSTextView`'s, so the standard
        // `copy:` would find nothing.
        let copy = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Copy"),
            Some(objc2::sel!(copySelection:)),
            &NSString::from_str("c"),
        );
        copy.setTarget(Some(delegate));
        edit_menu.addItem(&copy);

        let paste = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Paste"),
            Some(objc2::sel!(pasteText:)),
            &NSString::from_str("v"),
        );
        paste.setTarget(Some(delegate));
        edit_menu.addItem(&paste);
        edit_item.setSubmenu(Some(&edit_menu));
    }
    menubar.addItem(&edit_item);

    // --- window menu -------------------------------------------------------
    let window_item = NSMenuItem::new(mtm);
    let window_menu =
        unsafe { NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Window")) };
    window_item.setSubmenu(Some(&window_menu));
    menubar.addItem(&window_item);

    menubar
}

/// Starts the application. Does not return until it quits.
pub fn run(settings: Settings) {
    let mtm = MainThreadMarker::new().expect("mica must start on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // Regular, not Accessory: Mica has a Dock icon and a menu bar.
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let delegate = AppDelegate::new(mtm, settings);
    let menu = build_menu(mtm, &delegate);
    app.setMainMenu(Some(&menu));
    unsafe {
        app.setWindowsMenu(Some(&menu.itemAtIndex(3).unwrap().submenu().unwrap()));
    }
    app.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*delegate)));

    app.activate();
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_session_gets_its_own_integration_directory() {
        // Two sessions sharing a generated ZDOTDIR would race on the same
        // .zshrc, and the second one to start would win.
        assert_ne!(integration_root(1), integration_root(2));
        assert!(integration_root(1).starts_with(std::env::temp_dir()));
    }

    #[test]
    fn the_integration_directory_is_never_inside_the_users_configuration() {
        let root = integration_root(1);
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
        assert!(!root.starts_with(home.join(".config")));
        assert!(!root.starts_with(home.join(".zshrc")));
    }
}
