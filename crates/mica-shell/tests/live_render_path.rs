//! The window renders through the non-blocking path.
//!
//! Regression test for the bug that made Mica unusable: the view rendered
//! through `Renderer::render_to_texture`, which is the *offscreen* path. It
//! ends in `synchronizeResource` and `waitUntilCompleted` — both correct when
//! the point is to read pixels back in a test, both ruinous in a window,
//! because the drawable is held for the whole GPU execution and only handed
//! back to Core Animation afterwards.
//!
//! Measured on this machine before the fix: `nextDrawable` blocked the main
//! thread for **36.9 ms per frame** while the render itself took 1.1 ms. After:
//! 16.5 ms per frame, which is this display's refresh interval — the wait is
//! now vsync rather than a stall, and every keystroke no longer queues behind
//! a GPU round trip.
//!
//! Nothing in the type system distinguishes the two calls, so the guard is
//! textual and crude, in the same spirit as `layering.rs`.

#![allow(unused_unsafe)]

use objc2_core_foundation::CGSize;
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLPixelFormat};
use objc2_quartz_core::CAMetalLayer;

fn view_source() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("view.rs");
    let text = std::fs::read_to_string(&path).expect("view.rs");
    // Prose mentions both names on purpose; only code counts.
    text.lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_window_does_not_render_through_the_blocking_offscreen_path() {
    let code = view_source();
    assert!(
        code.contains("render_to_drawable"),
        "view.rs no longer uses the non-blocking render path"
    );
    assert!(
        !code.contains("render_to_texture"),
        "view.rs is rendering the window through render_to_texture, which ends \
         in waitUntilCompleted. That is a five-frames-a-second window and the \
         only symptom is that everything feels slow."
    );
}

#[test]
fn the_layer_does_not_present_with_transaction() {
    // `presentsWithTransaction` defers the present until Core Animation
    // commits, so the drawable is not reclaimed until the run loop turns. With
    // two drawables that is a stall per frame, and it is invisible in every
    // offscreen test because no test has a CAMetalLayer.
    let code = view_source();
    assert!(
        code.contains("setPresentsWithTransaction(false)"),
        "the layer presents with transaction again — measure nextDrawable before \
         keeping this"
    );
}

#[test]
fn a_real_drawable_can_be_rendered_into_and_returns_without_blocking() {
    // The live path had never been executed by a test, which is the reason a
    // function referenced in its own sibling's doc comment could simply not
    // exist. This renders into an actual CAMetalLayer drawable.
    let Some(device) = MTLCreateSystemDefaultDevice() else { return };

    let layer = unsafe { CAMetalLayer::new() };
    unsafe {
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setDrawableSize(CGSize { width: 256.0, height: 128.0 });
        layer.setPresentsWithTransaction(false);
    }

    let root = std::env::temp_dir().join(format!("mica-live-render-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut surface = mica_shell::surface::Surface::open(
        mica_core::settings::Settings::default(),
        (256, 128),
        2.0,
        root.clone(),
        None,
    )
    .expect("a surface should open");

    for _ in 0..3 {
        let drawable = unsafe { layer.nextDrawable() }.expect("a drawable");
        let texture = unsafe { objc2_quartz_core::CAMetalDrawable::texture(&*drawable) };
        let as_drawable = objc2::runtime::ProtocolObject::from_ref(&*drawable);
        surface
            .render_to_drawable(as_drawable, &texture)
            .expect("the live render path should succeed");
    }

    let _ = std::fs::remove_dir_all(&root);
}
