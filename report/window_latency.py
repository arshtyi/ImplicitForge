"""Measure live orbit feedback through X11/XInput2, winit and softbuffer.

From the repository root, with a visible, unobscured desktop window:
    cargo build --locked --release --bin implicit-forge
    uv run --project report report/window_latency.py
On WSLg with locally unpacked X11 libraries:
    env -u WAYLAND_DISPLAY LD_LIBRARY_PATH=/tmp/if-x11/usr/lib64 \
        uv run --project report report/window_latency.py
For an isolated X11 server use `xvfb-run -a -s '-screen 0 1280x800x24'`
with the same driver. Label this as a virtual display when reporting results.

Leave the pointer alone while sampling. winit's X11 backend consumes XI2
mouse events, which core XSendEvent does not generate. This driver uses XTEST
on the spawned client's verified visible window. It neither raises a window
nor sets input focus. Each trial starts after a settled Target and traverses
a closed six-motion horizontal path. Timestamps come from Rust's monotonic
clock at normal winit handler entry and immediately after successful present.
The first Preview must represent the most recent dispatched orbit generation
at presentation. A later Target must represent the released pose. The release
interval includes remaining quiet time, refinement and framebuffer submission.
Input transport before dispatch, compositor scanout and photon timing are
outside these intervals. See the CSV for raw nanoseconds, generations,
coalesced/missing positions and first presents of distinct painted tickets.
"""

import argparse
import csv
import ctypes as c
import datetime
import hashlib
import json
import math
import os
import platform
import select
import subprocess
import sys
import time
from ctypes.util import find_library
from pathlib import Path
from typing import IO, Any

REPORT = Path(__file__).resolve().parent
ROOT = REPORT.parent
# Always end at the starting pose, including the final sample.
DX = (12, 24, 36, 24, 12, 0)
INTERVAL = 0.016
META_FIELDS = ("scene", "backend", "window", "preview", "target", "quiet_ms")
FIELDS = [
    "trial",
    "phase",
    "status",
    "reason",
    "scene",
    "backend",
    "window",
    "preview",
    "target",
    "quiet_ms",
    "sent",
    "received",
    "dropped",
    "press_revision",
    "first_revision",
    "preview_revision",
    "release_revision",
    "target_revision",
    "press_ns",
    "first_motion_ns",
    "last_motion_ns",
    "preview_ns",
    "release_ns",
    "target_ns",
    "motion_preview_ms",
    "release_target_ms",
    "press_xy",
    "motions",
    "presents",
]


class Unavailable(Exception):
    """No valid measurement for this run or trial."""


class Pointer:
    def __init__(self, xid: int):
        x11, xtst = find_library("X11"), find_library("Xtst")
        if not x11 or not xtst:
            raise Unavailable("libX11 and libXtst are required")
        self.x, self.test = c.CDLL(x11), c.CDLL(xtst)
        d, win, i, u = c.c_void_p, c.c_ulong, c.c_int, c.c_uint
        ip, wp = c.POINTER(i), c.POINTER(win)
        for lib, name, out, args in [
            (self.x, "XOpenDisplay", d, [c.c_char_p]),
            (self.x, "XDefaultScreen", i, [d]),
            (self.x, "XDefaultRootWindow", win, [d]),
            (self.x, "XServerVendor", c.c_char_p, [d]),
            (self.x, "XTranslateCoordinates", i, [d, win, win, i, i, ip, ip, wp]),
            (
                self.x,
                "XQueryPointer",
                i,
                [d, win, wp, wp, ip, ip, ip, ip, c.POINTER(u)],
            ),
            (self.x, "XGrabServer", i, [d]),
            (self.x, "XUngrabServer", i, [d]),
            (self.x, "XGrabPointer", i, [d, win, i, u, i, i, win, win, win]),
            (self.x, "XUngrabPointer", i, [d, win]),
            (self.x, "XSync", i, [d, i]),
            (self.x, "XCloseDisplay", i, [d]),
            (self.test, "XTestQueryExtension", i, [d, ip, ip, ip, ip]),
            (self.test, "XTestFakeMotionEvent", i, [d, i, i, i, win]),
            (self.test, "XTestFakeButtonEvent", i, [d, u, i, win]),
        ]:
            fn = getattr(lib, name)
            fn.restype, fn.argtypes = out, args
        self.d = self.x.XOpenDisplay(None)
        if not self.d:
            raise Unavailable("cannot connect to DISPLAY")
        fields = [c.c_int() for _ in range(4)]
        if not self.test.XTestQueryExtension(self.d, *(c.byref(v) for v in fields)):
            self.x.XCloseDisplay(self.d)
            raise Unavailable("X server lacks XTEST")
        self.xid = xid
        self.screen = self.x.XDefaultScreen(self.d)
        self.root = self.x.XDefaultRootWindow(self.d)
        self.vendor = self.x.XServerVendor(self.d).decode(errors="replace")
        self.x_error = None

        # Convert a closed XID (e.g. the user closed this window mid-trial)
        # into an incomplete trace instead of Xlib's default process exit.
        @c.CFUNCTYPE(c.c_int, c.c_void_p, c.c_void_p)
        def on_error(_display, _event):
            self.x_error = "target window or X11 request became unavailable"
            return 0

        self.on_error = on_error
        self.x.XSetErrorHandler.argtypes = [type(on_error)]
        self.x.XSetErrorHandler(on_error)
        self.original, _ = self.query()
        self.last = self.original
        self.down = False

    def query(self) -> tuple[tuple[int, int], int]:
        root, child = c.c_ulong(), c.c_ulong()
        x, y, local_x, local_y = (c.c_int() for _ in range(4))
        mask = c.c_uint()
        args = (root, child, x, y, local_x, local_y, mask)
        if not self.x.XQueryPointer(self.d, self.root, *(c.byref(v) for v in args)):
            raise Unavailable("pointer is on a different screen")
        return (x.value, y.value), mask.value

    def translate(self, src: int, dst: int, xy: tuple[int, int]):
        x, y, child = c.c_int(), c.c_int(), c.c_ulong()
        if not self.x.XTranslateCoordinates(
            self.d, src, dst, *xy, c.byref(x), c.byref(y), c.byref(child)
        ):
            raise Unavailable("target window is on a different screen")
        return (x.value, y.value), child.value

    def event(self, xy: tuple[int, int], button: bool | None = None):
        # Resolve the *topmost* child at the point through WM reparenting.
        # Serialize the check and injection against other X clients' restacking.
        self.x.XGrabServer(self.d)
        try:
            pos, _ = self.translate(self.xid, self.root, xy)
            under = self.root
            while under:
                _, child = self.translate(self.root, under, pos)
                if under == self.xid and not child:
                    break
                under = child
            else:
                raise Unavailable("target point is covered, off-screen or unmapped")
            actual, state = self.query()
            # Mouse buttons 1..5; and modifiers except lock states (Caps/Num).
            if actual != self.last or state & 0x1F00 != (0x100 if self.down else 0):
                raise Unavailable("pointer or buttons changed outside this driver")
            if state & 0xED:
                raise Unavailable("release keyboard modifiers before the trial")
            if button is True:
                # A menu/modal grab could redirect clicks even at a visible
                # XID. Probe while the server is locked, then release the grab
                # before generating the real press so winit receives it.
                if self.x.XGrabPointer(self.d, self.xid, 0, 0, 1, 1, 0, 0, 0):
                    raise Unavailable("pointer is grabbed by another client")
                self.x.XUngrabPointer(self.d, 0)
            if button is None:
                if not self.test.XTestFakeMotionEvent(self.d, self.screen, *pos, 0):
                    raise Unavailable("XTEST rejected motion")
                self.last = pos
            elif actual == pos:
                if not self.test.XTestFakeButtonEvent(self.d, 1, int(button), 0):
                    raise Unavailable("XTEST rejected button event")
                self.down = button
            else:
                raise Unavailable("button target moved before dispatch")
            self.x.XSync(self.d, 0)
        finally:
            self.x.XUngrabServer(self.d)
            self.x.XSync(self.d, 0)
        if self.x_error:
            raise Unavailable(self.x_error)

    def close(self):
        try:
            if self.down:
                self.test.XTestFakeButtonEvent(self.d, 1, 0, 0)
            pos, state = self.query()
            if pos == self.last and not state & 0x1F00:
                self.test.XTestFakeMotionEvent(self.d, self.screen, *self.original, 0)
            self.x.XSync(self.d, 0)
        finally:
            self.x.XCloseDisplay(self.d)


def message(stream: IO[bytes], timeout: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    while select.select([stream], [], [], max(0, deadline - time.monotonic()))[0]:
        line = stream.readline()
        if not line:
            raise Unavailable("application exited before the matching present")
        if line.startswith(b"WINDOW_LATENCY "):
            return json.loads(line.removeprefix(b"WINDOW_LATENCY "))
    raise Unavailable("timed out waiting for normal window input and matching present")


def percentile(values: list[float], p: int) -> float | None:
    return sorted(values)[math.ceil(len(values) * p / 100) - 1] if values else None


def measured_row(
    base: dict, event: dict, previous: int, origin: tuple[int, int]
) -> dict:
    trial = event["trial"]
    if event["kind"] != "trial" or trial["id"] != base["trial"]:
        raise Unavailable("unexpected orbit press or out-of-order trial")
    for key in META_FIELDS:
        if event[key] != base[key]:
            raise Unavailable(f"{key} changed during run")
    motions = trial["motions"]
    expected = [(origin[0] + dx, origin[1]) for dx in DX]
    seen = [tuple(m["xy"]) for m in motions]
    remain = iter(expected)
    reason = trial["invalid"]
    if (
        tuple(trial["press_xy"]) != origin
        or trial["press_revision"] != previous
        or not seen
        or not all(pos in remain for pos in seen)
        or seen[-1] != origin
    ):
        reason = reason or "drag positions, starting revision or final pose differed"
    row = {
        **base,
        **{
            key: trial[key]
            for key in (
                "press_revision",
                "preview_revision",
                "release_revision",
                "target_revision",
                "press_ns",
                "release_ns",
                "preview_ns",
                "target_ns",
                "press_xy",
                "motions",
                "presents",
            )
        },
    }
    row.update(
        sent=len(DX),
        received=len(motions),
        dropped=max(0, len(DX) - len(motions)),
        first_revision=motions[0]["revision"] if motions else None,
        first_motion_ns=motions[0]["ns"] if motions else None,
        last_motion_ns=motions[-1]["ns"] if motions else None,
        status="invalid" if reason else "ok",
        reason=reason,
    )
    if not reason:
        if trial["preview_ns"] is not None:
            row["motion_preview_ms"] = (trial["preview_ns"] - motions[0]["ns"]) / 1e6
        row["release_target_ms"] = (trial["target_ns"] - trial["release_ns"]) / 1e6
    return row


def main() -> int:
    cli = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    cli.add_argument(
        "--binary", type=Path, default=ROOT / "target/release/implicit-forge"
    )
    cli.add_argument("--out", type=Path, default=REPORT / "data/window.csv")
    cli.add_argument("--warmup", type=int, default=3)
    cli.add_argument("--trials", type=int, default=21)
    cli.add_argument(
        "--timeout", type=float, default=20, help="seconds per ready/terminal present"
    )
    args = cli.parse_args()
    if args.trials < 1 or args.warmup < 0 or not args.timeout > 0:
        cli.error("require trials >= 1, warmup >= 0 and positive timeout")
    rows: list[dict] = []
    ready = None
    vendor = None
    failure = None
    command = [str(args.binary.resolve()), "--cpu"]
    digest = None
    try:
        if sys.platform != "linux" or not os.environ.get("DISPLAY"):
            raise Unavailable("requires a Linux X11 display with XTEST")
        digest = hashlib.sha256(args.binary.read_bytes()).hexdigest()
        env = {**os.environ, "IMPLICITFORGE_WINDOW_LATENCY": "1"}
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("WAYLAND_SOCKET", None)
        with subprocess.Popen(
            command, stdout=subprocess.PIPE, bufsize=0, env=env
        ) as proc:
            try:
                assert proc.stdout is not None
                ready = message(proc.stdout, args.timeout)
                if ready["kind"] != "ready" or not ready["xid"] or ready["trial"]:
                    raise Unavailable("initial frame was not an idle X11 Target")
                pointer = Pointer(ready["xid"])
                try:
                    vendor = pointer.vendor
                    origin = (ready["window"][0] // 4, ready["window"][1] // 2)
                    ends = [(origin[0] + dx, origin[1]) for dx in DX]
                    previous = ready["revision"]
                    for idx in range(1, args.warmup + args.trials + 1):
                        base = {k: ready[k] for k in META_FIELDS}
                        base.update(
                            trial=idx,
                            phase="warmup" if idx <= args.warmup else "measured",
                        )
                        sent = 0
                        try:
                            pointer.event(origin)
                            time.sleep(INTERVAL)
                            pointer.event(origin, True)
                            for pos in ends:
                                time.sleep(INTERVAL)
                                pointer.event(pos)
                                sent += 1
                            pointer.event(ends[-1], False)
                            event = message(proc.stdout, args.timeout)
                            row = measured_row(base, event, previous, origin)
                        except Unavailable as exc:
                            rows.append(
                                {
                                    **base,
                                    "sent": sent,
                                    "status": "incomplete",
                                    "reason": str(exc),
                                }
                            )
                            raise
                        rows.append(row)
                        if row["status"] != "ok":
                            raise Unavailable(row["reason"])
                        previous = event["revision"]
                finally:
                    pointer.close()
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    proc.kill()
    except (Unavailable, OSError) as exc:
        failure = str(exc)

    measured = [r for r in rows if r["phase"] == "measured" and r["status"] == "ok"]
    stats = {}
    for key in ("motion_preview_ms", "release_target_ms"):
        values = [r[key] for r in measured if r.get(key) is not None]
        stats[key] = {
            "n": len(values),
            "p50": percentile(values, 50),
            "p95": percentile(values, 95),
        }
        if not values:
            failure = failure or f"no matching samples for {key}"
    report = {
        "status": "unavailable" if failure else "complete",
        "reason": failure,
        "captured_utc": datetime.datetime.now(datetime.UTC).isoformat(),
        "platform": platform.platform(),
        "display": os.environ.get("DISPLAY"),
        "x11_vendor": vendor,
        "input": "X11 XTEST / winit XI2",
        "binary_sha256": digest,
        "command": [args.binary.name, "--cpu"],
        "rayon_num_threads": os.environ.get("RAYON_NUM_THREADS"),
        "ready": ready,
        "warmup": args.warmup,
        "trials": args.trials,
        "dx_px": DX,
        "step_interval_ms": INTERVAL * 1000,
        "quantiles": "nearest rank",
        "measured_dropped_motions": sum(r["dropped"] for r in measured),
        "summary": stats,
    }
    path = args.out.with_suffix(".partial.csv") if failure else args.out
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="", encoding="utf-8") as f:
        writer = csv.DictWriter(f, fieldnames=FIELDS)
        writer.writeheader()
        for row in rows:
            writer.writerow(
                {
                    k: json.dumps(v, separators=(",", ":"))
                    if isinstance(v, (tuple, list))
                    else v
                    for k, v in row.items()
                }
            )
    report["csv_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
    path.with_suffix(".json").write_text(
        json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
    )
    print(json.dumps(report["summary"]), flush=True)
    if failure:
        print(f"Unavailable: {failure}. Partial trace: {path}", file=sys.stderr)
        return 1
    print(f"Saved {path} and {path.with_suffix('.json')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
