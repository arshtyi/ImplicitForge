"""Collect data/frames.csv, data/run.json and the CPU-rendered figures for Typst."""

import csv
import datetime
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
from pathlib import Path
from tempfile import TemporaryDirectory

from figures import FigureMetrics, generate

REPORT = Path(__file__).resolve().parent
ROOT = REPORT.parent


def cpu_model() -> str:
    if sys.platform.startswith("linux"):
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.partition(":")[2].strip()
    if sys.platform == "darwin":
        result = subprocess.run(
            ["sysctl", "-n", "machdep.cpu.brand_string"],
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode == 0 and result.stdout.strip():
            return result.stdout.strip()
    return platform.processor() or platform.machine()


def collect(figure_metrics: FigureMetrics) -> None:
    data = REPORT / "data"
    data.mkdir(exist_ok=True)
    cmd = ["cargo", "run", "--locked", "--release", "--bin", "measure"]
    # Stage a complete measurement before replacing the source of the finished tables.
    with TemporaryDirectory(dir=data) as tmp:
        frames = Path(tmp) / "frames.csv"
        with frames.open("wb") as out:
            run = subprocess.run(
                cmd, cwd=ROOT, stdout=out, stderr=subprocess.PIPE, check=False
            )
        log = run.stderr.decode(errors="replace")
        if run.returncode != 0:
            raise SystemExit(log)
        with frames.open(encoding="utf-8", newline="") as f:
            rows = list(csv.DictReader(f))
        counts = re.search(
            r"Rayon workers: (\d+)\. Timed frames per variant: (\d+)\.", log
        )
        if counts is None:
            raise ValueError("Missing worker or sample count in benchmark output")
        workers, samples = map(int, counts.groups())
        adapter = re.search(r"^Compute adapter: (.+)$", log, re.MULTILINE)
        unavailable = re.search(r"^Compute unavailable: (.+)$", log, re.MULTILINE)
        if not (adapter or unavailable):
            raise ValueError("Missing compute adapter or failure reason")
        full_modes = ["serial-full", "serial-pruned", "rayon-full", "rayon-pruned"]
        if adapter:
            full_modes.append("wgpu-full")
        groups: dict[tuple[str, str, str, str], list[int]] = {
            (case, mode, "160", "96"): []
            for case in ("few", "many", "sparse", "chain", "sculpture")
            for mode in full_modes
        }
        groups.update(
            {
                ("sculpture", "rayon-pruned", w, h): []
                for w, h in [("40", "24"), ("80", "48")]
            }
        )
        for r in rows:
            groups[(r["case"], r["mode"], r["width"], r["height"])].append(
                int(r["sample"])
            )
        if any(sorted(ids) != list(range(samples)) for ids in groups.values()):
            raise ValueError("Incomplete benchmark sample set")
        meta = {
            "captured_utc": datetime.datetime.now(datetime.UTC).isoformat(
                timespec="seconds"
            ),
            "system": platform.system(),
            "kernel": platform.release(),
            "arch": platform.machine(),
            "cpu": cpu_model(),
            "workers": workers,
            "samples": samples,
            "figures": figure_metrics,
            "adapter": adapter.group(1) if adapter else None,
            "unavailable": unavailable.group(1) if unavailable else None,
            "rustc": subprocess.check_output(["rustc", "-V"], text=True).strip(),
            "profile": "release",
            "command": " ".join(cmd),
            "environment": {
                var: os.environ[var]
                for var in (
                    "RAYON_NUM_THREADS",
                    "WGPU_BACKEND",
                    "GALLIUM_DRIVER",
                    "MESA_LOADER_DRIVER_OVERRIDE",
                    "LIBGL_ALWAYS_SOFTWARE",
                    "MESA_D3D12_DEFAULT_ADAPTER_NAME",
                    "VK_DRIVER_FILES",
                )
                if var in os.environ
            },
            "frames_sha256": hashlib.sha256(frames.read_bytes()).hexdigest(),
        }
        metadata = Path(tmp) / "run.json"
        metadata.write_text(
            json.dumps(meta, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
        frames.replace(data / frames.name)
        metadata.replace(data / metadata.name)
        print(
            f"{len(rows)} samples, {workers} workers, {meta['adapter'] or meta['unavailable']}"
        )


if __name__ == "__main__":
    collect(generate())
