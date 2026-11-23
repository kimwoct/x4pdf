# x4pdf

PDF → EPUB 3 / TXT for the **Xteink X4C** e-ink reader: 480×800 portrait, 800×480 landscape, 220 ppi, 16-level grayscale.

The Rust binary classifies each PDF page (prose, code, figures), reflows it, renders figures as 16-level grayscale PNGs, and writes a reflowable EPUB. A plain-text edition is available alongside it.

## Requirements

- macOS or Linux with **Poppler** on `PATH`:
  `brew install poppler` (macOS) or `apt install poppler-utils` (Debian/Ubuntu)
- Rust 1.75+

Poppler supplies `pdftohtml` (text and image boxes), `pdfinfo` (metadata), and `pdftoppm` (page rasters). The layout, wrapping, grayscale pipeline, and EPUB writer are Rust.

## Build and run

```bash
cargo build --release

# Portrait EPUB beside the PDF
./target/release/x4pdf book.pdf

# Both EPUB and TXT, landscape layout
./target/release/x4pdf book.pdf --format both --orientation landscape

# Inspect classification without writing
./target/release/x4pdf book.pdf --dry-run -v

# Choose where output goes
./target/release/x4pdf book.pdf -o out/book.epub --force
```

Options: `--format epub|txt|both`, `--orientation portrait|landscape`, `--font-pt 8–12.5`, `--dither auto|none|fs`, `--rasterize`, `--spread`, `--output`, `--title`, `--author`, `--lang`, `--wrap`, `--force`, `--dry-run`, `--keep-temp`, `-v`.

## Fixed layouts and spreads

For comics, magazines, and PDFs exported as two-page spreads, reflowing the text breaks the layout. Render each page as an image instead:

```bash
./target/release/x4pdf book.pdf --rasterize --spread
```

`--rasterize` keeps every page intact as a 16-level grayscale image. `--spread` (requires `--rasterize`) cuts a landscape page down the middle and emits the left and right book pages in reading order. Each output page is one XHTML document and one full 480×800 canvas, so the next page cannot appear in the bottom margin. The trade-off is no selectable text or search in that edition.

## Reading size

The Figma frame is a 480×800 mock with a 20 px body. On a 220 ppi panel that is only about **6.5 pt** — too small on the real device. The default output is **10.5 pt** (32 px at 220 ppi), adjustable with `--font-pt` from 8 to 12.5.

At 10.5 pt the body has roughly 28 characters per line, headings scale from the same base, and code/captions are 0.95 em (about 9.95 pt). Code lines are reflowed to the columns that physically fit: **23 at 10.5 pt**, 37 at a 20 px body. The 45-column rule in the original spec only applies to 16 px code at a 20 px body; keeping that at 10.5 pt would overflow the 480 px screen.

## Web app (mobile PWA)

The repo also ships a small local server that turns the converter into a phone-friendly PWA: upload a PDF, choose reflow or page-image mode, convert on the Mac, and download the EPUB.

```bash
cargo build --release
cargo build --release --bin x4pdf-web
./target/release/x4pdf-web --port 4820
```

Open `http://127.0.0.1:4820` on the Mac, or expose it to your tailnet:

```bash
tailscale serve --bg --https=443 http://127.0.0.1:4820
```

Then open `https://<machine>.<tailnet>.ts.net/`. On iPhone use **Share → Add to Home Screen**; on Android use **Install app**. The service worker caches the shell, while `/api/` stays network-only.

The server accepts one conversion at a time, limits uploads to 100 MB, and keeps finished files for one hour before deleting them. It binds `127.0.0.1`; Tailscale provides the HTTPS tailnet entry.

## Design mapping

Source of truth: the Figma file `pdf2x4c — X4C reading layouts`, page `X4C agent build` (frames 01–06).

| Element | Figma | EPUB CSS |
|---|---|---|
| Body | 20 px / 29 px at 100%, `#222222` | `32px / 1.45` default (10.5 pt) |
| H1 | 31 px / 37 px, `#171717` | `1.55em / 1.19` |
| H2 | 25 px / 30 px | `1.25em / 1.2` |
| H3 | 22 px / 28 px | `1.1em / 1.27` |
| Code | 16 px / 21.6 px, bg `#EBE8DF`, 4 px `#555` rule | `0.95em / 1.35`, `4px solid #555` |
| Caption | 16 px / 22 px italic, `#6B6960` | `0.95em / 1.375` |
| Kicker | uppercase sans, `#6B6960` | `.kicker`, `0.6em`, `0.12em` tracking |
| Hairline | `#D6D3C8` | `.rule`, 1 px |
| Margins | 16 px portrait / 40 px landscape | `body` margin + `@media (min-width: 600px)` |
| Figures | ≤ 448 × 704 px portrait, ≤ 720 × 430 px landscape | `max-height` + 3× supersample and Lanczos3 |
| Grayscale | 16 levels, flat UI never dithered | `auto`: nearest-level for flat images, Floyd–Steinberg for photos |

Code is wrapped to the measured column count with two-space continuation lines; tabs expand to four spaces. `pre` disables ligatures and hyphenation so code stays byte-faithful.

Each chapter opens with a hairline and an uppercase `CHAPTER NN` kicker, matching frames 01–04. The number is the generated EPUB chapter index; the app does not read the PDF outline, so it is not necessarily the printed chapter number. Page numbers and running headers are reader chrome, so they are not embedded in the reflowable EPUB.

The style sheet is responsive: a portrait export opened in landscape gets 40 px margins and a 430 px figure cap. `--orientation` selects the figure width and image budget at build time.

## Output

```
book.x4.epub
  mimetype
  META-INF/container.xml
  OEBPS/content.opf
  OEBPS/nav.xhtml
  OEBPS/style.css
  OEBPS/cover.xhtml
  OEBPS/chapter_0001.xhtml …
  OEBPS/images/f0001_01.png …
```

EPUB 3, reflowable, `orientation=auto`, no embedded fonts, no JavaScript, no fixed pixel containers.

## Tests

```bash
cargo test
cargo fmt --check
```

The tests pin the Figma tokens in the generated style sheet, the 10.5 pt default, and the code-column fit.
