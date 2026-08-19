//! The `NSView` that owns a `CAMetalLayer`, and the event plumbing around it.
//!
//! The class is registered at runtime through `objc2`'s `define_class!`, which
//! goes through `objc_allocateClassPair` — the same mechanism visible in the
//! reference binary's symbol table.
//!
//! ## Why there is no timer in this file
//!
//! The PTY reader thread fires a wakeup that hops to the main queue, and the
//! main queue calls [`MicaView::pump`]. Output arriving is what causes work; a
//! window with nothing happening in it receives no callbacks at all. That is
//! the top of the chain whose bottom is "an idle window submits zero command
//! buffers".

// allow lint: `objc2` moves methods between `unsafe` and safe across minor
// versions, and which ones are which is not part of its stability promise. The
// blocks below are kept because they document "this is a message send into
// AppKit", and dropping them would mean re-auditing every call site on each
// objc2 upgrade. Nothing here is unsound; the warning is about redundancy.
#![allow(unused_unsafe)]
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DeclaredClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSView};
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSNotification, NSObjectProtocol, NSRect, NSSize};
use objc2_metal::MTLDrawable;
use objc2_quartz_core::{CALayer, CAMetalDrawable, CAMetalLayer};

use mica_core::session::SessionEvent;

use crate::keys::{CursorKeyMode, Key, KeyConfig, Modifiers};
use crate::surface::Surface;

// libdispatch, declared here rather than pulled in as a dependency: two symbols
// is not worth a crate, and the ABI has been stable for fifteen years.
extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(
        queue: *const c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

/// AppKit virtual key codes for the keys that are not characters.
mod keycode {
    pub const RETURN: u16 = 0x24;
    pub const TAB: u16 = 0x30;
    pub const DELETE: u16 = 0x33; // labelled Backspace on a Mac keyboard
    pub const ESCAPE: u16 = 0x35;
    pub const FORWARD_DELETE: u16 = 0x75;
    pub const HOME: u16 = 0x73;
    pub const END: u16 = 0x77;
    pub const PAGE_UP: u16 = 0x74;
    pub const PAGE_DOWN: u16 = 0x79;
    pub const LEFT: u16 = 0x7B;
    pub const RIGHT: u16 = 0x7C;
    pub const DOWN: u16 = 0x7D;
    pub const UP: u16 = 0x7E;
    pub const KEYPAD_ENTER: u16 = 0x4C;
}

/// Translates an `NSEvent` into a [`Key`].
///
/// The characters come from `charactersIgnoringModifiers`, so the keyboard
/// layout, dead keys, and input methods are all still AppKit's job — Mica does
/// not reimplement any of that, which is why a Dvorak or AZERTY layout works
/// without a single line of code here knowing about it.
pub fn decode(key_code: u16, characters: &str, modifiers: Modifiers) -> Option<Key> {
    let named = match key_code {
        keycode::RETURN | keycode::KEYPAD_ENTER => Some(Key::Enter),
        keycode::TAB => Some(Key::Tab),
        keycode::DELETE => Some(Key::Backspace),
        keycode::FORWARD_DELETE => Some(Key::Delete),
        keycode::ESCAPE => Some(Key::Escape),
        keycode::UP => Some(Key::Up),
        keycode::DOWN => Some(Key::Down),
        keycode::LEFT => Some(Key::Left),
        keycode::RIGHT => Some(Key::Right),
        keycode::HOME => Some(Key::Home),
        keycode::END => Some(Key::End),
        keycode::PAGE_UP => Some(Key::PageUp),
        keycode::PAGE_DOWN => Some(Key::PageDown),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    // Function keys arrive as private-use characters rather than as key codes.
    let mut chars = characters.chars();
    let ch = chars.next()?;
    if ('\u{F704}'..='\u{F717}').contains(&ch) {
        return Some(Key::Function((ch as u32 - 0xF703) as u8));
    }
    // Everything else in the private-use area is a key AppKit has a name for
    // and Mica does not handle; sending it as text would insert garbage.
    if ('\u{F700}'..='\u{F8FF}').contains(&ch) {
        return None;
    }
    let _ = modifiers;
    Some(Key::Char(ch))
}

pub fn modifiers_from(flags: NSEventModifierFlags) -> Modifiers {
    Modifiers {
        shift: flags.contains(NSEventModifierFlags::Shift),
        control: flags.contains(NSEventModifierFlags::Control),
        alt: flags.contains(NSEventModifierFlags::Option),
        command: flags.contains(NSEventModifierFlags::Command),
    }
}

pub struct MicaViewState {
    pub surface: RefCell<Option<Surface>>,
    pub key_config: RefCell<KeyConfig>,
    /// Set while a pump is already queued on the main queue, so a burst of
    /// output produces one hop rather than one per chunk.
    pub pump_queued: Arc<AtomicBool>,
    /// Cleared by [`MicaView::detach`] before the surface is dropped. A wakeup
    /// that arrives after that point is discarded rather than dereferencing a
    /// view that is on its way out.
    pub alive: Arc<AtomicBool>,
}

/// What crosses the thread boundary to the main queue.
///
/// The view pointer is **not** owning. `Retained<MicaView>` is neither `Send`
/// nor `Sync` — correctly, because an `NSView` must not be released off the
/// main thread — so ownership cannot travel with it. Liveness is guaranteed
/// instead by ordering: [`MicaView::detach`] clears `alive` *before* dropping
/// the surface, and dropping the surface is what stops the reader thread. A
/// wakeup already in flight finds `alive` false and does nothing.
struct WakeupPayload {
    view: *const MicaView,
    alive: Arc<AtomicBool>,
    queued: Arc<AtomicBool>,
}

// SAFETY: the pointer is only ever dereferenced by `pump_trampoline`, which
// runs on the main queue, and only while `alive` is set.
unsafe impl Send for WakeupPayload {}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "MicaView"]
    #[ivars = MicaViewState]
    pub struct MicaView;

    unsafe impl NSObjectProtocol for MicaView {}

    impl MicaView {
        /// A `CAMetalLayer` rather than a plain layer. Returning it from
        /// `makeBackingLayer` is what makes the view Metal-backed without an
        /// `MTKView`, which brings its own display link and its own redraw
        /// policy — precisely what Mica does not want.
        #[unsafe(method_id(makeBackingLayer))]
        fn make_backing_layer(&self) -> Retained<CALayer> {
            let layer = unsafe { CAMetalLayer::new() };
            unsafe {
                layer.setPixelFormat(mica_gpu::context::DRAWABLE_FORMAT);
                // Two, not three: a terminal is a latency instrument, and the
                // third buffer buys smoothness nobody asked for at the cost of
                // a frame between keystroke and glyph.
                layer.setMaximumDrawableCount(2);
                // Present in the same transaction as the layer tree, so a
                // resize does not show a frame of stale content.
                layer.setPresentsWithTransaction(true);
                layer.setAllowsNextDrawableTimeout(false);
            }
            Retained::into_super(layer)
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let modifiers = modifiers_from(unsafe { event.modifierFlags() });
            let key_code = unsafe { event.keyCode() };
            let characters = unsafe { event.charactersIgnoringModifiers() }
                .map(|s| s.to_string())
                .unwrap_or_default();

            // Overlay shortcuts are checked before anything else so that ⌘F
            // works even while an overlay already has the keyboard.
            if self.handle_shortcut(&characters, modifiers) {
                self.redraw();
                return;
            }
            // An open overlay owns the keyboard. Without this, typing a query
            // would also type it into the shell.
            if self.handle_overlay_key(key_code, &characters, modifiers) {
                self.redraw();
                return;
            }

            let Some(key) = decode(key_code, &characters, modifiers) else { return };
            let config = *self.ivars().key_config.borrow();
            let Some(bytes) = crate::keys::encode(&key, modifiers, config) else { return };

            if let Some(surface) = self.ivars().surface.borrow_mut().as_mut() {
                surface.write_input(&bytes);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let delta = unsafe { event.scrollingDeltaY() };
            if delta == 0.0 {
                return;
            }
            // Scroll up (positive delta) moves the viewport into history.
            let lines = (delta / 3.0).round() as i32;
            if lines == 0 {
                return;
            }
            if let Some(surface) = self.ivars().surface.borrow_mut().as_mut() {
                surface.scroll(lines);
            }
            self.redraw();
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            unsafe { msg_send![super(self), setFrameSize: size] }
            self.synchronise_layer();
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            unsafe { msg_send![super(self), viewDidChangeBackingProperties] }
            self.synchronise_layer();
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            self.draw();
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            self.set_focused(true);
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            self.set_focused(false);
        }
    }
);

impl MicaView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<MicaView> {
        let this = MicaView::alloc(mtm).set_ivars(MicaViewState {
            surface: RefCell::new(None),
            key_config: RefCell::new(KeyConfig::default()),
            pump_queued: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(true)),
        });
        let this: Retained<MicaView> = unsafe { msg_send![super(this), initWithFrame: frame] };
        unsafe {
            this.setWantsLayer(true);
        }
        this
    }

    pub fn attach(&self, surface: Surface) {
        *self.ivars().surface.borrow_mut() = Some(surface);
        self.synchronise_layer();
    }

    /// A wakeup the PTY reader thread can call from any thread.
    ///
    /// This is what makes the UI event-driven. Without it the only way to
    /// notice new output would be to ask on a timer, and an idle terminal
    /// would wake the CPU hundreds of times a second to be told nothing
    /// happened.
    pub fn wakeup(&self) -> mica_core::pty::Wakeup {
        let view = self as *const MicaView;
        let alive = Arc::clone(&self.ivars().alive);
        let queued = Arc::clone(&self.ivars().pump_queued);

        struct SendView(*const MicaView);
        impl SendView {
            // A method rather than a public field: closure capture in Rust
            // 2021 is per-field, so reading `.0` directly would capture the
            // bare pointer and lose the `Send` impl this wrapper exists for.
            fn get(&self) -> *const MicaView {
                self.0
            }
        }
        // SAFETY: dereferenced only on the main queue, only while `alive`.
        unsafe impl Send for SendView {}
        unsafe impl Sync for SendView {}
        let view = SendView(view);

        Arc::new(move || {
            if !alive.load(Ordering::Acquire) {
                return;
            }
            // Coalesce: `cat` of a large file produces thousands of chunks and
            // should schedule one pump, not thousands.
            if queued.swap(true, Ordering::AcqRel) {
                return;
            }
            let payload = Box::new(WakeupPayload {
                view: view.get(),
                alive: Arc::clone(&alive),
                queued: Arc::clone(&queued),
            });
            let context = Box::into_raw(payload) as *mut c_void;
            // SAFETY: `pump_trampoline` reconstitutes and drops the box.
            unsafe {
                dispatch_async_f(&raw const _dispatch_main_q, context, pump_trampoline);
            }
        })
    }

    /// Tears the view down in the order the wakeup contract requires: mark it
    /// dead first, *then* drop the surface, because dropping the surface is
    /// what stops the reader thread that fires wakeups.
    pub fn detach(&self) {
        self.ivars().alive.store(false, Ordering::Release);
        let _ = self.ivars().surface.borrow_mut().take();
    }

    /// Reads whatever the PTY produced and redraws if anything changed.
    pub fn pump(&self) -> Vec<SessionEvent> {
        let events = match self.ivars().surface.borrow_mut().as_mut() {
            Some(surface) => surface.pump(),
            None => Vec::new(),
        };
        self.redraw();
        events
    }

    /// Draws if — and only if — the scheduler says a frame is owed.
    pub fn redraw(&self) {
        let mut borrowed = self.ivars().surface.borrow_mut();
        let Some(surface) = borrowed.as_mut() else { return };
        if !mica_gpu::renderer::should_draw(surface.scheduler()) {
            return;
        }
        drop(borrowed);
        self.draw();
    }

    fn draw(&self) {
        let Some(layer) = self.metal_layer() else { return };
        let mut borrowed = self.ivars().surface.borrow_mut();
        let Some(surface) = borrowed.as_mut() else { return };

        let Some(drawable) = (unsafe { layer.nextDrawable() }) else {
            // The pool was exhausted. The frame is still owed, so put the
            // reason back rather than losing it.
            surface.scheduler().drop_frame(mica_gpu::frame::Reason::Damage);
            return;
        };

        let texture = unsafe { drawable.texture() };
        if surface.render_to_texture(&texture).is_ok() {
            surface.scheduler().begin_frame();
            // `presentsWithTransaction` means presenting is a synchronous act
            // inside the current CATransaction rather than a scheduled one.
            unsafe { drawable.present() };
            surface.scheduler().end_frame();
        }
    }

    fn set_focused(&self, focused: bool) {
        if let Some(surface) = self.ivars().surface.borrow_mut().as_mut() {
            surface.set_focused(focused);
        }
        self.redraw();
    }

    fn metal_layer(&self) -> Option<Retained<CAMetalLayer>> {
        let layer = unsafe { self.layer() }?;
        layer.downcast::<CAMetalLayer>().ok()
    }

    /// Keeps the layer's pixel geometry in step with the view's, in device
    /// pixels rather than points.
    ///
    /// Getting this wrong is the classic Retina bug: the drawable is half the
    /// size it should be and everything is soft.
    fn synchronise_layer(&self) {
        let Some(layer) = self.metal_layer() else { return };
        let scale = unsafe {
            self.window().map(|w| w.backingScaleFactor()).unwrap_or(2.0)
        };
        let bounds = self.bounds();
        let width = (bounds.size.width * scale).round().max(1.0);
        let height = (bounds.size.height * scale).round().max(1.0);

        unsafe {
            layer.setContentsScale(scale);
            layer.setDrawableSize(CGSize { width, height });
        }
        if let Some(surface) = self.ivars().surface.borrow_mut().as_mut() {
            surface.resize((width as u32, height as u32), scale as f32);
        }
        self.redraw();
    }

    /// Updates the cursor-key mode when the terminal switches DECCKM.
    pub fn set_cursor_key_mode(&self, mode: CursorKeyMode) {
        self.ivars().key_config.borrow_mut().cursor_keys = mode;
    }

    pub fn title(&self) -> String {
        self.ivars()
            .surface
            .borrow()
            .as_ref()
            .map(|s| s.title().to_owned())
            .unwrap_or_else(|| "Mica".to_owned())
    }

    /// Handles the Command shortcuts Mica owns.
    ///
    /// Returns whether the key was consumed. These are checked before overlay
    /// input so `⌘F` still works while the palette is open.
    fn handle_shortcut(&self, characters: &str, modifiers: Modifiers) -> bool {
        if !modifiers.command {
            return false;
        }
        let Some(surface) = self.ivars().surface.borrow_mut().as_mut().map(|s| s as *mut Surface)
        else {
            return false;
        };
        // SAFETY: the borrow ends with the statement above; nothing else on
        // this thread touches the surface in between.
        let surface = unsafe { &mut *surface };

        match characters {
            "p" | "P" if modifiers.shift => {
                surface.toggle_palette();
                true
            }
            "f" | "F" => {
                surface.toggle_find();
                true
            }
            "g" | "G" => {
                // ⌘G / ⌘⇧G step through matches, as they do everywhere else on
                // macOS.
                surface.overlay_step(!modifiers.shift)
            }
            _ => false,
        }
    }

    /// Routes a key to an open overlay. Returns whether it was consumed.
    fn handle_overlay_key(
        &self,
        key_code: u16,
        characters: &str,
        modifiers: Modifiers,
    ) -> bool {
        let Some(surface) = self.ivars().surface.borrow_mut().as_mut().map(|s| s as *mut Surface)
        else {
            return false;
        };
        // SAFETY: as above.
        let surface = unsafe { &mut *surface };
        if !surface.overlay_has_focus() {
            return false;
        }

        match key_code {
            keycode::ESCAPE => {
                surface.close_overlays();
                true
            }
            keycode::RETURN | keycode::KEYPAD_ENTER => {
                if let Some(id) = surface.overlay_accept() {
                    if !surface.dispatch(&id) {
                        eprintln!("mica: action `{id}` is not implemented yet");
                    }
                }
                true
            }
            keycode::DELETE => {
                surface.overlay_backspace();
                true
            }
            keycode::DOWN => {
                surface.overlay_step(true);
                true
            }
            keycode::UP => {
                surface.overlay_step(false);
                true
            }
            _ => {
                // Control combinations are not text; swallowing them silently
                // would make Ctrl-C inside a palette do nothing at all rather
                // than closing it.
                if modifiers.control || modifiers.command {
                    if modifiers.control && characters.eq_ignore_ascii_case("c") {
                        surface.close_overlays();
                        return true;
                    }
                    return false;
                }
                let mut consumed = false;
                for ch in characters.chars() {
                    if surface.overlay_char(ch) {
                        consumed = true;
                    }
                }
                consumed
            }
        }
    }

    /// The current selection, for Copy.
    pub fn selection_text(&self) -> Option<String> {
        self.ivars().surface.borrow().as_ref().and_then(Surface::selection_text)
    }

    /// Sends pasted text, wrapped in bracketed-paste markers.
    ///
    /// Bracketed paste is what stops a pasted script from executing line by
    /// line as it arrives — the single most dangerous default a terminal can
    /// get wrong.
    pub fn paste(&self, text: &str) {
        let Some(surface) = self.ivars().surface.borrow_mut().as_mut().map(|s| s as *mut Surface)
        else {
            return;
        };
        // SAFETY: the borrow above is released at the end of the statement and
        // nothing else touches the surface on this thread in between.
        let surface = unsafe { &mut *surface };
        surface.write_input(b"\x1b[200~");
        surface.write_input(text.as_bytes());
        surface.write_input(b"\x1b[201~");
    }

    pub fn has_exited(&self) -> bool {
        self.ivars().surface.borrow().as_ref().is_some_and(Surface::has_exited)
    }
}

/// Runs on the main queue. Owns and drops the payload.
extern "C" fn pump_trampoline(context: *mut c_void) {
    // SAFETY: `context` came from `Box::into_raw` in `wakeup`.
    let payload = unsafe { Box::from_raw(context as *mut WakeupPayload) };
    payload.queued.store(false, Ordering::Release);
    if !payload.alive.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: `alive` is still set, and it is only cleared on this thread in
    // `detach`, before the view goes away.
    let view = unsafe { &*payload.view };
    view.pump();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_are_decoded_from_their_key_codes() {
        assert_eq!(decode(keycode::RETURN, "\r", Modifiers::NONE), Some(Key::Enter));
        assert_eq!(decode(keycode::TAB, "\t", Modifiers::NONE), Some(Key::Tab));
        assert_eq!(decode(keycode::ESCAPE, "\x1b", Modifiers::NONE), Some(Key::Escape));
        assert_eq!(decode(keycode::UP, "", Modifiers::NONE), Some(Key::Up));
        assert_eq!(decode(keycode::PAGE_DOWN, "", Modifiers::NONE), Some(Key::PageDown));
    }

    #[test]
    fn the_mac_delete_key_is_backspace_and_forward_delete_is_delete() {
        // On a Mac keyboard the key labelled "delete" is backspace. Getting
        // these two the wrong way round is a bug users describe as "my
        // backspace deletes forwards".
        assert_eq!(decode(keycode::DELETE, "", Modifiers::NONE), Some(Key::Backspace));
        assert_eq!(decode(keycode::FORWARD_DELETE, "", Modifiers::NONE), Some(Key::Delete));
    }

    #[test]
    fn keypad_enter_is_enter() {
        assert_eq!(decode(keycode::KEYPAD_ENTER, "\r", Modifiers::NONE), Some(Key::Enter));
    }

    #[test]
    fn ordinary_characters_come_from_the_keyboard_layout() {
        // The layout, dead keys, and input methods are AppKit's job. This is
        // why a Dvorak or AZERTY layout needs no code here.
        assert_eq!(decode(0, "a", Modifiers::NONE), Some(Key::Char('a')));
        assert_eq!(decode(0, "é", Modifiers::NONE), Some(Key::Char('é')));
        assert_eq!(decode(0, "世", Modifiers::NONE), Some(Key::Char('世')));
    }

    #[test]
    fn function_keys_arrive_as_private_use_characters() {
        assert_eq!(decode(0, "\u{F704}", Modifiers::NONE), Some(Key::Function(1)));
        assert_eq!(decode(0, "\u{F707}", Modifiers::NONE), Some(Key::Function(4)));
        assert_eq!(decode(0, "\u{F708}", Modifiers::NONE), Some(Key::Function(5)));
        assert_eq!(decode(0, "\u{F717}", Modifiers::NONE), Some(Key::Function(20)));
    }

    #[test]
    fn other_private_use_keys_are_dropped_rather_than_inserted_as_text() {
        // AppKit uses this range for keys Mica does not handle. Passing one
        // through would insert an invisible character into the shell.
        assert_eq!(decode(0, "\u{F729}", Modifiers::NONE), None); // Home, as a character
        assert_eq!(decode(0, "\u{F800}", Modifiers::NONE), None);
    }

    #[test]
    fn an_empty_event_decodes_to_nothing() {
        assert_eq!(decode(0, "", Modifiers::NONE), None);
    }

    #[test]
    fn decoding_and_encoding_compose_into_the_right_bytes() {
        // The two halves of the input path, checked together: a key code and
        // its characters go in, terminal bytes come out.
        let cases: &[(u16, &str, Modifiers, &[u8])] = &[
            (keycode::RETURN, "\r", Modifiers::NONE, b"\r"),
            (keycode::DELETE, "", Modifiers::NONE, &[0x7f]),
            (keycode::UP, "", Modifiers::NONE, b"\x1b[A"),
            (keycode::LEFT, "", Modifiers::control(), b"\x1b[1;5D"),
            (0, "c", Modifiers::control(), &[0x03]),
            (0, "b", Modifiers::alt(), &[0x1b, b'b']),
        ];
        for (code, characters, modifiers, expected) in cases {
            let key = decode(*code, characters, *modifiers)
                .unwrap_or_else(|| panic!("{code:#x} {characters:?} decoded to nothing"));
            let bytes = crate::keys::encode(&key, *modifiers, KeyConfig::default())
                .unwrap_or_else(|| panic!("{key:?} encoded to nothing"));
            assert_eq!(bytes, *expected, "{key:?} with {modifiers:?}");
        }
    }
}
