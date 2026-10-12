# Changelog — Selectable

## Unreleased

### Behavior change: external fallbacks now ask first (privacy)
- Engine-failure and unsupported-pair fallbacks no longer open the browser
  on their own. Failures show a consent dialog (destination host disclosed,
  with a "don't ask again" checkbox persisted to `config.toml`);
  unsupported pairs only toast. Gate: `[translate] external_fallback`
  (default `false`). Explicit search clicks and `source = "external"` are
  unaffected (clicking IS consent).
- `translate_url` default template uses `{tgt}` (BCP47, e.g. `zh-CN`).

### Added
- `[translate] source = bergamot|online|external` switch (online falls back
  to local with a notice until wired); `external` mode opens the translator
  URL directly (partial = selection, full = everything).
- `[search] engine` presets (bing/google/ddg/baidu/wikipedia/qwant) sourced
  from the local Firefox search-config registry; explicit `engine_url` wins.
- Right-click menu: select-all, translate-selected-text, translate-all-text;
  staged Esc (cancel translation → clear selection → close).
