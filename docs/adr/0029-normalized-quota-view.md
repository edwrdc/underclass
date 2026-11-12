# 0029 - Normalized Quota View

## Status

Accepted

## Context

The admin dashboard showed Claude utilization as percent used in one column and Codex usage as
percent left in another, so the same question ("how much is left?") had two answers in two units.
Claude banked resets were read only from the usage payload, and per-model weekly windows such as
Fable were dropped because only a fixed set of keys was parsed.

## Decision

- Add a pure `quota` module that normalizes both backends into a list of windows (key, label, used
  fraction, reset time, window length) plus a banked-reset count and credit expirations. The state
  API exposes it as `quota` on each account; Copilot has none.
- Claude windows are parsed generically from any payload key carrying `utilization`, so new
  model-scoped windows appear without code changes.
- Model-scoped windows are display-only. Only the account-wide 5h and 7d windows cool an account.
- The Claude banked-reset count comes from `/api/oauth/rate-limit-reset-credits`.
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
