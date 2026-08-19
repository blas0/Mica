//! A `CAMetalLayer` with no device vends no drawables — silently.
//!
//! This is a regression test for the bug that made every Mica window a blank
//! rectangle. `makeBackingLayer` returns a `CAMetalLayer` configured with a
//! pixel format, a drawable count, and a drawable size — everything except a
//! device, because the device belongs to the renderer, which does not exist
//! until a surface is opened. A device-less layer answers `nextDrawable` with
//! nil forever. Nothing raises, nothing logs, no Metal validation layer
//! complains: the frame is simply never drawn.
//!
//! `MicaView::attach` sets the device, and this test pins the underlying
//! behaviour so the reason that call exists survives the next person reading
//! `makeBackingLayer` and wondering why the layer is configured in two places.

// objc2 moves these methods between unsafe and safe across minor versions;
// the blocks are written for the stricter reading and suppressed here rather
// than churning every call site on each bump.
#![allow(unused_unsafe)]

use objc2_core_foundation::CGSize;
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLPixelFormat};
use objc2_quartz_core::CAMetalLayer;

/// Offscreen, exactly as the view configures it, minus the device.
fn layer() -> objc2::rc::Retained<CAMetalLayer> {
    let layer = unsafe { CAMetalLayer::new() };
    unsafe {
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setDrawableSize(CGSize { width: 64.0, height: 64.0 });
        // Without this the nil-device case blocks for a second before giving
        // up, which would make a failing test look like a hung one.
        layer.setAllowsNextDrawableTimeout(true);
    }
    layer
}

#[test]
fn a_layer_without_a_device_vends_nothing() {
    let layer = layer();
    assert!(
        unsafe { layer.device() }.is_none(),
        "a fresh CAMetalLayer is expected to start with no device"
    );
    assert!(
        unsafe { layer.nextDrawable() }.is_none(),
        "a device-less CAMetalLayer vended a drawable — if this ever becomes \
         true, the blank-window failure mode this test guards has changed \
         shape and MicaView::attach should be re-read, not deleted"
    );
}

#[test]
fn the_same_layer_vends_drawables_once_it_has_a_device() {
    let Some(device) = MTLCreateSystemDefaultDevice() else {
        // A machine with no Metal device cannot run Mica at all; there is
        // nothing useful to assert here.
        return;
    };
    let layer = layer();
    unsafe { layer.setDevice(Some(&device)) };

    assert!(
        unsafe { layer.nextDrawable() }.is_some(),
        "the layer still vends no drawable with a device set — every window \
         will be blank"
    );
}

#[test]
fn the_view_still_gives_its_layer_a_device() {
    // The behaviour above is only useful if something acts on it. This is the
    // crude half: `attach` is the one place that can set the device, because
    // it is the first moment a renderer exists.
    let view = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("view.rs");
    let text = std::fs::read_to_string(&view).expect("view.rs");
    let code: String = text
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("setDevice"),
        "nothing in view.rs sets the layer's Metal device any more. Every \
         window will be blank and nothing will say so — see the tests above."
    );
}
