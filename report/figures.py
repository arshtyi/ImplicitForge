"""Render the report's CPU figures and measure the image differences."""

import subprocess
import zlib
from pathlib import Path
from struct import pack
from tempfile import TemporaryDirectory
from typing import TypedDict

REPORT = Path(__file__).resolve().parent
W, H = 640, 400
SIDE = 48


class DifferenceMetrics(TypedDict):
    pixels: int
    changed: int
    mae_rgb8: float


class CropMetrics(DifferenceMetrics):
    x: int
    y: int
    side: int


class FigureMetrics(TypedDict):
    width: int
    height: int
    edit: DifferenceMetrics
    crop: CropMetrics


def read_frame(path: Path) -> bytes:
    with path.open("rb") as f:
        header = [f.readline().strip() for _ in range(3)]
        rgb = f.read()
    if header != [b"P6", f"{W} {H}".encode(), b"255"] or len(rgb) != W * H * 3:
        raise ValueError(f"Unexpected renderer output in {path}")
    return rgb


def save_png(name: str, w: int, h: int, rgb: bytes) -> None:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return pack(">I", len(data)) + kind + data + pack(">I", zlib.crc32(kind + data))

    rows = b"".join(b"\0" + rgb[n : n + 3 * w] for n in range(0, len(rgb), 3 * w))
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(rows))
        + chunk(b"IEND", b"")
    )
    (REPORT / "figures" / f"{name}.png").write_bytes(png)


def crop(rgb: bytes, x: int, y: int) -> bytes:
    return b"".join(
        rgb[3 * (row * W + x) : 3 * (row * W + x + SIDE)] for row in range(y, y + SIDE)
    )


def differences(label: str, a: bytes, b: bytes) -> DifferenceMetrics:
    changed = sum(a[i : i + 3] != b[i : i + 3] for i in range(0, len(a), 3))
    mae = sum(abs(x - y) for x, y in zip(a, b)) / len(a)
    print(f"{label}: {changed}/{len(a) // 3} changed pixels, RGB8 MAE {mae:.4f}")
    return {"pixels": len(a) // 3, "changed": changed, "mae_rgb8": mae}


def generate() -> FigureMetrics:
    (REPORT / "figures").mkdir(exist_ok=True)
    with TemporaryDirectory() as directory:
        tmp = Path(directory)
        cargo = ["cargo", "run", "--locked", "--release"]
        subprocess.run(
            cargo + ["--bin", "measure", "--", "--gallery", tmp],
            cwd=REPORT.parent,
            check=True,
        )
        subprocess.run(
            cargo + ["--example", "figure_variants", "--", tmp],
            cwd=REPORT.parent,
            check=True,
        )
        base = read_frame(tmp / "demo-1.ppm")
        save_png("demo-1", W, H, base)
        for i in range(2, 10):
            name = f"demo-{i}"
            save_png(name, W, H, read_frame(tmp / f"{name}.ppm"))
        stamped = read_frame(tmp / "stamped.ppm")
        save_png("stamped", W, H, stamped)
        edit = differences("default vs stamped", base, stamped)

        center = read_frame(tmp / "center.ppm")
        # First sharp vertical transition along a fixed column on the left shoulder.
        column = W // 2 - W // 12
        for row in range(SIDE // 2, H // 2):
            n = 3 * (row * W + column)
            if max(abs(center[n + c] - center[n + c - 3 * W]) for c in range(3)) >= 24:
                break
        else:
            raise ValueError("Could not locate the upper silhouette")
        x, y = column - SIDE // 2, row - SIDE // 2
        single, four = crop(center, x, y), crop(base, x, y)
        save_png("aa-center", SIDE, SIDE, single)
        save_png("aa-2x2", SIDE, SIDE, four)
        detail = differences(
            f"{SIDE}x{SIDE} crop at ({x}, {y}), center vs 2x2", single, four
        )
        return {
            "width": W,
            "height": H,
            "edit": edit,
            "crop": {"x": x, "y": y, "side": SIDE, **detail},
        }


if __name__ == "__main__":
    generate()
