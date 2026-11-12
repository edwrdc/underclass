# underclass DMS plugin

A bar widget for [DMS / Dux Shell](https://github.com/duxlabs-dev/dux-docs) that shows the pool at a
glance and opens a `utop`-style panel on click. It only reads `GET /admin/api/monitor`, the same
snapshot `underclass top` renders, so the server needs no changes.

- **Bar pill**: `healthy/total` accounts in rotation. Amber when some are cooling or in
  `auth_error`, red when none are healthy, in which case it shows the countdown to the earliest
  reset (the moment the proxy stops returning `429`). Grey `offline` when the server is
  unreachable.
- **Popout**: attempts per minute over the last hour, month token totals, one card per account
  (status, plan, tightest quota window left, in-flight, sticky sessions, tokens this month) and the
  last few failed requests. Quota comes from the monitor's `quota_view`: every window (including
  per-model ones such as "7d Fable") with percent left and renewal countdown, banked Codex resets,
  and a stale marker. Older servers without `quota_view` fall back to `quota`/`observation`.

## Install

```sh
ln -s "$PWD/contrib/dms-plugin/underclass" ~/.config/dms/plugins/underclass
dms ipc call plugins reload underclass   # or `dms restart` for a newly added plugin
```

Then in DMS Settings > Plugins, scan, enable **Underclass**, add it to the bar, and fill in its
settings.

## Connecting

| Setup | Settings |
|-------|----------|
| Remote server (the usual case) | **Server URL** and **Admin token file** (or **Admin token**) |
| Same host as `underclass serve` | leave both empty; the widget reads the monitor socket |

The admin token is the UI token minted on first run (it gates `/admin/api/*`). Put it in a `0600`
file and point **Admin token file** at it. The widget hands curl the `Authorization` header on stdin,
so it never appears in curl's arguments. The inline **Admin token** setting is stored in DMS
settings in plain text and is passed to the fetch script as an argument, which other local users can
see in the process list; prefer the file. Use HTTPS for any non-local server.

## Limits

- Polling, not streaming: the pill is as fresh as the refresh interval (default 5 s).
- Read-only. Refresh/reset/disable actions exist in the admin API and could be added to the account
  cards later.
- Needs `curl` and a DMS with `PluginComponent` popouts (see `PLUGINS/` in the DMS install).
