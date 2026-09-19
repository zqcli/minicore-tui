# Changelog

## Unreleased — v0.3 E1 tool details

- Add a read-only tool detail in the main area, opened through a separate
  `[详情]` card-title target or an exact four-part `/tool` identity.
- Preserve tool-card folding, session drafts, the Rail Editor, one-line Footer,
  and the conversation scroll position. F6 switches main/editor focus; leaving
  the detail never cancels execution.
- Page bounded stdout/stderr/result/input streams using authoritative byte
  cursors, with explicit pending/partial/expired/gap/EOF states, safe control
  rendering, follow-tail, copy and explicit retry.
- Keep execution outcome, nonzero command exit, termination confirmation,
  output completion and auxiliary recording distinct. No full PTY or synthetic
  diff is introduced. Workspace/changes/context pages remain later work.