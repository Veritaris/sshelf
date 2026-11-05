"""Builds `lucide.ttf`, the icons sshelf embeds, from `lucide-full.ttf`.

Keeps only the glyphs `src/icons.rs` names (its `pub const NAME: char =
'\\u{....}'` lines), so the binary carries a few dozen icons instead of the
whole set. Run it after adding or changing an icon there:

    python -m pip install fonttools
    python crates/sshelf/assets/fonts/subset_lucide.py

The same input gives the same bytes (no timestamps are touched). The UI
tests fail on any icon the subset lacks.
"""

import re
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

HERE = Path(__file__).resolve().parent
ICONS = HERE.parent.parent / "src" / "icons.rs"
SOURCE = HERE / "lucide-full.ttf"
TARGET = HERE / "lucide.ttf"


def codepoints() -> list[int]:
    pattern = re.compile(r"pub const \w+: char = '\\u\{([0-9a-fA-F]+)\}';")
    found = sorted({int(m.group(1), 16) for m in pattern.finditer(ICONS.read_text())})
    if not found:
        raise SystemExit(f"no icons found in {ICONS}")
    return found


def main() -> None:
    wanted = codepoints()
    options = subset.Options()
    # Keep every name record (copyright, licence, version).
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.name_legacy = True
    options.notdef_outline = True
    options.recalc_timestamp = False
    font = TTFont(SOURCE, recalcTimestamp=False)
    subsetter = subset.Subsetter(options)
    subsetter.populate(unicodes=wanted)
    subsetter.subset(font)
    missing = [cp for cp in wanted if cp not in font.getBestCmap()]
    if missing:
        raise SystemExit("not in the font: " + ", ".join(f"U+{cp:04X}" for cp in missing))
    font.save(TARGET)
    print(f"{TARGET.name}: {len(wanted)} icons, {TARGET.stat().st_size} bytes")


if __name__ == "__main__":
    main()
