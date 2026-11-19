"""把压缩存储的录制会话展开成 OpenQuestCapture 的原始格式。

用法: python expand_session.py <压缩会话目录> <输出目录>

JPEG 相机帧还原成 YUV_420_888 的平面布局（.yuv），压缩的 16 位深度还原成
小端 float32（.raw），其余文件原样复制。已经是原始格式的内容直接复制。
"""

import json
import shutil
import sys
import zlib
from pathlib import Path

import numpy as np
from PIL import Image

SIDES = ("left", "right")


def decode_jpeg_ycbcr(path: Path) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """解码出全分辨率的 Y 和 2x2 下采样的 Cb、Cr，不经过 RGB。"""
    with Image.open(path) as image:
        image.draft("YCbCr", image.size)
        if image.mode != "YCbCr":
            image = image.convert("YCbCr")
        ycbcr = np.asarray(image, dtype=np.uint8)
    height, width = ycbcr.shape[:2]
    chroma = ycbcr[:, :, 1:].astype(np.uint16)
    chroma = chroma.reshape(height // 2, 2, width // 2, 2, 2).sum(axis=(1, 3))
    chroma = ((chroma + 2) // 4).astype(np.uint8)
    return ycbcr[:, :, 0], chroma[:, :, 0], chroma[:, :, 1]


def pack_plane(samples: np.ndarray, row_stride: int) -> np.ndarray:
    """把每行的采样放进 row_stride 宽的行里，行尾补零。"""
    rows = np.zeros((samples.shape[0], row_stride), dtype=np.uint8)
    rows[:, : samples.shape[1]] = samples
    return rows.ravel()


def pack_yuv(y: np.ndarray, u: np.ndarray, v: np.ndarray, planes: list[dict]) -> bytes:
    """按格式描述里三个平面的步长和缓冲区大小排出 .yuv 文件内容。"""
    out = [pack_plane(y, planes[0]["rowStride"])[: planes[0]["bufferSize"]]]
    if planes[1]["pixelStride"] == 1:
        out.append(pack_plane(u, planes[1]["rowStride"])[: planes[1]["bufferSize"]])
        out.append(pack_plane(v, planes[2]["rowStride"])[: planes[2]["bufferSize"]])
    else:
        # 两个色度平面是同一块 U V 交错内存的两个视图，V 平面比 U 平面晚一个字节。
        interleaved = np.empty((u.shape[0], u.shape[1] * 2), dtype=np.uint8)
        interleaved[:, 0::2] = u
        interleaved[:, 1::2] = v
        packed = pack_plane(interleaved, planes[1]["rowStride"])
        out.append(packed[: planes[1]["bufferSize"]])
        out.append(packed[1 : 1 + planes[2]["bufferSize"]])
    return b"".join(part.tobytes() for part in out)


def expand_camera(session: Path, output: Path, side: str) -> int:
    format_path = session / f"{side}_camera_image_format.json"
    source = session / f"{side}_camera_raw"
    target = output / f"{side}_camera_raw"
    if not format_path.exists() or not source.is_dir():
        return 0
    info = json.loads(format_path.read_text(encoding="utf-8"))
    info.pop("storage", None)
    info.pop("jpegQuality", None)
    (output / format_path.name).write_text(json.dumps(info, indent=2) + "\n", encoding="utf-8")

    target.mkdir(parents=True, exist_ok=True)
    count = 0
    for path in sorted(source.iterdir()):
        if path.suffix == ".jpg":
            y, u, v = decode_jpeg_ycbcr(path)
            (target / f"{path.stem}.yuv").write_bytes(pack_yuv(y, u, v, info["planes"]))
        else:
            shutil.copy2(path, target / path.name)
        count += 1
    return count


def expand_depth(session: Path, output: Path, side: str) -> int:
    source = session / f"{side}_depth"
    target = output / f"{side}_depth"
    if not source.is_dir():
        return 0
    target.mkdir(parents=True, exist_ok=True)
    count = 0
    for path in sorted(source.iterdir()):
        if path.suffix == ".depth16z":
            raw = np.frombuffer(zlib.decompress(path.read_bytes()), dtype="<u2")
            normalized = raw.astype(np.float32) / np.float32(65535)
            (target / f"{path.stem}.raw").write_bytes(normalized.astype("<f4").tobytes())
        else:
            shutil.copy2(path, target / path.name)
        count += 1
    return count


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    session, output = Path(sys.argv[1]), Path(sys.argv[2])
    if not session.is_dir():
        print(f"not a directory: {session}")
        return 1
    if output.resolve() == session.resolve():
        print("output directory must differ from the session directory")
        return 1
    output.mkdir(parents=True, exist_ok=True)

    handled = set()
    for side in SIDES:
        frames = expand_camera(session, output, side)
        depths = expand_depth(session, output, side)
        print(f"{side}: {frames} camera frames, {depths} depth maps")
        handled |= {f"{side}_camera_raw", f"{side}_depth", f"{side}_camera_image_format.json"}
    for path in session.iterdir():
        if path.name not in handled and path.is_file():
            shutil.copy2(path, output / path.name)
    print(f"expanded to {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
