# PROTOTYPE (#243): renders orbit and low-altitude views of dumped world maps. Throwaway.
import sys, numpy as np, pathlib
from PIL import Image
OUT = pathlib.Path(sys.argv[1]); OUT.mkdir(exist_ok=True)
W, H = 4096, 2048
UNIT = float(sys.argv[2]) if len(sys.argv) > 2 else 0.1   # metres per elevation_at unit

def colour(e_m, peak):
    # e_m in metres; land colours scaled to the variant's peak
    c = np.zeros(e_m.shape + (3,))
    sea = e_m < 0
    d = np.clip(-e_m / 4000.0, 0, 1)[..., None]
    c[sea] = ((1 - d) * np.array([70, 130, 200]) + d * np.array([10, 25, 80]))[sea]
    t = np.clip(e_m / peak, 0, 1)[..., None]
    low, mid, rock, snow = map(np.array, ([70, 130, 60], [150, 140, 90], [125, 120, 115], [245, 245, 250]))
    land = np.where(t < 0.3, low + (mid - low) * (t / 0.3), np.where(t < 0.7, mid + (rock - mid) * ((t - 0.3) / 0.4), rock + (snow - rock) * ((t - 0.7) / 0.3)))
    c[~sea] = land[~sea]
    return c

def orbit(lat_long, peak, trench, name, lon0=0.6, lat0=0.35, size=900):
    e = lat_long * UNIT
    e = np.where(e > 0, e * peak / 2000.0, e * trench / 1000.0)
    ys, xs = np.mgrid[0:size, 0:size]
    u = (xs + 0.5) / size * 2 - 1; v = 1 - (ys + 0.5) / size * 2
    r2 = u * u + v * v; inside = r2 < 1
    z = np.sqrt(np.clip(1 - r2, 0, 1))
    # rotate view: point on sphere in view space (u,v,z) -> world
    cl, sl, co, so = np.cos(lat0), np.sin(lat0), np.cos(lon0), np.sin(lon0)
    y1 = v * cl + z * sl; z1 = -v * sl + z * cl
    x2 = u * co + z1 * so; z2 = -u * so + z1 * co
    lat = np.arcsin(np.clip(y1, -1, 1)); lon = np.arctan2(x2, z2)
    py = np.clip(((0.5 - lat / np.pi) * H).astype(int), 0, H - 1)
    px = np.clip(((lon + np.pi) / (2 * np.pi) * W).astype(int), 0, W - 1)
    el = e[py, px]
    # hillshade from the map gradient (metres per texel ~ 490 m at the equator)
    gy, gx = np.gradient(e)
    sx = gx[py, px] / 490.0; sy = gy[py, px] / 490.0
    shade = np.clip(1 - 2.5 * (sx * 0.7 - sy * 0.7), 0.35, 1.4)
    c = colour(el, peak) * np.where(el > 0, shade, 1.0)[..., None]
    c = c * (0.35 + 0.65 * z)[..., None] + np.array([150, 160, 200]) * (1 - z)[..., None] * 0.35
    img = np.where(inside[..., None], c, np.array([12, 12, 20]))
    Image.fromarray(np.clip(img, 0, 255).astype(np.uint8)).save(OUT / f"{name}.png")

def low_view(patch, peak, trench, name, cam_h=5000.0, w=1280, h=720):
    n = patch.shape[0]; step = 100.0
    e = patch * UNIT; e = np.where(e > 0, e * peak / 2000.0, e * trench / 1000.0)
    water = np.maximum(e, 0)
    col = colour(e, peak)
    gy, gx = np.gradient(e, step)
    shade = np.clip(1 + 1.6 * (-gx * 0.6 + gy * 0.4), 0.3, 1.5)
    col = col * np.where(e > 0, shade, 1.0)[..., None]
    sky_top, sky_hor = np.array([150, 160, 205]), np.array([235, 205, 190])
    img = np.zeros((h, w, 3))
    for yy in range(h):
        t = yy / h; img[yy] = sky_top * (1 - t) + sky_hor * t
    # camera at south edge centre, looking north, slightly down
    cx, cy = n / 2 * step, 0.0
    fov = np.radians(70); horizon = h * 0.42; focal = (w / 2) / np.tan(fov / 2)
    xs = (np.arange(w) - w / 2) / focal
    ybuf = np.full(w, h, dtype=float)
    dist = 50.0
    while dist < n * step * 0.98:
        px = cx + xs * dist; py = cy + dist
        ix = np.clip((px / step).astype(int), 0, n - 1); iy = min(max(n - 1 - int(py / step), 0), n - 1)
        hgt = water[iy, ix]
        sy = horizon + (cam_h - hgt) / dist * focal
        fog = 1 - np.exp(-dist / 60000.0)
        c = col[iy, ix] * (1 - fog) + sky_hor * fog
        for x in np.nonzero(sy < ybuf)[0]:
            y0 = int(max(sy[x], 0)); y1 = int(ybuf[x])
            if y0 < y1: img[y0:y1, x] = c[x]
        ybuf = np.minimum(ybuf, sy)
        dist += max(25.0, dist * 0.004)
    Image.fromarray(np.clip(img, 0, 255).astype(np.uint8)).save(OUT / f"{name}.png")

if __name__ == "__main__":
    mode = sys.argv[3]
    src = pathlib.Path(sys.argv[4]); peak = float(sys.argv[5]); trench = float(sys.argv[6]); name = sys.argv[7]
    if mode == "orbit":
        orbit(np.fromfile(src / "latlong_f32.bin", dtype="<f4").reshape(H, W), peak, trench, name)
    else:
        p = np.fromfile(src / "patch_f32.bin", dtype="<f4"); n = int(np.sqrt(p.size))
        low_view(p.reshape(n, n), peak, trench, name, cam_h=float(sys.argv[8]) if len(sys.argv) > 8 else 5000.0)
