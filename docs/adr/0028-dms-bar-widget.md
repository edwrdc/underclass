# 0028 - DMS Bar Widget

## Status

Accepted

## Context

`underclass top` (`utop`) shows pool health, quota headroom and request rate, but only while a
terminal is open. The operator runs DMS (Dux Shell) and wants the same glance permanently on the
bar, including when the server is on another host.

DMS plugins are QML components in `~/.config/dms/plugins/`. A `widget` plugin provides a bar pill
and an optional popout; the other surfaces (control center tile, desktop widget, daemon) either fit
a toggle or cost screen space, and a monitor is neither.

## Decision

- Ship a single bar widget with a popout in `contrib/dms-plugin/underclass/`, outside the Rust
  crate. It consumes the existing read-only `GET /admin/api/monitor` snapshot; no server change.
- Remote use is the primary mode: server URL plus the admin token, preferably from a file, passed
  to curl as config on stdin. The local monitor socket is a same-host fallback only, because the
  socket does not exist on a remote client.
- The widget is read-only. Account actions stay in the admin UI and CLI.
- Pill state is derived from the snapshot (healthy count, earliest cooling reset), not a new
  server field, so the contract stays the `MonitorSnapshot` shape.

## Consequences

- Changing `MonitorSnapshot`, `MonitorAccount` or the `observation` JSON can break the widget;
  `monitor.rs` and `top.rs` remain the reference consumers and the widget must follow them.
- The widget is not covered by `cargo test`; it is verified by loading it in DMS.
- The admin token lives on the operator's desktop. A token file keeps it out of DMS settings and
  process arguments; the inline setting is a convenience with weaker protection.
