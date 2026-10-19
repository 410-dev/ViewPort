"""Writes minimal HEIC files (single image, and a 2x2 tile grid) from libx265 output."""
import struct, subprocess, sys, os

out_dir = sys.argv[1]

def hevc(w, h, hue):
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-f", "lavfi", "-i", f"testsrc2=size={w}x{h}", "-vf", f"hue=h={hue}",
         "-frames:v", "1", "-c:v", "libx265", "-x265-params", "keyint=1:info=0:log-level=error", "-f", "hevc", "-"],
        check=True, capture_output=True).stdout
    nals, i = [], 0
    starts = []
    while True:
        j = raw.find(b"\x00\x00\x01", i)
        if j < 0: break
        starts.append(j + 3); i = j + 3
    for k, s in enumerate(starts):
        e = starts[k + 1] - 3 if k + 1 < len(starts) else len(raw)
        n = raw[s:e]
        while n.endswith(b"\x00"): n = n[:-1]
        nals.append(n)
    return nals

def box(t, payload): return struct.pack(">I", 8 + len(payload)) + t + payload
def full(t, v, f, payload): return box(t, struct.pack(">I", (v << 24) | f) + payload)

def hvcc(nals):
    ps = {32: None, 33: None, 34: None}
    for n in nals:
        t = (n[0] >> 1) & 0x3F
        if t in ps and ps[t] is None: ps[t] = n
    sps = ps[33]
    rbsp = sps.replace(b"\x00\x00\x03", b"\x00\x00")
    ptl = rbsp[3:15]
    p = bytes([1]) + ptl[0:1] + ptl[1:5] + ptl[5:11] + ptl[11:12]
    p += struct.pack(">HBBBBHB", 0xF000, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0x0F)
    p += bytes([3])
    for t in (32, 33, 34):
        p += bytes([0x80 | t]) + struct.pack(">HH", 1, len(ps[t])) + ps[t]
    return box(b"hvcC", p)

def data(nals):
    return b"".join(struct.pack(">I", len(n)) + n for n in nals if ((n[0] >> 1) & 0x3F) < 32)

def heif(items, primary, props, assoc, iref=b""):
    """items: list of (id, type, bytes)."""
    def meta(offsets):
        hdlr = full(b"hdlr", 0, 0, struct.pack(">I", 0) + b"pict" + b"\0" * 12 + b"\0")
        pitm = full(b"pitm", 0, 0, struct.pack(">H", primary))
        infes = b"".join(full(b"infe", 2, 0, struct.pack(">HH", i, 0) + t + b"\0") for i, t, _ in items)
        iinf = full(b"iinf", 0, 0, struct.pack(">H", len(items)) + infes)
        ipco = box(b"ipco", b"".join(props))
        ipma_p = struct.pack(">I", len(assoc))
        for i, idx in assoc:
            ipma_p += struct.pack(">HB", i, len(idx)) + bytes(idx)
        iprp = box(b"iprp", ipco + full(b"ipma", 0, 0, ipma_p))
        iloc_p = bytes([0x44, 0x00]) + struct.pack(">H", len(items))
        for (i, _, d), off in zip(items, offsets):
            iloc_p += struct.pack(">HHHII", i, 0, 1, off, len(d))
        iloc = full(b"iloc", 0, 0, iloc_p)
        return full(b"meta", 0, 0, hdlr + pitm + iinf + iref + iprp + iloc)
    ftyp = box(b"ftyp", b"heic" + struct.pack(">I", 0) + b"mif1heic")
    m = meta([0] * len(items))
    base = len(ftyp) + len(m) + 8
    offs, o = [], base
    for _, _, d in items:
        offs.append(o); o += len(d)
    m = meta(offs)
    return ftyp + m + box(b"mdat", b"".join(d for _, _, d in items))

def ispe(w, h): return full(b"ispe", 0, 0, struct.pack(">II", w, h))

# Single 640x480 image.
n = hevc(640, 480, 0)
open(os.path.join(out_dir, "single.heic"), "wb").write(
    heif([(1, b"hvc1", data(n))], 1, [hvcc(n), ispe(640, 480)], [(1, [0x81, 2])]))

# 2x2 grid of 256x256 tiles -> 512x512, rotated 90 degrees (irot) like a portrait phone photo.
tiles = [hevc(256, 256, h) for h in (0, 90, 180, 270)]
items = [(i + 1, b"hvc1", data(t)) for i, t in enumerate(tiles)]
grid = struct.pack(">BBBBHH", 0, 0, 1, 1, 512, 512)
items.append((5, b"grid", grid))
iref = full(b"iref", 0, 0, box(b"dimg", struct.pack(">HH", 5, 4) + struct.pack(">HHHH", 1, 2, 3, 4)))
irot = box(b"irot", bytes([1]))
props = [hvcc(tiles[0]), ispe(256, 256), ispe(512, 512), irot]
assoc = [(i, [0x81, 2]) for i in range(1, 5)] + [(5, [3, 0x84])]
open(os.path.join(out_dir, "grid.heic"), "wb").write(heif(items, 5, props, assoc, iref))
print("ok")
