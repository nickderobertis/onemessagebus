#!/usr/bin/env python3
"""Render the animated GIF of a `subscribe` tail — the README hero.

Like `screenshots/capture.sh` this drives the **real release `onemessagebus`
binary** over the e2e tier's own `desk` layout on a temporary local transport, so
the records, positions and rendering are genuine CLI output: no model, no
network, no credential.

`subscribe` is a true tail — it flushes every record already on the queue one at
a time, then polls once a second and appends new ones as other processes write
them — and nothing redraws, which is what makes this simpler than a live view
that repaints. So rather than screen-recording a PTY (which would need ttyd and
ffmpeg, and would not be reproducible anyway), this spawns the real `subscribe`,
**reads its stdout line by line as it arrives with the instant each line landed**,
drives a sibling `send` process alongside it, and then replays those lines at
their real inter-arrival gaps. What you watch is the backlog landing in a burst
and then records arriving one at a time as they are sent.

The GIF is informational and, unlike the SVG stills, is **not** hash-gated — a
GIF is not byte-reproducible across rendering libraries — so it is regenerated on
demand with `just screenshots-gif` and committed. Regenerate it when
`subscribe`'s rendering changes.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# GitHub-dark, matching the SVG stills' window (background #0d1117).
BG = (13, 17, 23)
BAR = (22, 27, 34)
FG = (201, 209, 217)
DIM = (139, 148, 158)
CYAN = (57, 197, 207)
DOTS = [(255, 95, 86), (255, 189, 46), (39, 201, 63)]  # traffic-light window dots

COLS = 112
FONT_SIZE = 17
PAD = 22
BAR_H = 36
MIN_MS = 220        # the backlog flush is faster than a frame; floor it so it reads
MAX_MS = 1300       # a poll gap longer than this is dead air, not information
HOLD_MS = 2800      # hold on the finished tail

# The queue the tail watches, and what a sibling process appends to it while it
# runs. The first three are the backlog `subscribe` flushes on startup; the rest
# arrive one at a time, and the `complete` record is the one `--until` admits.
BACKLOG = [
    ("finding", "the base moved under the change", "proposal", True),
    ("note", "nightly sweep is green", "sweep", False),
    ("question", "which base do I fork from?", "proposal", True),
]
ARRIVING = [
    ("note", "worker-2 claimed the retry", "desk", False),
    ("finding", "the schema bundle pin is stale", "checkin", True),
    ("note", "worker-2 settled", "desk", False),
    ("complete", "the desk is clear", "lead", False),
]
GAP_SECONDS = 0.9   # between sibling sends, so the tail's poll shows them arriving


def record(kind: str, message: str, source: str, blocking: bool) -> str:
    return (
        '{"kind":"%s","message":"%s","source":"%s","blocking":%s}'
        % (kind, message, source, "true" if blocking else "false")
    )


def stage(root: Path, repo: Path) -> Path:
    """Write the configuration the scenes share: a local transport under `root`,
    the bus's own `desk` layout bundle linked, exactly as `desk_config` stages it
    for the journeys."""
    desk = repo / "crates/onemessagebus-e2e/tests/layouts/desk.json"
    config = root / "bus.yaml"
    config.write_text(
        "version: 1\n"
        f'transport: {{kind: local, dir: "{root / "bus"}"}}\n'
        "profile: desk\n"
        "schemas:\n"
        f'  - "{desk}@1"\n'
    )
    return config


def tail(bus: str, root: Path, config: Path) -> list[tuple[float, str]]:
    """Run the real `subscribe` while a sibling sends, and hand back each line it
    printed with the instant it landed."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("ONEMESSAGEBUS_")}
    for spec in BACKLOG:
        subprocess.run(
            [bus, "send", "questions", "--config", str(config)],
            input=record(*spec), cwd=root, env=env, text=True,
            capture_output=True, check=True,
        )

    child = subprocess.Popen(
        [bus, "subscribe", "questions", "--until", '{"field":"kind","equals":"complete"}',
         "--timeout", "60", "--format", "text", "--config", str(config)],
        cwd=root, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )

    started = time.monotonic()
    lines: list[tuple[float, str]] = []

    def read() -> None:
        assert child.stdout is not None
        for line in child.stdout:
            lines.append((time.monotonic() - started, line.rstrip("\n")))

    reader = threading.Thread(target=read, daemon=True)
    reader.start()

    for spec in ARRIVING:
        time.sleep(GAP_SECONDS)
        subprocess.run(
            [bus, "send", "questions", "--config", str(config)],
            input=record(*spec), cwd=root, env=env, text=True,
            capture_output=True, check=True,
        )

    if child.wait(timeout=60) != 0:
        raise SystemExit("subscribe-gif: the tail did not end on its predicate")
    reader.join(timeout=5)
    if not lines:
        raise SystemExit("subscribe-gif: the tail printed nothing to animate")
    return lines


# The per-run values the desk stamps, rewritten to the same fixed placeholders the
# hash-gated stills use, so the two read as one session.
def normalize(text: str) -> str:
    text = re.sub(r"c-[0-9a-f]{32}", "c-4f3c1d92a08b47e6b1d5c0a7e93f2b18", text)
    return re.sub(r'"raised_at":\d{13}', '"raised_at":1789300000000', text)


def wrap(line: str) -> list[str]:
    """Fold an over-wide line at the window's column budget, as freeze does for
    the stills."""
    return [line[i:i + COLS] for i in range(0, len(line), COLS)] or [""]


def frames(command: list[str], lines: list[tuple[float, str]]) -> list[tuple[list, int]]:
    """One frame per arriving line, held for the gap until the next one really
    arrived."""
    prompt = [[("$ ", CYAN), (command[0], FG)]]
    prompt += [[("    " + part, FG)] for part in command[1:]]

    out: list[tuple[list, int]] = [(list(prompt), 700)]
    shown: list = list(prompt)
    for index, (at, line) in enumerate(lines):
        for part in wrap(normalize(line)):
            position, _, rest = part.partition(" ")
            shown = shown + [[(position + " ", DIM), (rest, FG)] if rest else [(part, FG)]]
        nxt = lines[index + 1][0] if index + 1 < len(lines) else None
        gap = HOLD_MS if nxt is None else int(max(MIN_MS, min(MAX_MS, (nxt - at) * 1000)))
        out.append((list(shown), gap))
    return out


def render(frames_: list[tuple[list, int]], font_path: Path, out: Path) -> None:
    font = ImageFont.truetype(str(font_path), FONT_SIZE)
    cw = font.getlength("M")
    ascent, descent = font.getmetrics()
    line_height = ascent + descent + 5
    rows = max(len(f[0]) for f in frames_)
    width = int(PAD * 2 + COLS * cw)
    height = int(BAR_H + PAD + rows * line_height + PAD)

    def draw(lines: list) -> Image.Image:
        image = Image.new("RGB", (width, height), BG)
        pen = ImageDraw.Draw(image)
        pen.rectangle([0, 0, width, BAR_H], fill=BAR)
        for index, colour in enumerate(DOTS):
            cx, cy = PAD + index * 20, BAR_H // 2
            pen.ellipse([cx - 5, cy - 5, cx + 5, cy + 5], fill=colour)
        y = BAR_H + PAD
        for segments in lines:
            x = PAD
            for text, colour in segments:
                pen.text((x, y), text, font=font, fill=colour)
                x += font.getlength(text)
            y += line_height
        return image

    images = [draw(lines) for lines, _ in frames_]
    images[0].save(
        out, save_all=True, append_images=images[1:],
        duration=[ms for _, ms in frames_], loop=0, optimize=True, disposal=2,
    )


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    bus = os.environ.get("ONEMESSAGEBUS_BIN", str(repo / "target/release/onemessagebus"))
    font = repo / "screenshots/fonts/JetBrainsMono-Regular.ttf"
    out = Path(os.environ.get("SUBSCRIBE_GIF_OUT", repo / "docs/screenshots/subscribe.gif"))

    if not Path(bus).is_file():
        if not os.environ.get("SCREENSHOTS_NO_BUILD"):
            subprocess.run(
                ["cargo", "build", "--release", "--locked", "-p", "onemessagebus-cli"],
                cwd=repo, check=True,
            )
        if not Path(bus).is_file():
            print(f"subscribe-gif: no onemessagebus binary at {bus}", file=sys.stderr)
            print("               Build it: cargo build --release -p onemessagebus-cli", file=sys.stderr)
            return 1
    if not font.is_file():
        print(f"subscribe-gif: missing the vendored font at {font}", file=sys.stderr)
        return 1

    root = Path(tempfile.mkdtemp(prefix="onemessagebus-gif-"))
    try:
        config = stage(root, repo)
        lines = tail(bus, root, config)
    finally:
        shutil.rmtree(root, ignore_errors=True)

    command = [
        "onemessagebus subscribe questions --format text \\",
        "--until '{\"field\":\"kind\",\"equals\":\"complete\"}' --config bus.yaml",
    ]
    out.parent.mkdir(parents=True, exist_ok=True)
    render(frames(command, lines), font, out)
    print(f"subscribe-gif: wrote {out} ({len(lines)} lines tailed)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
