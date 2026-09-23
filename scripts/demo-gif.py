#!/usr/bin/env python3
"""Render the animated GIF of one change's whole life — the README hero.

Like `scripts/screenshots.sh`, this drives the **real release `onevcs` binary**
against the scratch host `scripts/screenshots-capture.sh` builds: real bare origins,
real clones, a real executable pre-push hook, real pushes, and the e2e tier's own
`gh` stand-in for the remote host's decisioning. So the session token, the commit,
the event envelopes and the merge are genuine `onevcs` output — there is no model, no
network, no credential and no real GitHub in it.

What is reconstructed rather than screen-recorded is the *arrival*. `onevcs events
TOKEN --follow` is a real tail — it drains the stream and sleeps 100 ms until the
session closes — and capturing a live PTY hermetically would need `ttyd`/`ffmpeg` and
would not be reproducible anyway. Instead the frames a terminal would draw are
rebuilt from that run's real output, line by line, in the order it arrived, and
rendered with the same **vendored, pinned JetBrains Mono** the SVG screenshots use
(`screenshots/fonts/`). Pillow only.

The GIF is informational, like the stills, but it is **not** hash-gated: a GIF is not
byte-reproducible across Pillow versions. So it is regenerated on demand with
`just screenshots-gif` and committed to `docs/screenshots/demo.gif`. Regenerate it
when the event envelope or any of these four commands' output changes.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

# GitHub-dark, matching the SVG screenshots' window (background #0d1117).
BG = (13, 17, 23)
BAR = (22, 27, 34)
FG = (201, 209, 217)
DIM = (139, 148, 158)
PROMPT = (57, 197, 207)
COMMAND = (201, 209, 217)
DOTS = [(255, 95, 86), (255, 189, 46), (39, 201, 63)]  # traffic-light window dots

COLS = 104           # display columns; longer lines fold at this width
ROWS = 26            # visible rows — the view scrolls once the output passes this
FONT_SIZE = 15
PAD = 22
BAR_H = 36
COMMAND_MS = 900     # hold after a command line lands
LINE_MS = 520        # per output line, however many rows it folds onto
HOLD_MS = 3200       # hold on the settled view


def fold(text: str) -> list[str]:
    """Wrap one output line the way a terminal of COLS columns would."""
    if text == "":
        return [""]
    return [text[at:at + COLS] for at in range(0, len(text), COLS)]


def transcript(work: Path) -> list[tuple[list[tuple[str, tuple[int, int, int]]], int]]:
    """The beats of the hero, each a list of (text, color) lines and a duration.

    One beat per command line and one per output line, so the view fills the way it
    does in a terminal: the command, then what it printed, then the next command.
    """
    role_color = {"out": FG, "stream": DIM}
    beats: list[tuple[list[tuple[str, tuple[int, int, int]]], int]] = []
    for line in (work / "hero/steps.tsv").read_text().splitlines():
        if not line:
            continue
        _name, shown, outputs = line.split("\t")
        beats.append(([("$ " + shown, COMMAND)], COMMAND_MS))
        for output in filter(None, outputs.split(",")):
            name, _, role = output.partition(":")
            color = role_color[role]
            for printed in (work / "hero" / name).read_text().splitlines():
                # One beat per *line*, with all the rows it folds onto: a terminal
                # receives an event envelope whole, so its wrapped rows appear
                # together rather than crawling into view one row at a time.
                beats.append(([(folded, color) for folded in fold(printed)], LINE_MS))
    return beats


def frames(beats):
    """Accumulate the beats into one frame per beat, scrolled to the last ROWS."""
    lines: list[tuple[str, tuple[int, int, int]]] = []
    out = []
    for added, duration in beats:
        lines.extend(added)
        out.append((lines[-ROWS:], duration))
    out[-1] = (out[-1][0], HOLD_MS)
    return out


def render(rendered, font_path: str, out: str) -> None:
    font = ImageFont.truetype(font_path, FONT_SIZE)
    cw = font.getlength("M")
    ascent, descent = font.getmetrics()
    line_h = ascent + descent + 4
    width = int(PAD * 2 + COLS * cw)
    height = int(BAR_H + PAD + ROWS * line_h + PAD)

    def draw(lines) -> Image.Image:
        image = Image.new("RGB", (width, height), BG)
        pen = ImageDraw.Draw(image)
        pen.rectangle([0, 0, width, BAR_H], fill=BAR)
        for index, color in enumerate(DOTS):
            cx, cy = PAD + index * 21, BAR_H // 2
            pen.ellipse([cx - 6, cy - 6, cx + 6, cy + 6], fill=color)
        y = BAR_H + PAD
        for text, color in lines:
            if text.startswith("$ "):
                pen.text((PAD, y), "$", font=font, fill=PROMPT)
                pen.text((PAD + 2 * cw, y), text[2:], font=font, fill=color)
            else:
                pen.text((PAD, y), text, font=font, fill=color)
            y += line_h
        return image

    # Every frame scrolls, so no two are alike and GIF's inter-frame compression has
    # nothing to work with — the palette is the only lever. The view uses six colors,
    # so quantizing to eight keeps it exact and takes the file from megabytes to
    # hundreds of kilobytes. `dither=NONE` because dithering flat text is noise that
    # compresses badly and reads as fringing.
    palette = Image.new("P", (1, 1))
    flat = [channel for color in (BG, BAR, FG, DIM, PROMPT, *DOTS) for channel in color]
    palette.putpalette(flat + [0] * (768 - len(flat)))
    images = [
        draw(lines).quantize(palette=palette, dither=Image.Dither.NONE)
        for lines, _ in rendered
    ]
    images[0].save(
        out,
        save_all=True,
        append_images=images[1:],
        duration=[ms for _, ms in rendered],
        loop=0,
        optimize=True,
        disposal=2,
    )


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    binary = os.environ.get("ONEVCS_BIN", str(root / "target/release/onevcs"))
    font_path = str(root / "screenshots/fonts/JetBrainsMono-Regular.ttf")
    out = os.environ.get("DEMO_GIF_OUT", str(root / "docs/screenshots/demo.gif"))

    for path in (binary, font_path):
        if not Path(path).exists():
            print(f"demo-gif: missing {path}", file=sys.stderr)
            return 1

    with tempfile.TemporaryDirectory(prefix="onevcs-gif-") as scratch:
        work = Path(scratch) / "capture"
        subprocess.run(
            ["bash", str(root / "scripts/screenshots-capture.sh"), str(work)],
            check=True,
            env={**os.environ, "ONEVCS_BIN": binary},
        )
        rendered = frames(transcript(work))

    Path(out).parent.mkdir(parents=True, exist_ok=True)
    render(rendered, font_path, out)
    print(f"demo-gif: wrote {out} ({len(rendered)} frames)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
