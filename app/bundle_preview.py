#!/usr/bin/env python3
"""Bundle app/ui into one self-contained HTML page for previewing the UI in a
browser or sharing it: stylesheets and scripts inlined, fonts as data: URIs.

    python3 app/bundle_preview.py out.html             # a complete HTML document
    python3 app/bundle_preview.py out.html --fragment  # body content only, for hosts that add their own <head>

The page runs on demo data (window.__TAURI__ is absent), so nothing it does
touches a disk or a stick.
"""
import argparse
import base64
import os
import re

UI = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ui")


def read(rel):
    with open(os.path.join(UI, rel), encoding="utf-8") as f:
        return f.read()


def inline_fonts(css):
    def repl(m):
        with open(os.path.join(UI, m.group(1)), "rb") as f:
            data = base64.b64encode(f.read()).decode()
        return f'url("data:font/woff2;base64,{data}")'

    return re.sub(r'url\("(fonts/[^"]+\.woff2)"\)', repl, css)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--fragment", action="store_true")
    ap.add_argument("--title", default="Fatstick Prototype")
    a = ap.parse_args()

    html = read("index.html")
    css = "\n".join(inline_fonts(read(h)) for h in re.findall(r'<link rel="stylesheet" href="([^"]+)">', html))
    scripts = re.findall(r'<script src="([^"]+)"></script>', html)
    js = "\n".join(read(s) for s in scripts)
    body = re.search(r"<body>(.*?)\n\s*<script", html, re.S).group(1)
    # Framed: a side gutter around the window, for pages that aren't a desktop window.
    boot = "document.documentElement.classList.add('framed');"
    head = (
        f"<title>{a.title}</title>\n"
        '<meta name="claude:viewport-fit-sides" content="cover">\n'
        f"<style>\n{css}\n</style>\n<script>{boot}</script>\n"
    )
    page = f"{head}{body.strip()}\n<script>\n{js}\n</script>\n"
    if not a.fragment:
        page = (
            '<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
            '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n'
            f"{head}</head>\n<body>\n{body.strip()}\n<script>\n{js}\n</script>\n</body>\n</html>\n"
        )
    with open(a.out, "w", encoding="utf-8") as f:
        f.write(page)
    print(f"{a.out}: {len(page.encode()) / 1024:.0f} KiB, {len(scripts)} scripts inlined")


if __name__ == "__main__":
    main()
