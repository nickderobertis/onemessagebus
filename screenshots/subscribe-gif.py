#!/usr/bin/env python3
"""Render docs/screenshots/subscribe.gif — the README hero.

Spawns the real `subscribe` over the same fixture the stills use, reads its
stdout line by line with the instant each line landed, drives a sibling `send`
alongside it, and replays those lines at their real inter-arrival gaps. Why this
is the hero and why it is not hash-gated: screenshots/AGENTS.md.

llmlint: ignore-file[async_typed_clients_at_boundaries] the point of this renderer
is that it drives the binary as a user's shell does — `subscribe`'s stdout read
as it is flushed, with a second process writing to the same queue — so the GIF
documents the command line rather than an SDK. Reaching for the typed async
client would photograph a different product; the subprocess seam IS the surface
under capture, and it is the same seam tests/e2e/ask.rs drives.

llmlint: ignore-file[changed_behavior_has_e2e] exercising this renderer means
running a capture against the real binary, which the visual-docs adoption keeps
out of `just check`, `just gate` and CI's gate job (screenshots/AGENTS.md); its
output is committed and reviewed as an image, and the hash-gated half of the
adoption is what CI enforces.
"""

from __future__ import annotations

import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import NamedTuple

from PIL import Image, ImageDraw, ImageFont

Colour = tuple[int, int, int]

# GitHub-dark, matching the SVG stills' window (background #0d1117).
BG: Colour = (13, 17, 23)
BAR: Colour = (22, 27, 34)
FG: Colour = (201, 209, 217)
DIM: Colour = (139, 148, 158)
CYAN: Colour = (57, 197, 207)
DOTS: list[Colour] = [(255, 95, 86), (255, 189, 46), (39, 201, 63)]

COLS = 112
FONT_SIZE = 17
PAD = 22
BAR_H = 36
MIN_MS = 220        # the backlog flush is faster than a frame; floor it so it reads
MAX_MS = 1300       # a poll gap longer than this is dead air, not information
HOLD_MS = 2800      # hold on the finished tail
GAP_SECONDS = 0.9   # between sibling sends, so the tail's poll shows them arriving


class Question(NamedTuple):
    """One record a sibling process appends to the queue the tail watches."""

    kind: str
    message: str
    source: str
    blocking: bool

    def json(self) -> str:
        return (
            '{"kind":"%s","message":"%s","source":"%s","blocking":%s}'
            % (self.kind, self.message, self.source, "true" if self.blocking else "false")
        )


class Arrival(NamedTuple):
    """A line the tail printed, and how long after it started it landed."""

    after_seconds: float
    line: str


class Segment(NamedTuple):
    """A run of text drawn in one colour."""

    text: str
    colour: Colour


@dataclass(frozen=True)
class Frame:
    """What the window shows, and how long it shows it."""

    lines: list[list[Segment]]
    hold_ms: int


# The first three are the backlog `subscribe` flushes on startup; the rest arrive
# one at a time, and the `complete` record is the one `--until` admits.
BACKLOG: list[Question] = [
    Question("finding", "the base moved under the change", "proposal", True),
    Question("note", "nightly sweep is green", "sweep", False),
    Question("question", "which base do I fork from?", "proposal", True),
]
ARRIVING: list[Question] = [
    Question("note", "worker-2 claimed the retry", "desk", False),
    Question("finding", "the schema bundle pin is stale", "checkin", True),
    Question("note", "worker-2 settled", "desk", False),
    Question("complete", "the desk is clear", "lead", False),
]


def stage(root: Path, repo: Path) -> Path:
    """Stage the fixture through the one script that writes it, so the hero is
    taken over the same layout as the stills rather than a second copy of it."""
    staged = subprocess.run(
        ["bash", str(repo / "screenshots/stage-fixture.sh"), str(root)],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    return Path(staged)


def tail_argv(config: Path) -> list[str]:
    """The `subscribe` this GIF is of. One value, because the window's prompt
    line is rendered from it: a prompt written beside the argv is a second
    spelling of the command line that nothing reconciles, and the hero would go
    on showing a flag the run no longer passes."""
    return [
        "subscribe", "questions",
        "--until", '{"field":"kind","equals":"complete"}',
        "--timeout", "60",
        "--format", "text",
        "--config", config.name,
    ]


def tail(bus: str, root: Path, config: Path) -> list[Arrival]:
    """Run the real `subscribe` while a sibling sends, and hand back each line it
    printed with the instant it landed."""
    # The queue verbs read these ahead of the flags below, so an exported one
    # would steer the tail away from this fixture.
    env = {k: v for k, v in os.environ.items() if not k.startswith("ONEMESSAGEBUS_")}

    # llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] a capture's
    # argv is the scene, not a restatement of the command line: it is the command
    # the picture is of. It is also self-gating at the moment it matters — this
    # CLI refuses an argv it does not have (exit 2), `check=True` below and the
    # `subscribe` status check further down turn that into a failed
    # `just screenshots-gif` rather than a stale picture, so a flag that has moved
    # cannot be rendered. `docs/cli.md` remains the one statement of the surface.
    def send(question: Question) -> None:
        subprocess.run(
            [bus, "send", "questions", "--config", str(config)],
            input=question.json(), cwd=root, env=env, text=True,
            capture_output=True, check=True,
        )

    for question in BACKLOG:
        send(question)

    child = subprocess.Popen(
        [bus, *tail_argv(config)],
        cwd=root, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
    )

    started = time.monotonic()
    arrivals: list[Arrival] = []

    def read() -> None:
        assert child.stdout is not None
        for line in child.stdout:
            arrivals.append(Arrival(time.monotonic() - started, line.rstrip("\n")))

    reader = threading.Thread(target=read, daemon=True)
    reader.start()

    for question in ARRIVING:
        time.sleep(GAP_SECONDS)
        send(question)

    if child.wait(timeout=60) != 0:
        raise SystemExit(
            "subscribe-gif: the tail did not end on its predicate, so there is no "
            "arrival sequence to animate. Run the same `subscribe` by hand over a "
            "staged fixture (bash screenshots/stage-fixture.sh \"$(mktemp -d)\")."
        )
    # llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate]
    reader.join(timeout=5)
    if not arrivals:
        raise SystemExit("subscribe-gif: the tail printed nothing to animate")
    return arrivals


def normalize(repo: Path, text: str) -> str:
    """Rewrite the per-run values the desk stamps, through the one script that
    writes them — the same one the hash-gated stills pass their scenes through,
    so the hero and the stills read as one session."""
    return subprocess.run(
        ["bash", str(repo / "screenshots/normalize.sh")],
        input=text, check=True, capture_output=True, text=True,
    ).stdout


def prompt(argv: list[str]) -> list[str]:
    """`onemessagebus <argv>` as a shell would need it typed, folded at the
    window's column budget with a trailing backslash."""
    words = ["onemessagebus"] + [shlex.quote(word) for word in argv]
    lines: list[str] = []
    line = ""
    for word in words:
        if line and len(line) + len(word) + 1 > COLS - 2:
            lines.append(line + " \\")
            line = "    " + word
        else:
            line = f"{line} {word}" if line else word
    lines.append(line)
    return lines


def wrap(line: str) -> list[str]:
    """Fold an over-wide line at the window's column budget, as freeze does."""
    return [line[i:i + COLS] for i in range(0, len(line), COLS)] or [""]


def frames(repo: Path, command: list[str], arrivals: list[Arrival]) -> list[Frame]:
    """One frame per arriving line, held for the gap until the next one really
    arrived."""
    prompt: list[list[Segment]] = [[Segment("$ ", CYAN), Segment(command[0], FG)]]
    prompt += [[Segment("    " + part, FG)] for part in command[1:]]

    out = [Frame(list(prompt), 700)]
    shown = list(prompt)
    for index, arrival in enumerate(arrivals):
        for part in wrap(normalize(repo, arrival.line)):
            position, _, rest = part.partition(" ")
            shown = shown + [
                [Segment(position + " ", DIM), Segment(rest, FG)] if rest
                else [Segment(part, FG)]
            ]
        following = arrivals[index + 1].after_seconds if index + 1 < len(arrivals) else None
        gap = HOLD_MS if following is None else int(
            max(MIN_MS, min(MAX_MS, (following - arrival.after_seconds) * 1000))
        )
        out.append(Frame(list(shown), gap))
    return out


def render(frames_: list[Frame], font_path: Path, out: Path) -> None:
    font = ImageFont.truetype(str(font_path), FONT_SIZE)
    ascent, descent = font.getmetrics()
    line_height = ascent + descent + 5
    rows = max(len(frame.lines) for frame in frames_)
    width = int(PAD * 2 + COLS * font.getlength("M"))
    height = int(BAR_H + PAD + rows * line_height + PAD)

    def draw(lines: list[list[Segment]]) -> Image.Image:
        image = Image.new("RGB", (width, height), BG)
        pen = ImageDraw.Draw(image)
        pen.rectangle([0, 0, width, BAR_H], fill=BAR)
        for index, colour in enumerate(DOTS):
            cx, cy = PAD + index * 20, BAR_H // 2
            pen.ellipse([cx - 5, cy - 5, cx + 5, cy + 5], fill=colour)
        y = BAR_H + PAD
        for segments in lines:
            x = PAD
            for segment in segments:
                pen.text((x, y), segment.text, font=font, fill=segment.colour)
                x += font.getlength(segment.text)
            y += line_height
        return image

    images = [draw(frame.lines) for frame in frames_]
    images[0].save(
        out, save_all=True, append_images=images[1:],
        duration=[frame.hold_ms for frame in frames_], loop=0, optimize=True, disposal=2,
    )


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    bus = os.environ.get("ONEMESSAGEBUS_BIN", str(repo / "target/release/onemessagebus"))
    font = repo / "screenshots/fonts/JetBrainsMono-Regular.ttf"
    out = Path(os.environ.get("SUBSCRIBE_GIF_OUT", repo / "docs/screenshots/subscribe.gif"))
    # The renderer creates this path's parents and overwrites what is there, so
    # bound it the way the still capture bounds `$SHOTS_OUT`: inside this
    # checkout, and a file rather than a directory.
    resolved = (repo / out).resolve() if not out.is_absolute() else out.resolve()
    if repo not in resolved.parents or resolved.is_dir():
        print(
            f"subscribe-gif: SUBSCRIBE_GIF_OUT must name a file inside this checkout;"
            f" {resolved} is not one. Unset it for docs/screenshots/subscribe.gif.",
            file=sys.stderr,
        )
        return 1
    out = resolved

    if not Path(bus).is_file():
        if not os.environ.get("SCREENSHOTS_NO_BUILD"):
            subprocess.run(
                ["cargo", "build", "--release", "--locked", "-p", "onemessagebus-cli"],
                cwd=repo, check=True,
            )
        if not Path(bus).is_file():
            print(f"subscribe-gif: no onemessagebus binary at {bus}", file=sys.stderr)
            print("               Build it: cargo build --release -p onemessagebus-cli",
                  file=sys.stderr)
            return 1
    if not font.is_file():
        print(f"subscribe-gif: missing the vendored font at {font}", file=sys.stderr)
        return 1

    root = Path(tempfile.mkdtemp(prefix="onemessagebus-gif-"))
    try:
        config = stage(root, repo)
        arrivals = tail(bus, root, config)
    finally:
        shutil.rmtree(root, ignore_errors=True)

    out.parent.mkdir(parents=True, exist_ok=True)
    render(frames(repo, prompt(tail_argv(config)), arrivals), font, out)
    print(f"subscribe-gif: wrote {out} ({len(arrivals)} lines tailed)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
