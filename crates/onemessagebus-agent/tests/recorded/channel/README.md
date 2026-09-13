# Recorded channel directories

Whole planner-channel directories as `onepipeline` 0.28.2's channel code wrote
them, held by `tests/channel.rs` to reading, re-applying and writing through the
`planner-channel` layout of this crate. Every file here is byte for byte what
was on disk; nothing was edited or redacted, and each file was checked against
the core `Redactor`'s credential-prefix rule, which would change none of them.

`onepipeline`'s `src/channel.rs` is byte-identical from v0.28.0 through v0.29.1
(its last change, `bbee552`, first shipped in v0.28.0), so every directory below
is exactly what the 0.28.2 release writes.

Only the seven files of the channel layout are copied: `surfaces.jsonl`,
`queue.json`, `replies.jsonl`, `replies-cursor.json`, `commands.jsonl`,
`commands-cursor.json` and `command-outcomes.jsonl`, where the run has them. Each
run's channel directory also held an empty `handover/` directory, which is not
part of the channel layout and is left out.

## `domain-driven-modularity-2/` — recorded

The complete channel of the finished run `domain-driven-modularity-2` on this
host (`/home/nick/projects/ai-orchestrator/runs/domain-driven-modularity-2`),
started at 2026-09-12T19:31:24Z and last written at 2026-09-12T22:10 local time,
between the 0.28.2 and 0.28.3 releases. All seven files.

- `surfaces.jsonl`: 49 records: 17 queued, 16 claimed, 3 answered, 10 abandoned
  and 3 attended.
- **A superseded check-in**: check-in `0` was still waiting when check-in `1`
  was queued, and was replaced by it without ever being claimed.
- **Surfaces answered by replies**: surfaces `4`, `6` and `11` were each claimed
  into the pending slot and released by the reply that answered them.
- **Surfaces abandoned by one serving session and attended by a later one of
  the same asker**: surfaces `4`, `10` and `13` were each abandoned when their
  `channel serve` session ended, and taken back by the next session of the same
  asker, whose name is the dispatch's scratch directory. Surfaces `5`, `6`,
  `11` and `14` were abandoned by the same sessions and never attended.
- `queue.json`: nothing waiting, nothing pending, `next_id` 17, stamped at
  105248 bytes and sealed.
- **A reply cursor short of the log's end**: `replies.jsonl` holds 9 replies
  (ids 0–8), and `replies-cursor.json` holds `7`, so replies 7 and 8 were never
  claimed. Replies 0–7 carry commands beside their verdict; reply 8 is
  verdict-only.
- **Commands-only replies with their recorded outcomes**: `commands.jsonl` holds
  7 envelopes (ids 0–6) and `commands-cursor.json` holds `7`. Envelopes `1` and
  `5` (the monitor's `finding`s) and `2`, `3` and `6` (the planner's `note`s)
  are commands-only: no reply on `replies.jsonl` carries their commands, as
  0.28.2 routes an envelope with no verdict to the command queue alone.
  `command-outcomes.jsonl` answers all 7, each `applied: true` with one
  per-command result.

## `onemessagebus-repair-2/` — recorded

The complete channel of the finished run `onemessagebus-repair-2` on this host
(`/home/nick/projects/ai-orchestrator/runs/onemessagebus-repair-2`), written on
2026-09-12 between 20:02 and 20:43 local time. It has no command queue, so four
files: `surfaces.jsonl` (3 surfaces, each queued and claimed, surface `1`
answered), `queue.json` (nothing waiting or pending, `next_id` 3),
`replies.jsonl` (one verdict-only reply) and `replies-cursor.json` (`1`). It is
the channel `onemessagebus-repair-2-pending/` was produced from, and the channel
of the run root in `../run-root/onemessagebus-repair-2/`.

## `onemessagebus-repair-2-pending/` — produced with the 0.28.2 binary

**A blocking surface claimed and still pending** is a state no finished run on
this host holds: every 0.28-format channel here ended with its pending slot
empty or holding an abandoned surface. So it was produced with the 0.28.2
release itself — the `onepipeline` binary of the `onepipeline-cli` 0.28.2 wheel,
run with `uv tool run --from onepipeline-cli==0.28.2` — over a scratch copy of the
recorded `onemessagebus-repair-2` run root and channel:

1. `channel serve onemessagebus-repair-2`, with `ONEPIPELINE_CHANNEL_ASKER=fixture-listener`,
   `ONEPIPELINE_REPLY_TIMEOUT_SECONDS=1` and `ONEPIPELINE_SERVE_SESSION_SECONDS=3`,
   fed one blocking frame (`{"kind":"planner-question","message":"Recorded for
   the onemessagebus channel fixtures: a blocking question raised through
   channel serve, left pending by next.","blocking":true}`) and holding its
   stream open past the session bound. It queued surface `3` under the asker,
   timed out waiting for a reply, and ended on its session bound — the ending
   that marks nothing abandoned.
2. `next onemessagebus-repair-2`, which claimed surface `3` into the pending slot.

The four files are the copy after those two commands, unedited: `surfaces.jsonl`
is the recorded one with `queued` and `claimed` records of surface `3` appended,
and `queue.json` holds surface `3` pending, not abandoned, with `next_id` 4.

# Recorded run root

## `../run-root/onemessagebus-repair-2/`

The run root of the finished run `onemessagebus-repair-2` (above): its
`launch.json`, `plan.json`, `checkpoint.json`, `events.jsonl`, `summary.json` and
`result.json`, copied from this host unedited. The 0.28.2 binary's `next`,
`results` and `status` open a run root rather than a bare channel directory, so
the journey that has that binary read a directory this crate wrote copies this
run root to a scratch runs directory and substitutes the written directory for
its `channel/`.
