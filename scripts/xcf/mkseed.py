#!/usr/bin/env python3
"""Writes two hand-made XCF files that GIMP 2.10 cannot produce by itself.

  seed_zlib.xcf  v008 with zlib tiles. GIMP keeps an image's compression
                 when it reloads it, so gen.scm opens this and builds the
                 zlib fixtures on top of it.
  v0_raw.xcf     "gimp xcf file" (version 0) with uncompressed tiles.

Usage: mkseed.py OUTDIR
"""
import struct, sys, zlib

def u32(v): return struct.pack(">I", v)
def i32(v): return struct.pack(">i", v)
def prop(t, payload): return u32(t) + u32(len(payload)) + payload

def xcf(version, compression, w, h, pixels, offsets=(0, 0), opacity=255):
    """One RGBA layer of w*h pixels (bytes, RGBA order) on a w*h canvas."""
    head = b"gimp xcf " + version + b"\0"
    head += u32(w) + u32(h) + u32(0)
    if version != b"file":
        head += u32(150)  # 8-bit gamma
    head += prop(17, bytes([compression])) + prop(0, b"")
    # layer list: 1 layer, then empty channel list
    layer_ptr_pos = len(head)
    head += u32(0) + u32(0) + u32(0)
    layer_off = len(head)
    name = b"seed\0"
    layer = u32(w) + u32(h) + u32(1) + u32(len(name)) + name
    layer += prop(6, u32(opacity)) + prop(8, u32(1)) + prop(7, u32(0))
    layer += prop(15, i32(offsets[0]) + i32(offsets[1])) + prop(0, b"")
    hier_off = layer_off + len(layer) + 8
    layer += u32(hier_off) + u32(0)
    level_off = hier_off + 20
    hier = u32(w) + u32(h) + u32(4) + u32(level_off) + u32(0)
    tiles = []
    for ty in range(0, h, 64):
        for tx in range(0, w, 64):
            tw, th = min(64, w - tx), min(64, h - ty)
            raw = b"".join(pixels[((ty + y) * w + tx) * 4:((ty + y) * w + tx + tw) * 4]
                           for y in range(th))
            tiles.append(zlib.compress(raw) if compression == 2 else raw)
    level = u32(w) + u32(h)
    data_off = level_off + 8 + 4 * (len(tiles) + 1)
    blob = b""
    for t in tiles:
        level += u32(data_off + len(blob))
        blob += t
    level += u32(0)
    out = bytearray(head + layer + hier + level + blob)
    out[layer_ptr_pos:layer_ptr_pos + 4] = u32(layer_off)
    return bytes(out)

def pattern(w, h):
    px = bytearray()
    for y in range(h):
        for x in range(w):
            px += bytes([(x * 255) // max(1, w - 1), (y * 255) // max(1, h - 1),
                         128 if (x // 8 + y // 8) % 2 else 32,
                         255 if x < w - 10 else 128])
    return bytes(px)

out = sys.argv[1]
open(f"{out}/seed_zlib.xcf", "wb").write(xcf(b"v008", 2, 8, 8, pattern(8, 8)))
open(f"{out}/v0_raw.xcf", "wb").write(xcf(b"file", 0, 70, 20, pattern(70, 20)))
