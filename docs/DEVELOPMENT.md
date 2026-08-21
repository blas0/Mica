# Mica

A GPU-rendered terminal for macOS. Rust, Metal 3, four crates.

Built by following [`../BUILD-ORDER.md`](../../BUILD-ORDER.md), which synthesises
[`../POC-spec.md`](../../POC-spec.md) (Metalterm's public product surface) and
[`../POC-spec-2.md`](../../POC-spec-2.md) (what its compiled binary actually
contains) into an executable sequence.

Mica is an **independent implementation informed by that architecture**. No
assets, icons, theme definitions, embedded scripts, or branding were copied, and
Mica is not affiliated with Metalterm.

---

## Build and run

```sh
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"

cargo test --workspace     # 331 tests
./bundle.sh release        # → target/release/Mica.app
open target/release/Mica.app
```

Xcode's Metal toolchain is required: `build.rs` compiles `shaders/mica.metal`
into `default.metallib` at build time rather than at launch.

---

## Layout

Dependencies point strictly inward. Each arrow is enforced by what the crate is
allowed to import, and the boundary is the single most valuable thing recovered
from the reference binary.

```
mica-shell   AppKit — the only crate that knows a window exists
    ↓
mica-gpu     Metal — nine pipelines, damage-driven frame scheduling
    ↓
mica-atlas   CoreText — rasterisation, shelf packing, the resident atlas
    ↓
mica-core    terminal state, PTY, OSC 133, settings, themes
```

`mica-core` never imports Metal or AppKit; `mica-atlas` never imports Metal;
`mica-gpu` never imports AppKit. That is why three of the four crates are fully
testable with no window server, and why the pixel tests can read a rendered
texture back and assert on it instead of comparing screenshots.

`crates/mica-core/tests/layering.rs` enforces it, along with two other rules
that were previously only comments: `fork`/`execve` appear in `pty.rs` and
nowhere else, and `TerminalCore` has not grown a `full_grid()`. All three were
checked by whoever was reviewing, which is the kind of rule that holds for six
weeks and then quietly stops.

---

## The five decisions worth knowing

**1. Zero frames when idle is a scheduling property, not a rendering one.**
`mica-gpu/src/frame.rs` contains no Metal at all. Every reason to draw is an
explicit `Reason`; if none arrives, `poll` returns `Idle` forever and no command
buffer is submitted. There is no `CVDisplayLink` and no timer anywhere in the
codebase — the PTY reader thread fires a wakeup that hops to the main queue, so
output arriving is what causes work.

Verified end to end: a real shell on a real Metal device, polled 1200 times as a
120 Hz display would, submits **zero** additional command buffers. The running
app measures 0.0% CPU while idle.

The corollary is that the frames it *does* draw must not block. The window
renders through `Renderer::render_to_drawable`, which schedules the present on
the command buffer and returns; `render_to_texture` is its offscreen twin, ends
in `waitUntilCompleted`, and is for tests that read pixels back. Rendering a
window through the offscreen path costs 36.9 ms per frame in `nextDrawable`
alone — the drawable is held for the whole GPU execution — against 16.5 ms, one
vsync, for the live path. Nothing in the type system tells the two apart, so
`tests/live_render_path.rs` does.

**2. There is no `full_grid()` on the `TerminalCore` trait, and there must never
be one.** The moment it exists, something calls it every frame and the property
above dies quietly. A renderer that needs everything calls `damage_all()` — an
explicit, greppable call site.

**3. Backends are swappable, because libghostty-vt is not yet safe to depend on
directly.** It is the right layer — its `render_state` dirty-row API exists
precisely so callers bring their own renderer — but it has never been tagged, its
header warns that breaking changes are expected, and it makes Zig 0.16.x a hard
build dependency. `TerminalCore` costs about a day and makes all of that
survivable. `crates/mica-core/tests/backend_contract.rs` is written against the
trait, so it runs unchanged under either backend.

**4. A cell is 20 bytes and stays 20 bytes.** Asserted at compile time. Grapheme
clusters, per-cell underline colour, and hyperlinks live in side tables that
allocate nothing until something uses them — a screenful of build log allocates
zero bytes across all three, which is a test.

**5. A theme is exactly eight colour roles.** The 256-colour palette is derived
from them, so ANSI red *is* the theme's error colour and a build failure looks
the same whether `cargo` coloured it or the block gutter did. The 6×6×6 colour
cube is deliberately left untouched, because applications compute those indices
arithmetically.

---

## Motion

Seven caret motion styles, sub-cell interpolation, a decay trail, a blink that
fades rather than strobes, and theme cross-fades. All of it is `mica-core::motion`,
which is a pure function of `dt` — no clock, no thread, no `Instant::now`. The
window layer measures the interval between frames and hands it in, which is why
`cargo test -p mica-core motion` covers all seven styles deterministically in
under a millisecond and with no GPU.

| Style | What it does |
|---|---|
| `snap` | No interpolation. The caret is simply in the new cell. |
| `ease` | Exponential approach. The default. |
| `spring` | Damped spring; overshoots slightly and comes back. |
| `smear` | Ease, softening along the direction of travel. |
| `squash` | Ease, stretching along travel and thinning across it, conserving area. |
| `phosphor` | Ease with a long, slow trail. |
| `arc` | Travels along an arc rather than a straight line. |

```toml
[motion]
cursor = "phosphor"   # one of the seven above
speed = 1.5           # 0.25..4.0, multiplies every rate
intensity = 0.8       # 0..1, how strongly the style deforms
decay = true          # the trail
blink = false         # see below
reduce = false        # collapse everything to a 90 ms fade
```

Three properties are enforced by tests rather than by intent:

- **Every style settles, and settles exactly on target.** A style that
  asymptotically approaches its cell is an animation that never ends, and an
  animation that never ends is a terminal that never stops drawing. Each style
  is stepped at a fixed `dt` until it reports itself finished, and the position
  is then asserted to be the target *exactly*.
- **Easing is frame-rate independent.** Interpolating by a fixed fraction per
  frame makes the caret twice as fast at 120 Hz as at 60 Hz. The same elapsed
  time reaches the same position at either step size, and that is a test.
- **A long stall does not fling the caret across the screen.** A breakpoint or a
  closed lid delivers a `dt` of seconds; an explicit integrator handed a step
  that large diverges rather than converging. Steps are clamped, so a stall
  makes the caret arrive late instead of arriving at infinity.

**Blinking is off by default, and that is a design position rather than an
oversight.** A blink is a change on a schedule, and a change on a schedule is
frames — roughly one per display refresh, forever, for decoration. It is the one
thing in `motion.rs` that never settles, and the test asserting so exists to
stop someone quietly optimising it into a caret that has stopped blinking. Turn
it on with `blink = true` or `⌘⇧P → Toggle Caret Blink`, knowing what it costs.

Reduce Motion collapses all of it — travel, deformation, trail, and the theme
cross-fade — to a single 90 ms fade. The app setting and the macOS system
preference are OR'd: someone who has asked the whole machine for less motion has
not asked this one app for an exception. The system preference is read when a
window opens rather than observed live.

---

## Status against BUILD-ORDER.md

| Phase | State |
|---|---|
| 0 Toolchain | ✅ except Zig — see below |
| 1 Workspace | ✅ |
| 2 `TerminalCore` + backends | ✅ alacritty; ghostty gated on Zig |
| 3 PTY and shell spawn | ✅ |
| 4 Glyph atlas | ✅ |
| 5 Metal renderer | ✅ nine pipelines, pixel-verified |
| 6 AppKit shell | ✅ window, keys, menu, native tabbing |
| 7 Shell integration | ✅ zsh, bash, fish |
| 8 terminfo | ✅ `tic -x` + `infocmp -x` round-trip |
| 9 Config, themes, palette, find | ✅ |
| 10 Packaging | ◐ bundle and hardened runtime; notarisation not attempted |

### The two things that are not done

**libghostty-vt is not wired up.** It needs Zig 0.16.x, which is a global
dependency install and therefore needs explicit approval. Until then
`crates/mica-core/src/backend/ghostty.rs` is a documented gate carrying the API
map recovered from the upstream headers, and it fails the build loudly rather
than falling back silently — a fallback nobody notices is how a project ships the
thing it meant to replace. The alacritty backend carries the build.

**Notarisation is not attempted.** `bundle.sh` signs ad-hoc with the hardened
runtime enabled, which is enough to run locally. Signing with a Developer ID and
notarising touches credentials and needs approval first. Note that `spctl` on
this machine reports `accepted` only because Gatekeeper assessment is disabled
there — that is not a passing Phase 10 exit test.

Also deliberately out of scope for v0.1, per the plan: splits, the full theme
set, caret motion styles, block folding, OSC 9/777 notifications, Sparkle
updates, the settings UI, and the content-aware renderers. The palette lists
`settings.fx.*` actions that are not implemented; `dispatch` returns `false` for
them and says so on stderr rather than appearing to work.

---

## Keys

| | |
|---|---|
| `⌘⇧P` | command palette |
| `⌘F` | find in scrollback |
| `⌘G` / `⌘⇧G` | next / previous match |
| `⌘N` | new window (a tab, via native tabbing) |
| `⌘C` / `⌘V` | copy selection / paste, bracketed |
| `⌘,` | keyboard shortcuts |
| `⌘]` / `⌘[` | next / previous command block |
| `⌥⌫` | delete the word to the left |
| `⌘⌫` | delete to the start of the line |
| `⎋` | close an overlay |

`⌘⇧P` also carries `Caret Motion · Next Style`, `Toggle Caret Decay`,
`Toggle Caret Blink`, and `Toggle Reduce Motion`.

Every shortcut above except copy, paste and the deletion keys is rebindable in
`⌘,` — arrows to move, space to capture a combination, `⌫` to unbind, `⎋` to
close. Changes land in `[keys]` in `settings.toml`, only where they differ from
the defaults. There is exactly one table: the window dispatches through it, the
palette prints its accelerator column from it, and the panel edits it. Before
that they were separate, and had already drifted — the palette advertised `⌘↓`
for two different actions and `⌘↑` for one that had no binding at all.

`⌘⌫` sends `^U`. Worth knowing what that means, because the two common shells
disagree and it was checked rather than assumed: **bash** binds it to
`unix-line-discard`, which deletes to the start of the line — the macOS
behaviour. **zsh** binds it to `kill-whole-line`, which takes the whole line
including anything right of the cursor. The difference only shows mid-line, and
at a prompt the cursor is usually at the end. There is no escape sequence that
means "delete to line start" in general, so the choice was `^U` or nothing.

Search is literal and smart-case: a lowercase query matches case-insensitively,
a query containing an uppercase letter matches exactly. Regex is the obvious
second step and `search.rs`'s `Matcher` enum is where it goes.

---

## Licensing

Ghostty and libghostty are MIT (© 2024 Mitchell Hashimoto and contributors);
linking them in is fine, and the notice travels with them if and when the
backend lands.

Metalterm is closed-source. Everything in `POC-spec-2.md` came from static
inspection of a freely distributed binary and from public strings. The terminfo
capability *set* is a description of standard escape sequences and is fine to
reimplement; the app's name, icon, and theme names are not, and none of them
appear here.
