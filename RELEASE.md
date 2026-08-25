# Release status

Mica is currently an alpha source release. The repository is public, but a
downloadable macOS application is not yet a supported public release.

## Known limitations

- The default terminal backend is Alacritty. The Ghostty backend is documented
  but not implemented yet.
- Emoji presentation sequences such as `⚠️` still have the Alacritty width
  behaviour. The exit test remains ignored until the Ghostty backend enables
  grapheme mode 2027.
- Metal renderer tests require a working Metal device. A successful compile or
  CPU-only test run does not replace a real-device smoke test.
- `bundle.sh` creates an ad-hoc signed app for local use. Public distribution
  still requires a Developer ID signature and notarisation.

## Public app release gate

Before publishing a downloadable app, all of these must be true:

1. Run `cargo test --workspace` on a supported Mac with a working Metal device.
2. Run the manual terminal smoke flow: launch, shell startup, `cmd+a`, drag
   selection, copy-on-select, `cmd+c`, `cmd+v`, a mouse-aware CLI, panes,
   tabs, resize, and clean exit.
3. Build with `MICA_SIGNING_IDENTITY` set to the approved Developer ID and the
   distribution entitlements selected by `bundle.sh`.
4. Notarise and staple the app, then verify it with Gatekeeper assessment
   enabled on a clean machine.
5. Record the shipped version in `CHANGELOG.md` and publish a signed release
   artifact rather than asking users to build from source.

The development entitlements in `resources/Mica.entitlements` are intentionally
not part of that path.
