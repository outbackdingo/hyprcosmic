#!/usr/bin/env python3
"""Generate waybar config.jsonc with \\uXXXX escapes from verified codepoints.

Every codepoint here was confirmed present in JetBrainsMono Nerd Font with
`fc-list :charset=<cp>`. The template is pure ASCII and uses @TOKEN@
placeholders so that no PUA character is ever typed by hand.
"""
import json
import re
import sys

# name -> codepoint. Verified present in the installed Nerd Font.
ICONS = {
    "VOL_LO": 0xF026,
    "VOL_MID": 0xF027,
    "VOL_HI": 0xF028,
    "VOL_HEADPHONE": 0xF025,
    "VOL_HEADSET": 0xF025,
    "VOL_HANDSFREE": 0xF025,
    "VOL_PHONE": 0xF095,
    "VOL_CAR": 0xF1B9,
    "VOL_MUTE": 0xF075F,
    "BT_ON": 0xF293,
    "BT_DISABLED": 0xF294,
    "BT_OFF": 0xF00B2,
    "CPU": 0xF035B,
    "MEM": 0xF0EE0,
    "TEMP": 0xF2CA,
    "WIFI": 0xF1EB,
    "ETH": 0xF0200,
    "NET_OFF": 0xF092F,
    "PLUG": 0xF1E6,
    "CAFFEINE_ON": 0xF0176,
    "CAFFEINE_OFF": 0xF06CA,
    "PLAY": 0xF04B,
    "PAUSE": 0xF04C,
    "STOP": 0xF04D,
    "NOTE": 0xF001,
    "BELL": 0xF0F3,
    "BELL_DOT": 0xF009A,
    "BELL_OFF": 0xF1F6,
    "PERF": 0xF135,
    "SAVER": 0xF06C,
    "BALANCED": 0xF24E,
    "PROFILE": 0xF0E7,
    "POWER": 0xF011,
    "BAT_00": 0xF008E,
    "BAT_10": 0xF007A,
    "BAT_20": 0xF007B,
    "BAT_30": 0xF007C,
    "BAT_40": 0xF007D,
    "BAT_50": 0xF007E,
    "BAT_60": 0xF007F,
    "BAT_70": 0xF0080,
    "BAT_80": 0xF0081,
    "BAT_90": 0xF0082,
    "BAT_100": 0xF0079,
}


def escape(cp: int) -> str:
    """JSON escape for one codepoint, surrogate pair when above the BMP."""
    return json.dumps(chr(cp), ensure_ascii=True)[1:-1]


def main() -> int:
    template_path, out_path = sys.argv[1], sys.argv[2]
    text = open(template_path, encoding="ascii").read()

    unknown = {t for t in re.findall(r"@@([A-Z0-9_]+)@@", text) if t not in ICONS}
    if unknown:
        print("unknown tokens:", sorted(unknown), file=sys.stderr)
        return 1

    used = set()

    def sub(m):
        used.add(m.group(1))
        return escape(ICONS[m.group(1)])

    out = re.sub(r"@@([A-Z0-9_]+)@@", sub, text)

    if not out.isascii():
        print("generated file is not pure ASCII", file=sys.stderr)
        return 1

    open(out_path, "w", encoding="ascii").write(out)

    unused = sorted(set(ICONS) - used)
    print(f"wrote {out_path}: {len(out.splitlines())} lines, {len(used)} icons used")
    if unused:
        print("unused icon names:", unused)
    return 0


if __name__ == "__main__":
    sys.exit(main())
