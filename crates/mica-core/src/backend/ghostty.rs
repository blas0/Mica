//! [`TerminalCore`](crate::backend::TerminalCore) over **libghostty-vt** — the
//! primary backend, currently gated.
//!
//! # Why this file does not compile yet
//!
//! libghostty-vt is built with Zig 0.16.x, which is **not installed on this
//! machine**, and installing it is a global dependency install that requires
//! explicit approval. Rather than ship several hundred lines of FFI that has
//! never been compiled — code that looks finished and is not — this module is
//! a gate plus the API map recovered from the upstream headers, so the work is
//! specified and can be picked up the moment the toolchain lands.
//!
//! The [`alacritty`](super::alacritty) backend carries the build until then.
//! That is exactly what the trait boundary was bought for.
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
//! # Implementation order when Zig lands
//!
//! 1. `brew install zig` (0.16.x — verify, do not assume). **Needs approval.**
//! 2. `git clone https://github.com/ghostty-org/ghostty vendor/ghostty`, check
//!    out a specific SHA, and **record that SHA in the repository**. The
//!    library has never been tagged; a floating `main` is not a dependency, it
//!    is a rolling outage.
//! 3. `build.rs`: build libghostty-vt from `GHOSTTY_SOURCE_DIR` and emit the
//!    link flags.
//! 4. Bind the calls above and implement the trait, translating cells into
//!    [`Mirror`](super::mirror::Mirror) exactly as the alacritty backend does.
//!    The mirror's content comparison stays: it is what keeps an idle terminal
//!    at zero command buffers regardless of how eagerly a backend reports
//!    damage.
//! 5. Delete the `compile_error!` below and run the Phase 2 exit test, which
//!    asserts **both backends produce identical cells** for the same input.
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
