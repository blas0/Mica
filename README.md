# Mica
Terminal emulator focused on smooth motion and lightweight rendering.

![Static Badge](https://img.shields.io/badge/Apple%20Frameworks-black?logo=apple)
![Static Badge](https://img.shields.io/badge/Rust-orange?logo=rust)

### Mission

- Lightweight terminal emulator
- Smooth and fluid motions, and or animations
- Extensible via open source code
- Diverging from biased appendages

> [!NOTE]
> **Projected release**: 8/20/26

### Primary Features

> `alpha-0.1`
1. Smooth kareting
2. Smooth cursor blink
3. Config is a file, keyboard shortcuts are a TUI

### Stack

**Crates**:

`mica-core`
> terminal model, TerminalCore, PTY, motion physics, settings.

`mica-atlas`
> CoreText glyph rasterization into packed pages

`mica-gpu`
> Metal renderer, instance buffers, overlays (keyboard shortcuts)

`mica-shell`
> AppKit layer: NSView/CAMetalLayer, key decoding, bindings, scroll, wired by Surface

**Apple Frameworks**:

`AppKit`
> window, view, event loop, menus

`Metal 3`
> nine vertex/fragment pipeline pairs, instanced unit quads

`Core Animation`
> CAMetalLayer for drawable vending and present

`Core Text`
> CTFontDrawGlyphs for greyscale masks, CTLine for colour emoji and clusters

`Core Graphics` 
> the bitmap contexts those rasterise into

`libc / POSIX`
> forkpty, execve, the reader thread

**Config**:

`toml` 
> settings via `CMD + ,`

