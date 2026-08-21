# CLAUDE.md

## SOP

Standard operation procedures while working within, or on `Mica`

**DO NOT**

- modify `README.md`, if diffs make modifications to `README.md` necessary, surface via `PROPOSAL.md`

**Strive for**:

1. .1ms < GPU time per frame
2. 0 dependency install/dist
3. 5ms GPU budget
4. Zero frame drop, serving 120fps for compatible displays
5. 2.5mb app size
6. 100ms < first exec
7. 75mb < idle mem usage
8. 75mb < Memory after 15k lines
9. 15mb < Memory after 15k lines
10. PTY in → 10ms (15k lines, weighted)
