//! [`TerminalCore`](crate::backend::TerminalCore) over **libghostty-vt** — the
//! primary backend, currently gated.
//!
//! # Why this file does not compile yet
//!
//! The toolchain is no longer the blocker. Zig 0.16.0 is installed,
//! `libghostty-vt` builds, and its C API has been exercised — see
//! *Verified by building it* below. What is left is the FFI and the trait
//! implementation itself, which is several hundred lines and has not been
//! written. Shipping that half-written, behind a feature flag that silently
//! fell back, is how a project ends up running the backend it meant to
//! replace.
//!
//! The [`alacritty`](super::alacritty) backend carries the build until then.
//! That is exactly what the trait boundary was bought for.
//!
//! # Verified by building it
//!
//! Pinned at `ca9e5b1301354018f92152c1282a922baacfa0e1` (recorded in
//! `vendor-ghostty.sha`; the tree itself is not committed). Built with:
//!
//! ```sh
//! git clone --depth 1 https://github.com/ghostty-org/ghostty vendor/ghostty
//! cd vendor/ghostty && zig build -Demit-lib-vt=true -Doptimize=ReleaseFast
//! # → zig-out/lib/libghostty-vt.a  (9.8 MB), headers in zig-out/include
//! ```
//!
//! ## The emoji width bug, measured
//!
//! This is the defect the backend exists to fix, and the measurement changed
//! what the fix is. A C probe against the built library, feeding
//! `U+26A0 U+FE0F` then `X` into a fresh terminal and reading the cells back:
//!
//! | | column 0 | column 1 | column 2 |
//! |---|---|---|---|
//! | default | `U+26A0` w=0 | `X` | — |
//! | mode 2027 on | `U+26A0` w=1 | spacer w=2 | `X` |
//!
//! **Out of the box, libghostty-vt gives `⚠️` one column — the same wrong
//! answer alacritty gives.** What fixes it is DEC mode **2027**, grapheme
//! clustering, under which `ghostty_unicode_grapheme_width` applies the
//! cluster rule: VS16 forces a cluster wide when the preceding codepoint is a
//! valid emoji variation base. `🚀` is two columns either way, which is why
//! the bare-emoji rendering always looked correct.
//!
//! So the backend swap alone does not fix emoji. **Mode 2027 has to be on**,
//! and it is `GHOSTTY_MODE_GRAPHEME_CLUSTER` in `modes.h`. alacritty 0.26 has
//! no 2027 at all — grep its source; the mode does not exist — which is what
//! makes this a backend change rather than an escape sequence Mica could send
//! today.
//!
//! ## The API, as it actually is
//!
//! The header map below was read from source before any of it was compiled.
//! Two corrections from having run it: construction is
//! `ghostty_terminal_new(allocator, &out, cols, rows)` — the out-parameter is
//! second, not last — and cells are read through a `GhosttyGridRef` resolved
//! from a `GhosttyPoint`, with `ghostty_cell_get(cell, GHOSTTY_CELL_DATA_*,
//! &out)` for each field. `GHOSTTY_CELL_DATA_WIDE` is the one that answers
//! this bug: `0` narrow, `1` the leading half of a wide cell, `2` its spacer,
//! which is [`CellFlags::WIDE_SPACER`](crate::cell::CellFlags) exactly.
//!
//! # What was verified against upstream
//!
//! Read from `include/ghostty/vt/{terminal,render}.h` at
//! `github.com/ghostty-org/ghostty@main`. The header opens with its own
//! warning: the API is incomplete, unstable, and expected to break.
//!
//! ## Lifecycle and input
//!
//! ```c
//! GhosttyResult ghostty_terminal_new(const GhosttyAllocator*, ..., GhosttyTerminal* out);
//! void          ghostty_terminal_free(GhosttyTerminal);
//! GhosttyResult ghostty_terminal_resize(GhosttyTerminal, ...);
//! void          ghostty_terminal_vt_write(GhosttyTerminal, const uint8_t*, size_t);
//! void          ghostty_terminal_scroll_viewport(GhosttyTerminal, GhosttyTerminalScrollViewport);
//! GhosttyResult ghostty_terminal_get(GhosttyTerminal, GhosttyTerminalData, void* out);
//! ```
//!
//! Construction is **option-based**, not struct-based: `ghostty_terminal_set`
//! with a `GhosttyTerminalOption`. The options Mica needs map onto the trait
//! almost one-to-one, which is the strongest evidence this is the right layer:
//!
//! | Option | Serves |
//! |---|---|
//! | `OPT_WRITE_PTY` | [`TerminalCore::take_replies`](crate::backend::TerminalCore::take_replies) — DA1/DSR answers |
//! | `OPT_TITLE_CHANGED`, `OPT_TITLE` | [`take_title`](crate::backend::TerminalCore::take_title) |
//! | `OPT_PWD_CHANGED`, `OPT_PWD` | [`SemanticEvent::Cwd`](crate::semantic::SemanticEvent::Cwd) — OSC 7, natively |
//! | `OPT_DESKTOP_NOTIFICATION` | `SemanticEvent::Notification` — OSC 9/777, natively |
//! | `OPT_CLIPBOARD_WRITE` | `SemanticEvent::ClipboardWrite` — OSC 52 |
//! | `OPT_BELL` | `SemanticEvent::Bell` |
//! | `OPT_SCROLLBACK_MAX_LINES` | the `scrollback` constructor argument |
//! | `OPT_DEFAULT_CURSOR_STYLE`, `OPT_DEFAULT_CURSOR_BLINK` | [`CursorState`](crate::backend::CursorState) |
//! | `OPT_DEVICE_ATTRIBUTES` | the `CSI ?62;22c` DA1 reply |
//! | `OPT_COLOR_SCHEME` | `CSI ?997;1n/2n` light/dark notification |
//! | `OPT_TERMINFO_NAME` | must be set to `mica`, and only after `tic -x` succeeds |
//!
//! `OPT_PROGRESS_REPORT` (ConEmu OSC 9;4) is also present. It is not needed for
//! v0.1, but it is the native hook for the progress-bar substitution the
//! reference implementation does — worth knowing it is free here.
//!
//! ## Render state — the reason this backend is the primary one
//!
//! ```c
//! GhosttyResult ghostty_render_state_new(const GhosttyAllocator*, ..., GhosttyRenderState* out);
//! GhosttyResult ghostty_render_state_update(GhosttyRenderState, GhosttyTerminal);
//! GhosttyResult ghostty_render_state_row_iterator_new(GhosttyRenderState, ...);
//! bool          ghostty_render_state_row_iterator_next_dirty(GhosttyRenderStateRowIterator, ...);
//! GhosttyResult ghostty_render_state_row_cells_new(...);
//! bool          ghostty_render_state_row_cells_next(GhosttyRenderStateRowCells);
//! GhosttyResult ghostty_render_state_clean(GhosttyRenderState);
//! ```
//!
//! `_next_dirty` exists precisely so a caller can bring its own renderer and
//! walk only what changed. It is [`TerminalCore::dirty_rows`] with a different
//! spelling, and it is why the trait was designed around damage rather than
//! around a full grid.
//!
//! # Implementation order
//!
//! 1. ~~`brew install zig`~~ — done, 0.16.0.
//! 2. ~~Clone and pin~~ — done, see the SHA above. The library has never been
//!    tagged; a floating `main` is not a dependency, it is a rolling outage.
//! 3. `build.rs`: build libghostty-vt from `GHOSTTY_SOURCE_DIR` and emit the
//!    link flags. Link the static `.a`: the `.dylib` needs an `LC_RPATH` the
//!    bundle would have to carry, and the whole point of a 2.5 MB app is not
//!    shipping a loader problem with it.
//! 4. Bind the calls above and implement the trait, translating cells into
//!    [`Mirror`](super::mirror::Mirror) exactly as the alacritty backend does.
//!    The mirror's content comparison stays: it is what keeps an idle terminal
//!    at zero command buffers regardless of how eagerly a backend reports
//!    damage.
//! 5. Enable mode 2027 at construction. Everything else is a default the user
//!    could reasonably want changed; this one is Mica choosing to measure text
//!    the way every other modern terminal measures it.
//! 6. Delete the `compile_error!` below and run the Phase 2 exit test, which
//!    asserts **both backends produce identical cells** for the same input —
//!    with the deliberate exception of grapheme width, where they now
//!    disagree on purpose. `crates/mica-shell/tests/emoji_width.rs` is the
//!    other exit criterion and flips from `#[ignore]` to passing here.
//!
//! Until step 5 passes, `--features ghostty` must fail loudly rather than
//! silently fall back — a fallback nobody notices is how a project ends up
//! shipping the thing it meant to replace.

compile_error!(
    "the `ghostty` backend is not implemented yet: libghostty-vt requires Zig 0.16.x, which is \
     not installed (a global dependency install, pending user approval). Build with \
     `--no-default-features --features alacritty` in the meantime. See the module documentation \
     in crates/mica-core/src/backend/ghostty.rs for the full API map and the implementation order."
);
