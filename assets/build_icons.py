"""Generate the WinDroidFS icons.

Shapes are defined in a 96x96 space (matching windroidfs.svg) and drawn with Pillow at 4x,
then downsampled, which antialiases the edges. "windroidfs" is the shipping icon; the other
entries in CONCEPTS are the alternatives that were considered, kept so the decision can be
revisited without redrawing them.

Usage:
    python assets/build_icons.py windroidfs      # the shipping .ico + .png
    python assets/build_icons.py --android       # the Android legacy .webp launcher icons
    python assets/build_icons.py folder-arrow    # any single alternative, to compare

After regenerating, copy the .ico into the crate so build.rs picks it up:
    cp assets/windroidfs.ico apps/windows-daemon/assets/windroidfs.ico
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw

OUT_DIR = Path(__file__).parent
ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]
SUPERSAMPLE = 4
UNIT = 96  # the design-space size the coordinates below are written in


def scaled(coords, k):
    return [c * k for c in coords]


def bolt_phone(draw, k):
    """Blue tile, white phone outline, amber bolt."""
    draw.rounded_rectangle(scaled([0, 0, 96, 96], k), radius=20 * k, fill="#185FA5")
    draw.rounded_rectangle(
        scaled([30, 18, 66, 78], k),
        radius=7 * k,
        outline="#FFFFFF",
        width=max(1, round(5 * k)),
    )
    draw.polygon(
        [(53 * k, 27 * k), (40 * k, 53 * k), (48 * k, 53 * k),
         (44 * k, 70 * k), (58 * k, 45 * k), (49 * k, 45 * k)],
        fill="#FAC775",
    )


def drive_plate(draw, k):
    """Dark tile, two white drive bars, green activity dot."""
    draw.rounded_rectangle(scaled([0, 0, 96, 96], k), radius=20 * k, fill="#444441")
    draw.rounded_rectangle(scaled([22, 33, 74, 43], k), radius=5 * k, fill="#FFFFFF")
    draw.rounded_rectangle(scaled([22, 53, 56, 63], k), radius=5 * k, fill="#FFFFFF")
    draw.ellipse(scaled([60, 51, 74, 65], k), fill="#97C459")


def folder_bolt(draw, k):
    """Teal tile, white folder, bolt knocked out of the folder."""
    draw.rounded_rectangle(scaled([0, 0, 96, 96], k), radius=20 * k, fill="#0F6E56")
    draw.rounded_rectangle(scaled([22, 28, 48, 40], k), radius=4 * k, fill="#FFFFFF")
    draw.rounded_rectangle(scaled([22, 36, 74, 72], k), radius=6 * k, fill="#FFFFFF")
    draw.polygon(
        [(52 * k, 40 * k), (40 * k, 60 * k), (47 * k, 60 * k),
         (44 * k, 72 * k), (57 * k, 51 * k), (49 * k, 51 * k)],
        fill="#0F6E56",
    )


DARK_TILE = "#171125"


def _big_folder(draw, k, fill):
    """The shared folder silhouette: deliberately large so it survives 16px."""
    draw.rounded_rectangle(scaled([13, 21, 45, 37], k), radius=5 * k, fill=fill)
    draw.rounded_rectangle(scaled([13, 30, 83, 78], k), radius=8 * k, fill=fill)


def _fat_bolt(draw, k, fill):
    draw.polygon(
        [(56 * k, 36 * k), (36 * k, 56 * k), (47 * k, 56 * k),
         (41 * k, 70 * k), (60 * k, 50 * k), (49 * k, 50 * k)],
        fill=fill,
    )


def folder_bolt_dark(draw, k):
    """Dark tile, big white folder, chunky bolt knocked out of it."""
    draw.rounded_rectangle(scaled([0, 0, 96, 96], k), radius=20 * k, fill=DARK_TILE)
    _big_folder(draw, k, "#FFFFFF")
    _fat_bolt(draw, k, DARK_TILE)


def folder_amber(draw, k):
    """Same silhouette, amber folder instead of white."""
    draw.rounded_rectangle(scaled([0, 0, 96, 96], k), radius=20 * k, fill=DARK_TILE)
    _big_folder(draw, k, "#EF9F27")
    _fat_bolt(draw, k, DARK_TILE)


def _transfer_arrow(draw, k, fill):
    draw.polygon(
        [(28 * k, 49 * k), (50 * k, 49 * k), (50 * k, 39 * k), (69 * k, 54 * k),
         (50 * k, 69 * k), (50 * k, 59 * k), (28 * k, 59 * k)],
        fill=fill,
    )


def folder_arrow(draw, k):
    """Dark tile, big white folder, transfer arrow knocked out of it."""
    draw.rounded_rectangle(scaled([0, 0, 96, 96], k), radius=20 * k, fill=DARK_TILE)
    _big_folder(draw, k, "#FFFFFF")
    _transfer_arrow(draw, k, DARK_TILE)


def folder_amber_arrow(draw, k):
    """Amber folder with the arrow cutout: the most legible pairing at 16px."""
    draw.rounded_rectangle(scaled([0, 0, 96, 96], k), radius=20 * k, fill=DARK_TILE)
    _big_folder(draw, k, "#EF9F27")
    _transfer_arrow(draw, k, DARK_TILE)


def _double_headed_arrow(draw, k, fill):
    """One shaft, a head at each end. Drawn as three overlapping shapes."""
    draw.polygon([(22 * k, 54 * k), (40 * k, 40 * k), (40 * k, 68 * k)], fill=fill)
    draw.polygon([(74 * k, 54 * k), (56 * k, 40 * k), (56 * k, 68 * k)], fill=fill)
    draw.rectangle(scaled([36, 48, 60, 60], k), fill=fill)


def folder_double_arrow(draw, k):
    """Amber folder, one double-headed arrow (<->)."""
    draw.rounded_rectangle(scaled([0, 0, 96, 96], k), radius=20 * k, fill=DARK_TILE)
    _big_folder(draw, k, "#EF9F27")
    _double_headed_arrow(draw, k, DARK_TILE)


def folder_stacked_arrows(draw, k):
    """Amber folder, two opposing stacked arrows (the classic sync glyph)."""
    draw.rounded_rectangle(scaled([0, 0, 96, 96], k), radius=20 * k, fill=DARK_TILE)
    _big_folder(draw, k, "#EF9F27")
    # top arrow, pointing right
    draw.rectangle(scaled([26, 42, 56, 50], k), fill=DARK_TILE)
    draw.polygon([(70 * k, 46 * k), (54 * k, 37 * k), (54 * k, 55 * k)], fill=DARK_TILE)
    # bottom arrow, pointing left
    draw.rectangle(scaled([40, 58, 70, 66], k), fill=DARK_TILE)
    draw.polygon([(26 * k, 62 * k), (42 * k, 53 * k), (42 * k, 71 * k)], fill=DARK_TILE)


AMBER = "#EF9F27"
# Drawing in this "colour" punches a hole: ImageDraw replaces pixels rather than blending, so
# a cutout shows whatever is behind the icon (taskbar, desktop, Explorer) on any theme.
HOLE = (0, 0, 0, 0)


def _filling_folder(draw, k, fill):
    """The folder silhouette grown to fill the canvas, for icons with no tile behind them."""
    draw.rounded_rectangle(scaled([4, 10, 44, 32], k), radius=7 * k, fill=fill)  # tab
    draw.rounded_rectangle(scaled([4, 22, 92, 86], k), radius=10 * k, fill=fill)  # body


def folder_only_double_arrow(draw, k):
    """No tile: a big amber folder with a double-headed arrow cut through it."""
    _filling_folder(draw, k, AMBER)
    draw.polygon([(12 * k, 56 * k), (34 * k, 37 * k), (34 * k, 75 * k)], fill=HOLE)
    draw.polygon([(84 * k, 56 * k), (62 * k, 37 * k), (62 * k, 75 * k)], fill=HOLE)
    draw.rectangle(scaled([30, 48, 66, 64], k), fill=HOLE)


def folder_only_arrow(draw, k):
    """No tile, single arrow: the double head merges into a lump at 16-24px."""
    _filling_folder(draw, k, AMBER)
    draw.polygon(
        [(18 * k, 48 * k), (52 * k, 48 * k), (52 * k, 35 * k), (80 * k, 56 * k),
         (52 * k, 77 * k), (52 * k, 64 * k), (18 * k, 64 * k)],
        fill=HOLE,
    )


CONCEPTS = {
    "bolt-phone": bolt_phone,
    "drive-plate": drive_plate,
    "folder-bolt": folder_bolt,
    "folder-bolt-dark": folder_bolt_dark,
    "folder-amber": folder_amber,
    "folder-arrow": folder_arrow,
    "folder-amber-arrow": folder_amber_arrow,
    "folder-double-arrow": folder_double_arrow,
    "folder-stacked-arrows": folder_stacked_arrows,
    "folder-only-double-arrow": folder_only_double_arrow,
    "folder-only-arrow": folder_only_arrow,
    "windroidfs": None,  # size-dependent, see SIZE_OVERRIDES
}

# The shipping icon: no tile, just the folder, as large as the canvas allows (a dark tile ate a
# quarter of a 16px tray icon). A double-headed arrow says "transfer" better, but at 16px its
# two heads merge with the shaft into a lump, so the small frames fall back to the single
# arrow. Shipping different artwork per size is what a multi-resolution .ico is for.
SIZE_OVERRIDES = {
    "windroidfs": {
        16: folder_only_arrow,
        24: folder_only_arrow,
        None: folder_only_double_arrow,  # everything 32 and up
    }
}


def paint_for(name, size):
    overrides = SIZE_OVERRIDES.get(name)
    if overrides:
        return overrides.get(size) or overrides[None]
    return CONCEPTS[name]


def android_legacy(round_mask):
    """Pre-API-26 launcher icon (API 24-25 only; newer ones use the adaptive vectors).

    Old launchers don't mask icons into a shape, so with no tile the square and round
    variants are the same: the Windows artwork at full size.
    """
    return folder_only_double_arrow


# Density buckets the Android launcher asks for, in px.
ANDROID_DENSITIES = {
    "mdpi": 48,
    "hdpi": 72,
    "xhdpi": 96,
    "xxhdpi": 144,
    "xxxhdpi": 192,
}

ANDROID_RES = (
    Path(__file__).parent.parent / "apps/android-host/app/src/main/res"
)


def build_android():
    for density, size in ANDROID_DENSITIES.items():
        out = ANDROID_RES / f"mipmap-{density}"
        out.mkdir(parents=True, exist_ok=True)
        for name, round_mask in (("ic_launcher", False), ("ic_launcher_round", True)):
            image = render(android_legacy(round_mask), size)
            image.save(out / f"{name}.webp", format="WEBP", lossless=True)
        print(f"mipmap-{density:<8} {size}px  ic_launcher.webp + ic_launcher_round.webp")


def render(paint, size):
    """Draw one icon at `size` px, supersampled for smooth edges."""
    big = size * SUPERSAMPLE
    image = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    paint(ImageDraw.Draw(image), big / UNIT)
    return image.resize((size, size), Image.LANCZOS)


def main():
    args = sys.argv[1:]
    if "--android" in args:
        args.remove("--android")
        build_android()
        if not args:
            return

    wanted = args or list(CONCEPTS)
    OUT_DIR.mkdir(parents=True, exist_ok=True)

    for name in wanted:
        if name not in CONCEPTS:
            sys.exit(f"Unknown concept {name!r}. Choose from: {', '.join(CONCEPTS)}")

        frames = [render(paint_for(name, size), size) for size in ICO_SIZES]
        ico_path = OUT_DIR / f"{name}.ico"
        # Pillow writes every supplied frame into the .ico when sizes are listed.
        frames[-1].save(ico_path, format="ICO",
                        sizes=[(s, s) for s in ICO_SIZES],
                        append_images=frames[:-1])

        png_path = OUT_DIR / f"{name}-256.png"
        frames[-1].save(png_path)
        print(f"{ico_path.name:20} {len(ICO_SIZES)} sizes   + {png_path.name}")


if __name__ == "__main__":
    main()
