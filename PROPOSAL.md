# PROPOSAL.md

Changes that would need `README.md` edited. Per `CLAUDE.md`, `README.md` is not
modified directly — proposals land here and the user applies or rejects them.

---

## 1 · Two lines in `README.md` are now false

### The Features list

```
3. Config is a file, keyboard shortcuts are a TUI
```

The keyboard-shortcut TUI is gone, along with the command palette. Config is a
file and only a file. Suggested:

```
3. Config is a file — one file, and nothing else writes it
```

### The Keyboard section

```
### Keyboard

Keyboard Shortcuts: `cmd+shift+k`

Settings: `cmd+,`
```

`cmd+shift+k` no longer does anything, and two things are missing. Suggested
replacement:

```
### Keyboard

Settings: `cmd+,` — opens `settings.toml` in your editor

Reload Settings: `cmd+shift+,` — applies the file without leaving Mica

Every binding is listed in `settings.toml` under `[keys]`, with a commented
catalogue of every action above it. A binding named `text:…` types the rest of
its name into the shell:

    [keys]
    "text:clear && ls -la\n" = "cmd+shift+l"
```

Nothing else in `README.md` is affected. Reasoning for all three changes:
[`docs/FOLLOW-UPS.md`](docs/FOLLOW-UPS.md), FU-1 through FU-3.
