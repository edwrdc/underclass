# 0029 - Normalized Quota View

## Status

Accepted

## Context

The admin dashboard showed Claude utilization as percent used in one column and Codex usage as
percent left in another, so the same question ("how much is left?") had two answers in two units.
Per-model weekly windows such as
Fable were dropped because only a fixed set of keys was parsed.

## Decision

- Add a pure `quota` module that normalizes both backends into a list of windows (key, label, used
  fraction, reset time, window length) plus a banked-reset count and credit expirations. The state
  API exposes it as `quota` on each account; Copilot has none.
- Claude windows are parsed generically from any payload key carrying `utilization`, so new
  model-scoped windows appear without code changes. Real payloads also carry per-model weekly
  limits (e.g. Fable) only in a `limits` array of `{group, kind, percent, resets_at, scope}`
  entries; every entry other than `session` and `weekly_all` (which duplicate `five_hour` and
  `seven_day`) becomes one window keyed `limit:<kind>:<scope>` and labelled like "7d Fable",
  replacing a top-level window with the same label. The top-level `extra_usage` (spend credits) is
  not a window.
- Model-scoped windows are display-only. Only the account-wide 5h and 7d windows cool an account.
- Claude banked resets are not available via OAuth: `/api/oauth/rate-limit-reset-credits` (and
  `/consume`) return 404 upstream for every account, and `/api/oauth/usage` carries no reset-credit
  field. They are visible only on claude.ai, so the dashboard shows "—" with an explanatory tooltip
  for Claude accounts, never polls that endpoint, and Claude reset redemption is removed.
  `POST /admin/api/accounts/:id/reset` for a Claude account returns 422 `unsupported`.
- Disabled accounts are polled slowly, for display only.
- `GET /admin/api/accounts/:id/usage/raw` returns the raw upstream payload for diagnosis.
- The dashboard shows two columns, "quota left" (percent remaining per window, with renewal time)
  and "banked resets", and falls back to the old `usage`/`observation` fields when `quota` is absent.

## Consequences

- One unit (percent left) and one layout across backends; per-model limits are visible.
- Generic parsing may surface windows with unfamiliar labels; they are display-only until
  deliberately promoted to cooling.
- The `quota` JSON shape is a new contract for the dashboard; the raw endpoint exposes upstream
  payloads to the admin token holder, so it must stay behind admin auth.
- `GET /admin/api/monitor` exposes the same view per account as `quota_view` (built by the shared
  `ui::quota_view` helper), next to the legacy `quota` and `observation` fields. The DMS widget and
  `utop` render it; per-model (`limit:`) windows are shown but never drive severity colors.
