# Mica

A GPU-rendered terminal for macOS. Rust, Metal 3, four crates.

Built by following [`../BUILD-ORDER.md`](../BUILD-ORDER.md), which synthesises
[`../POC-spec.md`](../POC-spec.md) (Metalterm's public product surface) and
[`../POC-spec-2.md`](../POC-spec-2.md) (what its compiled binary actually
contains) into an executable sequence.

Mica is an **independent implementation informed by that architecture**. No
assets, icons, theme definitions, embedded scripts, or branding were copied, and
Mica is not affiliated with Metalterm.

---

## Build and run

```sh
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"

cargo test --workspace     # 267 tests
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
| 9 Config, themes, palette | ◐ config and themes done; palette and find are not |
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
updates, the settings UI, and the content-aware renderers.

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
