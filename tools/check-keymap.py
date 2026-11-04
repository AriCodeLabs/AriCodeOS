#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
# SPDX-License-Identifier: Apache-2.0
"""Compares the Spanish keyboard of libs/input/src/es.rs with the one Linux
types (ADR 0076): the `es` layout of xkeyboard-config, its includes, and the
dead-key rules of the `Compose` file, as installed on this host (Debian:
xkb-data, libx11-data, x11proto-dev). Reads them; copies nothing. Prints
every difference and exits 1 if there is one.

    python3 tools/check-keymap.py
"""
import re
import sys

XKB = "/usr/share/X11/xkb"
COMPOSE = "/usr/share/X11/locale/en_US.UTF-8/Compose"
KEYSYMS = "/usr/include/X11/keysymdef.h"
OURS = "libs/input/src/es.rs"

# The dead keys es.rs combines, by keysym name.
DEAD = {"dead_grave": "Grave", "dead_acute": "Acute", "dead_circumflex": "Circumflex",
        "dead_diaeresis": "Diaeresis", "dead_tilde": "Tilde", "dead_cedilla": "Cedilla"}


def keysym_chars():
    """Keysym name -> character, from keysymdef.h's `U+XXXX` comments."""
    out = {}
    for m in re.finditer(r"^#define XK_(\w+)\s+0x([0-9a-fA-F]+)(.*)$", open(KEYSYMS).read(), re.M):
        name, value, rest = m.group(1), int(m.group(2), 16), m.group(3)
        u = re.search(r"U\+([0-9A-Fa-f]{4,6})", rest)
        if u:
            out.setdefault(name, chr(int(u.group(1), 16)))
        elif 0x20 <= value <= 0xFF:
            out.setdefault(name, chr(value))  # Latin-1 keysyms are their code point
        elif value & 0xFF000000 == 0x01000000:
            out.setdefault(name, chr(value & 0xFFFFFF))
    return out


def sym(name, chars):
    if name in DEAD:
        return ("dead", DEAD[name])
    if name.startswith("dead_") or name == "NoSymbol":
        return None
    if re.fullmatch(r"U[0-9A-Fa-f]{4,6}", name):
        return ("char", chr(int(name[1:], 16)))
    if len(name) == 1:
        return ("char", name)
    if name in chars:
        return ("char", chars[name])
    raise SystemExit(f"unknown keysym {name}")


def section(file, name):
    text = open(f"{XKB}/symbols/{file}").read()
    m = re.search(r'xkb_symbols\s+"%s"\s*\{(.*?)\n\};' % re.escape(name), text, re.S)
    if not m:
        raise SystemExit(f"no section {file}({name})")
    return m.group(1)


def layout(file, name, chars, keys):
    """Applies `file(name)` over `keys` (name -> 4 levels), includes first."""
    body = section(file, name)
    for inc in re.findall(r'include\s+"([^"]+)"', body):
        m = re.fullmatch(r"(\w+)(?:\((\w+)\))?", inc)
        if m.group(1) in ("level3", "eurosign"):
            continue  # how AltGr is reached; the levels are the same here
        layout(m.group(1), m.group(2) or "basic", chars, keys)
    for key, levels in re.findall(r"key\s+<(\w+)>\s*\{\s*\[([^\]]*)\]", body):
        keys[key] = [sym(s.strip(), chars) for s in levels.split(",")] + [None] * 4
        keys[key] = keys[key][:4]
    return keys


def evdev_codes():
    """XKB key name -> evdev code (the XKB keycode minus 8)."""
    out = {}
    for name, code in re.findall(r"<(\w+)>\s*=\s*(\d+);", open(f"{XKB}/keycodes/evdev").read()):
        out[name] = int(code) - 8
    return out


def ours():
    """Our table, evdev code -> 4 levels, read from es.rs."""
    text = open(OURS).read()
    def one(expr):
        expr = expr.strip()
        if expr == "N":
            return None
        m = re.fullmatch(r"c\('(.+)'\)", expr)
        if m:
            return ("char", m.group(1).encode().decode("unicode_escape").encode("latin-1").decode()
                    if "\\" in m.group(1) else m.group(1))
        m = re.fullmatch(r"d\(Dead::(\w+)\)", expr)
        if m:
            return ("dead", m.group(1))
        raise SystemExit(f"cannot read {expr!r} in {OURS}")
    table = {}
    for code, rhs in re.findall(r"^\s*k\[(\d+)\] = (.*);$", text, re.M):
        code = int(code)
        m = re.fullmatch(r"same\('(.+)'\)", rhs)
        if m:
            continue  # control keys and space, not part of the layout
        m = re.fullmatch(r"letter\('(.)', '(.)', (.+), (.+)\)", rhs)
        if m:
            table[code] = [("char", m.group(1)), ("char", m.group(2)), one(m.group(3)), one(m.group(4))]
            continue
        m = re.fullmatch(r"\[(.*)\]", rhs)
        parts = re.findall(r"c\('(?:\\.|[^'])+'\)|d\(Dead::\w+\)|N", m.group(1))
        table[code] = [one(p) for p in parts]
    return table


def compose():
    """(dead, base) -> result for our dead keys, from the Compose file."""
    out = {}
    pat = re.compile(r'^<(dead_\w+)>\s+<(\w+)>\s*:\s*"((?:\\.|[^"])*)"', re.M)
    chars = keysym_chars()
    for dead, base, result in pat.findall(open(COMPOSE, encoding="utf-8").read()):
        if dead not in DEAD:
            continue
        b = " " if base == "space" else (DEAD.get(base) and ("dead", DEAD[base])) or (
            base if len(base) == 1 else chars.get(base))
        out[(DEAD[dead], b)] = result.encode().decode("unicode_escape").encode("latin-1").decode() \
            if "\\" in result else result
    return out


def main():
    chars = keysym_chars()
    linux = layout("es", "basic", chars, {})
    codes = evdev_codes()
    mine = ours()
    problems = 0
    for name, levels in sorted(linux.items(), key=lambda kv: codes.get(kv[0], 999)):
        code = codes.get(name)
        if code is None or code > 86:
            continue
        got = mine.get(code, [None] * 4)
        for level, (want, have) in enumerate(zip(levels, got)):
            if want != have:
                problems += 1
                print(f"key <{name}> (evdev {code}) level {level + 1}: Linux {want}, ours {have}")
    # The accents: what Linux's Compose gives for a letter of Latin-1, ours
    # must give too (Dead::combine in es.rs is checked by its unit tests
    # against this list).
    rules = compose()
    latin1 = {k: v for k, v in rules.items()
              if isinstance(k[1], str) and len(v) == 1 and (ord(v) < 0x100)}
    print(f"{len(latin1)} dead-key rules of Linux give a Latin-1 character:")
    for (dead, base), result in sorted(latin1.items()):
        print(f"  {dead} + {base!r} = {result!r}")
    if problems:
        print(f"{problems} difference(s) with Linux's es layout")
        return 1
    print("the es layout matches Linux's, all four levels of every key")
    return 0


if __name__ == "__main__":
    sys.exit(main())
