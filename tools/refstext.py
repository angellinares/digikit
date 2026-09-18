"""Extract the reference manuals once, as per-page text with a table of contents.

    uv run --with pymupdf python tools/refstext.py [PDF ...] [--out out/refs]
    uv run --with pymupdf python tools/refstext.py --render PDF PAGE [--dpi 150]

With no PDF arguments it takes every docs/refs/*.pdf. For each PDF it writes,
under OUT/<pdf stem>/:

- pages/p0001.txt ...: `pdftotext -layout` text, one file per PDF page (the
  physical page number, as a PDF viewer counts);
- all.txt: every page, each preceded by a line `=== page N ===`, for grep -n;
- toc.md: the PDF's bookmarks, indented by level, each with its page;
- meta.json: title, page count, the source's size and SHA-256.

OUT/INDEX.md lists every extracted document. A PDF whose SHA-256 matches
meta.json is skipped. --render writes one page as OUT/<stem>/png/p0001.png,
for figures the text does not carry.

The manuals are their publishers' copyright: OUT stays out of git.
"""

import argparse
import glob
import hashlib
import json
import os
import subprocess
import sys


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


def extract(pdf, out):
    import fitz  # PyMuPDF

    stem = os.path.splitext(os.path.basename(pdf))[0]
    dest = os.path.join(out, stem)
    meta_path = os.path.join(dest, 'meta.json')
    digest = sha256(pdf)
    if os.path.exists(meta_path):
        with open(meta_path) as f:
            if json.load(f).get('sha256') == digest:
                return stem, None

    text = subprocess.run(['pdftotext', '-layout', pdf, '-'], check=True,
                          capture_output=True).stdout.decode('utf-8', 'replace')
    pages = text.split('\f')
    doc = fitz.open(pdf)
    count = doc.page_count
    if len(pages) and not pages[-1].strip():
        pages = pages[:-1]
    if len(pages) != count:
        print(f'{stem}: pdftotext gave {len(pages)} pages, the PDF has {count}', file=sys.stderr)

    os.makedirs(os.path.join(dest, 'pages'), exist_ok=True)
    with open(os.path.join(dest, 'all.txt'), 'w') as whole:
        for n, page in enumerate(pages, 1):
            with open(os.path.join(dest, 'pages', f'p{n:04d}.txt'), 'w') as f:
                f.write(page)
            whole.write(f'=== page {n} ===\n{page}\n')

    toc = doc.get_toc()
    with open(os.path.join(dest, 'toc.md'), 'w') as f:
        f.write(f'# {doc.metadata.get("title") or stem}\n\n')
        if not toc:
            f.write('(the PDF has no bookmarks)\n')
        for level, title, page in toc:
            f.write(f'{"  " * (level - 1)}- {title.strip()} (page {page})\n')

    meta = {'source': pdf, 'title': doc.metadata.get('title') or '', 'pages': count,
            'text_pages': len(pages), 'toc_entries': len(toc),
            'bytes': os.path.getsize(pdf), 'sha256': digest}
    with open(meta_path, 'w') as f:
        json.dump(meta, f, indent=1)
    return stem, meta


def write_index(out):
    rows = []
    for meta_path in sorted(glob.glob(os.path.join(out, '*', 'meta.json'))):
        with open(meta_path) as f:
            meta = json.load(f)
        stem = os.path.basename(os.path.dirname(meta_path))
        rows.append(f'| `{stem}` | {meta["title"]} | {meta["pages"]} | {meta["toc_entries"]} |')
    with open(os.path.join(out, 'INDEX.md'), 'w') as f:
        f.write('# Extracted reference manuals\n\n'
                'Text per page in `<stem>/pages/pNNNN.txt` (PDF page numbers), '
                'all pages in `<stem>/all.txt`, bookmarks in `<stem>/toc.md`.\n\n'
                '| stem | title | pages | bookmarks |\n|---|---|---|---|\n')
        f.write('\n'.join(rows) + '\n')


def render(pdf, page, out, dpi):
    import fitz

    stem = os.path.splitext(os.path.basename(pdf))[0]
    dest = os.path.join(out, stem, 'png')
    os.makedirs(dest, exist_ok=True)
    doc = fitz.open(pdf)
    path = os.path.join(dest, f'p{page:04d}.png')
    doc[page - 1].get_pixmap(dpi=dpi).save(path)
    return path


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument('pdfs', nargs='*')
    ap.add_argument('--out', default='out/refs')
    ap.add_argument('--render', nargs=2, metavar=('PDF', 'PAGE'))
    ap.add_argument('--dpi', type=int, default=150)
    args = ap.parse_args(argv)

    if args.render:
        print(render(args.render[0], int(args.render[1]), args.out, args.dpi))
        return 0
    pdfs = args.pdfs or sorted(glob.glob('docs/refs/*.pdf'))
    for pdf in pdfs:
        stem, meta = extract(pdf, args.out)
        if meta is None:
            print(f'{stem}: unchanged')
        else:
            print(f'{stem}: {meta["pages"]} pages, {meta["toc_entries"]} bookmarks')
    write_index(args.out)
    return 0


if __name__ == '__main__':
    sys.exit(main())
