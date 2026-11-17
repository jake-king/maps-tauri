#!/usr/bin/env bash
# The documentation site: the book (book/, mdBook) at the root and the API reference (rustdoc, every
# crate) under api/, as the book's last chapter links it. Both build with warnings fatal, as in the
# gate, and every link in the book resolves. Writes target/site/ and, with --archive, target/maps-tauri-docs.tar.gz.
#   tools/docs.sh [--archive]
set -euo pipefail
cd "$(dirname "$0")/.."
# cargo-installed tools (mdbook) live in cargo's bin, which a login shell may not have on PATH
export PATH=${CARGO_HOME:-$HOME/.cargo}/bin:$PATH
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked -q
# book.toml sets create-missing = false, so a chapter SUMMARY.md names but nobody wrote fails here
# rather than being created empty
mdbook build book
rm -rf target/site
mkdir -p target/site
cp -r book/book/. target/site/
# mdBook's 404 page carries <base href="/">: right only for a server with the book at its root, wrong
# for this site opened from the archive or served under a path
rm -f target/site/404.html
cp -r target/doc target/site/api
# the test coverage report, when tools/coverage.sh --html target/coverage has made one (CI and every
# release do); otherwise a page that says how to make it, so the book's link always resolves
if [[ -f target/coverage/index.html ]]; then
  cp -r target/coverage target/site/coverage
else
  mkdir -p target/site/coverage
  printf '%s\n' '<!doctype html><title>Coverage</title><p>Not built here: run <code>tools/coverage.sh --html target/coverage</code>, then <code>tools/docs.sh</code>.</p>' >target/site/coverage/index.html
fi
# every API page the book links to must exist in what was built
for crate in coords vtile wx maps_tauri_lib; do
  [[ -f target/site/api/$crate/index.html ]] || { echo "docs: no API reference for $crate" >&2; exit 1; }
done
# mdBook 0.5 does not check links (it exits 0 over a broken one): every relative link must resolve
python3 tools/linkcheck.py target/site
if [[ ${1:-} == --archive ]]; then
  tar -C target/site -czf target/maps-tauri-docs.tar.gz .
  echo "docs: target/maps-tauri-docs.tar.gz ($(du -h target/maps-tauri-docs.tar.gz | cut -f1))"
else
  echo "docs: target/site/index.html"
fi
