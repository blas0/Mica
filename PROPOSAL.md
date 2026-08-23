# PROPOSAL.md

Changes that would need `README.md` edited. Per `CLAUDE.md`, `README.md` is not
modified directly — proposals land here and the user applies or rejects them.

---

## 1 · The Keyboard section is now short by two things

`README.md` currently says:

```
### Keyboard

Keyboard Shortcuts: `cmd+shift+k`

Settings: `cmd+,`
```

Two additions since:

- **`cmd+shift+,` — Reload Settings.** Applies `settings.toml` without leaving
  Mica. Until now the file was only re-read when Mica became the active
  application, which is unreachable if Mica never lost focus.
- **`text:` bindings.** A binding whose action id begins with `text:` types the
  rest of its own name into the shell. File-only — the shortcut panel edits a
  fixed catalogue of actions and has no field for an arbitrary string.

Suggested replacement for that section:

```
### Keyboard

Keyboard Shortcuts: `cmd+shift+k`

Settings: `cmd+,`

Reload Settings: `cmd+shift+,`

Every binding is editable in `settings.toml` under `[keys]`. A binding named
`text:…` types the rest of its name into the shell:

    [keys]
    "text:clear && ls -la\n" = "cmd+shift+l"
```

Nothing else in `README.md` is affected. Details and reasoning:
[`docs/FOLLOW-UPS.md`](docs/FOLLOW-UPS.md).
