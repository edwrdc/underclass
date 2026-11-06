# 0027 - Failover on upstream stream errors before output

## Status

Accepted. Extends ADR 0022. Capacity failures keep their existing handling.

## Context

On 2026-11-01 at 14:45:45Z a client received Cloudflare `502 origin_bad_gateway`.
Underclass logged `request.completed status=200`, followed by
`stream.finished outcome=upstream_error, error_code=none, saw_output=false`.
Caddy logged `EOF` after 2.8 seconds for the matching Cloudflare ray.
The container had not restarted and was not out of memory.

ADR 0022 retries capacity failures during the pre-output hold. A body transport
error without a capacity code instead fell through to a `200` response backed
by a broken body. The resulting connection abort caused a gateway error.

## Decision

- If a streamed 2xx body errors while the pre-output hold is still undecided,
  discard its buffered bytes and try another account. No response bytes have
  reached the client. An undecided hold has seen neither output nor a terminal
  event nor a backend-specific acceptance event.
- Exclude that account for this request using the existing `select_excluding`
  path. Do not change account health. Preserve the existing attempt limit and
  persist sticky rebindings as for capacity failover.
- Record the discarded attempt as an `upstream_error` usage row with its original
  upstream HTTP status. Do not invent an upstream error code or event.
- Retain a complete JSON `502 upstream_stream_error` as this attempt's fallback,
  rather than an aborted 2xx body. If no account succeeds, return the last
  retained fallback, which may instead be a later upstream refusal.
- Keep the hold's existing time and byte limits. Do not retry after the hold
  commits a response. In particular, failures after output still pass through.

## Consequences

- An early upstream transport failure can recover through another account,
  at the cost of another upstream request and extra latency.
- If every attempt has an early transport failure, clients receive a complete
  application error instead of an aborted response and a proxy-generated 502.
- This does not prevent upstream connection failures. Clean EOF without a
  terminal event and broken non-streamed bodies keep their existing behavior.
