"""Every relative link and image in the book's built pages resolves: the file exists in the site
(book + API reference, as tools/docs.sh assembles it) and, for a `#fragment`, the target page has an
element with that id. mdBook 0.5 checks none of this and exits 0 over a broken link.

    python3 tools/linkcheck.py SITE_DIR      exit 0 all resolve · 1 any does not · 2 usage

Only the book's pages are scanned (not api/, whose links rustdoc's own lints check).
"""

import html.parser
import os
import sys
import urllib.parse


class Page(html.parser.HTMLParser):
    def __init__(self):
        super().__init__()
        self.links, self.ids = [], set()

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        if a.get("id"):
            self.ids.add(a["id"])
        for key in ("href", "src"):
            if a.get(key):
                self.links.append(a[key])


PAGES = {}


def parse(path):
    if path not in PAGES:
        p = Page()
        with open(path, encoding="utf-8") as f:
            p.feed(f.read())
        PAGES[path] = p
    return PAGES[path]


def main():
    if len(sys.argv) != 2 or not os.path.isdir(sys.argv[1]):
        print(__doc__, file=sys.stderr)
        return 2
    site = os.path.realpath(sys.argv[1])
    bad, pages, links = [], 0, 0
    for root, dirs, files in os.walk(site):
        if os.path.relpath(root, site).split(os.sep)[0] == "api":
            dirs[:] = []
            continue
        for name in files:
            if not name.endswith(".html"):
                continue
            page = os.path.join(root, name)
            pages += 1
            for link in parse(page).links:
                u = urllib.parse.urlsplit(link)
                if u.scheme or u.netloc or link.startswith(("mailto:", "javascript:")):
                    continue
                links += 1
                target = (
                    page
                    if not u.path
                    else os.path.normpath(
                        os.path.join(root, urllib.parse.unquote(u.path))
                    )
                )
                if os.path.isdir(target):
                    target = os.path.join(target, "index.html")
                where = os.path.relpath(page, site)
                if not target.startswith(site + os.sep) or not os.path.isfile(target):
                    bad.append(f"{where}: {link} (no such file)")
                elif (
                    u.fragment
                    and target.endswith(".html")
                    and u.fragment not in parse(target).ids
                ):
                    bad.append(f"{where}: {link} (no element with that id)")
    for b in bad:
        print(f"linkcheck: {b}")
    print(f"linkcheck: {pages} pages, {links} relative links, {len(bad)} broken")
    return 1 if bad or pages == 0 else 0


if __name__ == "__main__":
    sys.exit(main())
