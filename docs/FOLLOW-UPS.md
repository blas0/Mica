# Follow-ups: specification

Six open items, each with a decision. Three are built in this pass; three are
deferred with the reason and the cost written down, so the next session argues
with a record rather than with a memory.

The numbering is stable. Do not renumber when an item closes — strike it.

---

## FU-1 · Reload the settings file without leaving Mica

**Status: built.**

### The problem

Settings reload on `applicationDidBecomeActive:` — Mica polls the file's mtime
when it comes to the front. That covers the ordinary loop (edit in another
app, switch back) and covers nothing else. Two cases it misses:

- Mica never lost focus. The file changed underneath it — `git checkout`, a
  dotfile sync, `sed -i` from a shell running *inside* Mica itself.
- Mica has one window, focused, and the user wants to be certain the file on
  disk is the file in force. Today the only way to be sure is to switch away
  and back, which is a superstition, not an affordance.

### What was asked

> `cmd+,+r` while Mica.app is focused, to live-reload the config applying the
> settings changed

### The chord

`cmd+,+r` is two keys. Mica's `Chord` is one key plus modifiers, and that is
not an accident: a two-key sequence means a prefix mode, a pending-chord
timeout, and a rule for what happens to the keystrokes in between. That is a
real feature — tmux has it, Emacs has it — but it is a feature, not a spelling,
and nothing else in Mica needs it yet.

So the reload is a single chord, and the one that carries the meaning is the
sibling of the one that opens the file:

| Chord | Action |
|---|---|
| `⌘,` | `settings.open` — hand the file to a text editor |
| `⌘⇧,` | `settings.reload` — read the file back and apply it |

Same key, one modifier apart, because they are the two halves of one loop.
Rebindable like everything else; a user who wants `⌘⌥R` writes it in `[keys]`.

### The parse bug this exposed

`Chord::parse("cmd+,+r")` did not fail. It split on `+`, assigned `,` to the
key, then assigned `r` to the key, and returned `⌘R` — a chord the user never
asked for, bound to an action they did ask for, with no diagnostic. A settings
file that means one thing and does another is worse than one that is rejected.

`parse` now returns `None` when a second key appears, and `Settings` already
reports an unparseable chord rather than silently dropping it.

### Behaviour

`settings.reload` is a **window action**, not a surface action: it acts on
every window, not on the pane that has focus. It routes through the same
fallback path `session.new_tab` uses.

It re-reads unconditionally. `SettingsWatcher::poll` returns `None` when the
mtime is unchanged, which is right for a background poll and wrong for a key
the user deliberately pressed — "nothing happened" and "nothing needed to
happen" must not be the same outcome for an explicit command. A new
`SettingsWatcher::reread` reads the file whatever the mtime says and
resynchronises the watcher, so the automatic poll does not then re-apply the
same file a second time when the window is next activated.

A parse failure keeps the settings already in force and says so on stderr.
That is the existing contract, and the manual path must not be more destructive
than the automatic one.

### Feedback

Confirmation is the visible effect — a theme swaps with the cross-fade, a caret
style changes, motion settings take hold — plus a line on stderr naming what
applied and what is deferred to the next launch. No toast: Mica has no
transient-notice surface, and inventing one to confirm a keystroke is a larger
change than the keystroke.

### Tests

- `a_chord_with_two_keys_is_refused_rather_than_guessed` — `cmd+,+r` yields
  `None`, and `cmd+shift+p` still parses.
- `the_reload_action_is_bindable_and_bound` — the id is in `BINDABLE` and in
  `WINDOW_ACTIONS`, so the existing action-honesty test covers its dispatch.
- `rereading_returns_the_file_even_when_the_mtime_has_not_moved` — and a
  following `poll` returns `None`, i.e. reading manually does not queue a
  second apply.

---

## FU-2 · Palette, shortcut panel, settings file: what each is for

**Status: explained; nothing pruned; the boundary is now written down.**

### The question

> explain + prune if unnecessary, palette.toggle + keyboard shortcuts
> (cmd+shift+k) + settings.toml — it seems everything can be configured in
> settings.toml, what do the other features serve?

### The answer

Nothing is redundant, but the boundary was never stated, which is why three
surfaces read as three ways to do one thing. Each owns a different verb.

| Surface | Verb | What only it can do |
|---|---|---|
| `settings.toml` (`⌘,`) | **record** | Persist, diff, commit, share. The only writer. |
| Shortcut panel (`⌘⇧K`) | **capture** | Learn a chord from the *press*, and name the binding it would displace. A text file cannot know which key you hit; you would have to spell `shift+cmd+g` correctly and find the collision yourself. |
| Palette (`F5`) | **run** | Do something once. Scroll to top, jump to a block, try a theme. Not configuration. |

The panel and the palette both write through `Settings::save`. There is no
second store, so no surface can disagree with the file.

### The one real overlap

The palette carries `settings.fx.*` (caret style, decay, blink, reduce motion,
ambient light) and the `theme.*` entries. Those *are* settings-file values with
a fast switch attached — the only entries in the palette that are preferences
rather than commands.

**Kept.** Trying a caret style by cycling it and watching it is a different act
from editing a file and switching back, and the cost of keeping them is one
line each in a table that already exists. If the palette is ever to be strictly
commands-not-preferences, those five plus the themes are the exact cut list.

### What ships

The boundary above, in the module documentation of all three surfaces, each
pointing at the other two. No behaviour change.

---

## FU-3 · Bind a chord to literal text

**Status: built.**

### The problem

Every binding Mica has names an action Mica implements. There is no way to say
"when I press this, type this" — no `⌘⇧L` for `clear && ls -la`, no chord for
an ssh host reached forty times a day. Every terminal worth using has this, and
it costs nothing architecturally: the shell already accepts bytes and the
binding table already maps a chord to a string.

### The syntax

The action id *is* the payload, prefixed:

```toml
[keys]
"text:clear && ls -la\n" = "cmd+shift+l"
```

Prefix rather than a separate `[text]` table, because it keeps one map from
chord to action and one conflict check. A user who binds two chords to the same
text gets two entries, which is correct — they are two bindings.

### Escapes

TOML basic strings already give `\n`, `\t`, `\r`, `\\`, `\"` and `\uXXXX`, so
Mica adds nothing and invents nothing: the escape rules are TOML's, documented,
and already familiar. Escape is ``. A TOML *literal* string (single
quotes) passes backslashes through untouched, which is also correct — that is
what single quotes mean in TOML, and a binding that needs a literal backslash
should say so that way.

### Where it does and does not appear

`text:` bindings are a **file-only** feature. They do not appear in the
shortcut panel and cannot be created by capture: the panel edits the fixed
catalogue of actions Mica implements, and a text binding is an unbounded string
the panel has no field for. The panel's documentation says so rather than
pretending the binding does not exist.

The action-honesty test (`every_action_has_a_home`) walks `BINDABLE`; `text:`
is deliberately not in `BINDABLE`, so that test is unaffected by design and a
separate test covers the dispatch arm.

### Safety

The text goes to the PTY exactly as written, on the same path as a keystroke.
It is **not** bracketed-paste wrapped. Bracketed paste exists to draw one
distinction — "the user typed this" versus "this arrived from a clipboard" — and
a chord the user configured is the first kind. Wrapping it would also break the
escape-prefixed bindings that are half the point.

An empty payload (`"text:"`) is refused at parse time rather than binding a key
to nothing.

### Tests

- `a_text_binding_carries_its_payload`
- `an_empty_text_binding_is_refused`
- `a_text_binding_survives_the_settings_file`
- `a_text_binding_is_not_offered_in_the_shortcut_panel`
- `a_text_binding_reaches_the_shell` — end-to-end, real shell.

---

## FU-4 · Tabs drawn by Mica rather than by AppKit

**Status: deferred. Answered.**

### The question

> can tabs be not a separate window/app view - and implemented as a "tab"
> within the term emu, reference https://github.com/anomalyco/opentui to see if
> anything in there can help.

### OpenTUI does not help

OpenTUI is a framework for building applications that **run inside** a
terminal: a Zig core with TypeScript bindings, React and SolidJS reconcilers, a
keybinding and command engine, an SSH server. It draws by emitting text into
somebody else's terminal. Mica *is* the terminal. A Mica tab bar has to be
drawn by the Metal renderer in real pixels, not in terminal cells, so there is
nothing in OpenTUI to borrow for one.

Two things in it remain worth reading: its keybinding-and-command engine as a
design reference, and the fact that it is Zig — the toolchain libghostty-vt
already requires, so nothing new enters the build if something there ever does
turn out useful.

### The actual trade

In-app tabs are a real choice regardless of OpenTUI.

**Gained:** tabs that match Mica's theme, titles Mica controls, and behaviour
that does not change when the user's "prefer tabs when opening documents"
system setting changes.

**Given up:** AppKit's tab bar, its overflow menu, drag-to-reorder,
drag-a-tab-out-to-a-new-window, and the system tab shortcuts — each free today,
each a rebuild.

**Cost:** a tab-bar strip in the renderer (instances, hit-testing, the title
truncation nobody enjoys writing), a tab model above `Layout`, and a resize
path that gives the strip its rows back. The pane tree in `pane.rs` already
supplies most of what a tab needs — a tab *is* a `Layout` plus a title — so the
model is the small half.

### Why not now

The native bar is not yet wrong. It is worth building when it looks wrong
beside Mica, not on principle. Revisit when the theme work makes the mismatch
visible.

---

## FU-5 · Emoji width

**Status: deferred. Measured, not built.**

`⚠️` (U+26A0 U+FE0F) occupies one cell and should occupy two. This is not a
font problem and not a renderer problem.

Measured against libghostty-vt at `ca9e5b1`, by C probe:

| | col 0 | col 1 | col 2 |
|---|---|---|---|
| default | `U+26A0` w=0 | `X` | — |
| mode 2027 on | `U+26A0` w=1 | spacer w=2 | `X` |

Out of the box libghostty-vt gives the same wrong answer alacritty gives. The
fix is **DEC mode 2027**, grapheme clustering, under which the variation
selector widens the cluster. `🚀` is two columns either way, which is why bare
emoji always looked right and this went unnoticed.

alacritty 0.26 has no `2027` anywhere in its source, so this cannot be reached
by an escape sequence Mica sends today. It is a backend change: `GhosttyCore`,
against the trait in `backend/mod.rs`, with mode 2027 set at construction.

That is several hundred lines of FFI across selection, semantic events, side
tables, scrollback and search — not an increment that can be half-landed, and
not this pass. `crates/mica-shell/tests/emoji_width.rs` stays ignored and is
the exit criterion.

Note for whoever picks it up: `ghostty_unicode_grapheme_width()` in
`vendor/ghostty/zig-out/include/ghostty/vt/unicode.h` is pure, allocation-free,
needs no terminal instance, and reports exactly the mode-2027 answer. It is the
measuring stick to write the test against before writing the backend.

---

## FU-6 · Notarisation

**Status: blocked on credentials.**

`bundle.sh` signs ad-hoc, which is enough to run locally and not enough to
distribute. Notarisation needs a Developer ID certificate and an app-specific
password or an App Store Connect API key — the user's, entered by the user.
Nothing to build until those exist.
