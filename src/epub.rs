use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::images::{self, FigureOptions};
use crate::layout::{Block, PageLayout};
use crate::model::{Dither, Document, Orientation};
use crate::pdf::Renderer;
use crate::util::{escape, truncate_chars};

pub struct EpubOptions {
    pub title: String,
    pub author: Option<String>,
    pub language: String,
    pub orientation: Orientation,
    pub dither: Dither,
    pub content_width: u32,
    pub max_image_height: u32,
}

pub struct EpubSummary {
    pub chapters: usize,
    pub images: usize,
    pub bytes: u64,
}

struct Chapter<'a> {
    pages: Vec<&'a PageLayout>,
    title: String,
}

struct Asset {
    href: String,
    id: String,
    data: Vec<u8>,
}

pub fn write(
    doc: &Document,
    layouts: &[PageLayout],
    renderer: &mut Renderer,
    options: &EpubOptions,
    out: &Path,
) -> Result<EpubSummary> {
    let chapters = group_chapters(layouts);
    let figure_options = FigureOptions {
        content_width: options.content_width,
        max_height: options.max_image_height,
        dither: options.dither,
    };

    let mut assets: Vec<Asset> = Vec::new();
    let mut chapter_files: Vec<(String, String, String, String)> = Vec::new();

    for (chapter_index, chapter) in chapters.iter().enumerate() {
        let file = format!("chapter_{:04}.xhtml", chapter_index + 1);
        let id = format!("ch{:04}", chapter_index + 1);
        let mut body = String::new();

        // Figma "01 Prose": hairline, uppercase chapter kicker, then the heading.
        body.push_str("<hr class=\"rule\"/>\n");
        body.push_str(&format!(
            "<p class=\"kicker\">CHAPTER {:02}</p>\n",
            chapter_index + 1
        ));

        for page in &chapter.pages {
            body.push_str(&format!(
                "<div class=\"pdfpage\" id=\"p{}\">\n",
                page.number
            ));
            for block in &page.blocks {
                match block {
                    Block::Heading { level, html, .. } => {
                        let level = (*level).clamp(1, 4);
                        body.push_str(&format!("<h{level}>{html}</h{level}>\n"));
                    }
                    Block::Para { html, list, .. } => {
                        if *list {
                            body.push_str(&format!("<p class=\"li\">{html}</p>\n"));
                        } else {
                            body.push_str(&format!("<p>{html}</p>\n"));
                        }
                    }
                    Block::Code { text, .. } => {
                        body.push_str("<pre><code>");
                        body.push_str(&escape(text));
                        body.push_str("</code></pre>\n");
                    }
                    Block::Figure { spec, .. } => {
                        let stem = format!("f{:04}_{:02}", spec.page, spec.index);
                        let rendered =
                            images::render_figure(doc, renderer, spec, &figure_options, &stem)
                                .with_context(|| {
                                    format!("rendering figure {} on page {}", spec.index, spec.page)
                                })?;
                        let total = rendered.parts();
                        let alt = escape(
                            rendered
                                .caption
                                .as_deref()
                                .unwrap_or(&format!("Figure on page {}", spec.page)),
                        );
                        for (part_index, slice) in rendered.slices.iter().enumerate() {
                            let href = format!("images/{}", slice.name);
                            let asset_id = format!("img_{:05}", assets.len() + 1);
                            assets.push(Asset {
                                href: href.clone(),
                                id: asset_id,
                                data: slice.png.clone(),
                            });
                            body.push_str("<div class=\"fig\">");
                            body.push_str(&format!(
                                "<img src=\"{}\" alt=\"{}\" width=\"{}\" height=\"{}\"/>",
                                href, alt, slice.width, slice.height
                            ));
                            if total > 1 {
                                body.push_str(&format!(
                                    "<p class=\"cap\">part {}/{}</p>",
                                    part_index + 1,
                                    total
                                ));
                            }
                            body.push_str("</div>\n");
                        }
                        if let Some(caption) = &rendered.caption {
                            body.push_str(&format!("<p class=\"cap\">{}</p>\n", escape(caption)));
                        }
                    }
                }
            }
            body.push_str("</div>\n");
        }

        chapter_files.push((file, id, chapter.title.clone(), body));
    }

    let mut files: Vec<(String, Vec<u8>, bool)> = Vec::new();
    files.push((
        "META-INF/container.xml".to_string(),
        b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n<rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles>\n</container>\n".to_vec(),
        false,
    ));
    let css = stylesheet(options.orientation);
    files.push(("OEBPS/style.css".to_string(), css.into_bytes(), false));

    let cover = cover_xhtml(doc, layouts.len(), options);
    files.push(("OEBPS/cover.xhtml".to_string(), cover.into_bytes(), false));

    for (file, _, title, body) in &chapter_files {
        let xhtml = page_xhtml(title, body, &options.language, options.orientation);
        files.push((format!("OEBPS/{file}"), xhtml.into_bytes(), false));
    }

    let nav = nav_xhtml(&chapters, options);
    files.push(("OEBPS/nav.xhtml".to_string(), nav.into_bytes(), false));

    for asset in &assets {
        files.push((format!("OEBPS/{}", asset.href), asset.data.clone(), true));
    }

    let opf = content_opf(doc, &chapters, &assets, options);
    files.push(("OEBPS/content.opf".to_string(), opf.into_bytes(), false));

    // mimetype must be the first entry and must be stored uncompressed.
    let file = fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
    let mut zip = ZipWriter::new(file);
    let stored = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o644);
    zip.start_file("mimetype", stored)?;
    zip.write_all(b"application/epub+zip")?;

    files.sort_by(|a, b| {
        // Keep the package document early; chapters and images can follow.
        let rank = |name: &str| {
            if name.ends_with("content.opf") {
                0
            } else if name.ends_with("nav.xhtml") {
                1
            } else if name.ends_with("cover.xhtml") {
                2
            } else if name.ends_with("style.css") {
                3
            } else {
                4
            }
        };
        rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0))
    });

    for (name, data, image) in files {
        let method = if image {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let opts = SimpleFileOptions::default()
            .compression_method(method)
            .unix_permissions(0o644);
        zip.start_file(name, opts)?;
        zip.write_all(&data)?;
    }
    zip.finish()?;

    let bytes = fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    Ok(EpubSummary {
        chapters: chapters.len(),
        images: assets.len(),
        bytes,
    })
}

fn group_chapters(layouts: &[PageLayout]) -> Vec<Chapter<'_>> {
    let mut chapters: Vec<Chapter> = Vec::new();
    let mut current: Vec<&PageLayout> = Vec::new();

    for page in layouts {
        let starts_chapter = page
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Heading { level: 1, .. }));
        let too_long = current.len() >= 20;
        let too_big = current
            .iter()
            .map(|p| estimate_page_bytes(p))
            .sum::<usize>()
            > 260_000;
        if !current.is_empty() && (starts_chapter || too_long || too_big) {
            flush_chapter(&mut current, &mut chapters);
        }
        current.push(page);
    }
    flush_chapter(&mut current, &mut chapters);
    chapters
}

fn flush_chapter<'a>(current: &mut Vec<&'a PageLayout>, chapters: &mut Vec<Chapter<'a>>) {
    if current.is_empty() {
        return;
    }
    let first = current.first().map(|p| p.number).unwrap_or(0);
    let last = current.last().map(|p| p.number).unwrap_or(0);
    let title = current
        .iter()
        .flat_map(|page| page.blocks.iter())
        .find_map(|block| match block {
            Block::Heading { level, text, .. } if *level <= 2 => Some(truncate_chars(text, 80)),
            _ => None,
        })
        .unwrap_or_else(|| {
            if first == last {
                format!("Page {first}")
            } else {
                format!("Pages {first}–{last}")
            }
        });
    chapters.push(Chapter {
        pages: std::mem::take(current),
        title,
    });
}

fn estimate_page_bytes(page: &PageLayout) -> usize {
    page.blocks
        .iter()
        .map(|block| match block {
            Block::Heading { html, .. } => html.len() + 20,
            Block::Para { html, .. } => html.len() + 20,
            Block::Code { text, .. } => text.len() + 40,
            Block::Figure { .. } => 220,
        })
        .sum()
}

fn page_xhtml(title: &str, body: &str, language: &str, orientation: Orientation) -> String {
    let _ = title;
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <!DOCTYPE html>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"{language}\" lang=\"{language}\">\n\
         <head>\n\
         <title>{}</title>\n\
         <link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n\
         <meta name=\"x4pdf:orientation\" content=\"{}\"/>\n\
         <meta name=\"x4pdf:canvas\" content=\"{}x{}\"/>\n\
         </head>\n\
         <body class=\"{}\">\n\
         {body}\
         </body>\n\
         </html>\n",
        escape(title),
        orientation.label(),
        orientation.canvas().0,
        orientation.canvas().1,
        orientation.label()
    )
}

fn nav_xhtml(chapters: &[Chapter<'_>], options: &EpubOptions) -> String {
    let mut items = String::new();
    for (index, chapter) in chapters.iter().enumerate() {
        items.push_str(&format!(
            "<li><a href=\"chapter_{:04}.xhtml\">{}</a></li>\n",
            index + 1,
            escape(&chapter.title)
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <!DOCTYPE html>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"{lang}\" lang=\"{lang}\">\n\
         <head><title>Contents</title><link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/></head>\n\
         <body class=\"{orientation}\">\n\
         <nav epub:type=\"toc\" id=\"toc\"><h1>Contents</h1><ol>\n{items}</ol></nav>\n\
         </body>\n</html>\n",
        lang = options.language,
        orientation = options.orientation.label(),
    )
}

fn cover_xhtml(doc: &Document, pages: usize, options: &EpubOptions) -> String {
    let (w, h) = options.orientation.canvas();
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <!DOCTYPE html>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"{lang}\" lang=\"{lang}\">\n\
         <head><title>{}</title><link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/></head>\n\
         <body class=\"{orientation}\">\n\
         <section class=\"cover\">\n\
         <p class=\"kicker\">Xteink X4C edition</p>\n\
         <h1>{}</h1>\n\
         <p class=\"meta\">{} pages · {}×{} {}</p>\n\
         <p class=\"meta\">Source: {}</p>\n\
         <p class=\"meta\">PDF layout reflowed for a 4.0-inch, 220 ppi E Ink screen.</p>\n\
         </section>\n\
         </body>\n</html>\n",
        escape(&doc.title),
        escape(&doc.title),
        pages,
        w,
        h,
        options.orientation.label(),
        escape(
            doc.source
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .as_deref()
                .unwrap_or("PDF")
        ),
        lang = options.language,
        orientation = options.orientation.label(),
    )
}

fn content_opf(
    doc: &Document,
    chapters: &[Chapter<'_>],
    assets: &[Asset],
    options: &EpubOptions,
) -> String {
    let mut manifest = String::new();
    manifest.push_str("<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n");
    manifest.push_str("<item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n");
    manifest.push_str(
        "<item id=\"cover\" href=\"cover.xhtml\" media-type=\"application/xhtml+xml\"/>\n",
    );
    for index in 0..chapters.len() {
        manifest.push_str(&format!(
            "<item id=\"ch{:04}\" href=\"chapter_{:04}.xhtml\" media-type=\"application/xhtml+xml\"/>\n",
            index + 1,
            index + 1
        ));
    }
    for asset in assets {
        manifest.push_str(&format!(
            "<item id=\"{}\" href=\"{}\" media-type=\"image/png\"/>\n",
            asset.id, asset.href
        ));
    }

    let mut spine = String::new();
    spine.push_str("<itemref idref=\"cover\"/>\n");
    for index in 0..chapters.len() {
        spine.push_str(&format!("<itemref idref=\"ch{:04}\"/>\n", index + 1));
    }

    let modified = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "2026-01-01T00:00:00Z".to_string());
    let identifier = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let author = doc
        .author
        .as_ref()
        .or(options.author.as_ref())
        .map(|a| format!("<dc:creator>{}</dc:creator>\n", escape(a)))
        .unwrap_or_default();

    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"bookid\" xml:lang=\"{lang}\">\n\
         <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
         <dc:identifier id=\"bookid\">{identifier}</dc:identifier>\n\
         <dc:title>{title}</dc:title>\n\
         <dc:language>{lang}</dc:language>\n\
         {author}<meta property=\"dcterms:modified\">{modified}</meta>\n\
         <meta property=\"rendition:layout\">reflowable</meta>\n\
         <meta property=\"rendition:orientation\">auto</meta>\n\
         <meta property=\"rendition:spread\">auto</meta>\n\
         </metadata>\n\
         <manifest>\n{manifest}</manifest>\n\
         <spine page-progression-direction=\"ltr\">\n{spine}</spine>\n\
         </package>\n",
        lang = options.language,
        title = escape(&options.title),
        identifier = identifier,
        author = author,
        modified = modified,
        manifest = manifest,
        spine = spine,
    )
}

fn stylesheet(orientation: Orientation) -> String {
    let (margin, max_height) = match orientation {
        Orientation::Portrait => (16, 704),
        Orientation::Landscape => (40, 430),
    };
    format!(
        r#"@page {{ margin: 0; }}
html, body {{ margin: 0; padding: 0; }}
body {{
  font-family: serif;
  font-size: 100%;
  line-height: 1.45;
  margin: 0 {margin}px;
  text-align: left;
  color: #222222;
  -webkit-hyphens: auto;
  hyphens: auto;
}}
body.landscape {{ margin: 0 40px; }}
p {{ margin: 0 0 0.6em 0; }}
.rule {{ border: 0; border-top: 1px solid #D6D3C8; margin: 0 0 0.55em 0; }}
.kicker {{
  font-family: sans-serif;
  font-size: 0.55em;
  line-height: 1.3;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: #6B6960;
  margin: 0 0 0.55em 0;
}}
.kicker + .pdfpage > h1:first-child {{ margin-top: 0.4em; }}
h1 {{ font-size: 1.55em; line-height: 1.19; color: #171717; margin: 0.9em 0 0.5em 0; }}
h2 {{ font-size: 1.25em; line-height: 1.2; color: #171717; margin: 0.8em 0 0.4em 0; }}
h3 {{ font-size: 1.1em; line-height: 1.27; color: #171717; margin: 0.7em 0 0.35em 0; }}
h4 {{ font-size: 1em; line-height: 1.3; font-weight: bold; color: #171717; margin: 0.6em 0 0.3em 0; }}
a {{ color: inherit; text-decoration: underline; }}
pre {{
  font-family: monospace;
  font-size: 0.8em;
  line-height: 1.35;
  margin: 0.5em 0;
  padding: 0.3em 0.5em 0.3em 0.25em;
  border-left: 4px solid #555;
  background: #EBE8DF;
  color: #1B1B1B;
  white-space: pre-wrap;
  overflow-wrap: break-word;
  hyphens: none;
  font-variant-ligatures: none;
  tab-size: 4;
}}
code {{ font-family: monospace; font-size: 0.88em; hyphens: none; font-variant-ligatures: none; }}
blockquote {{
  font-size: 0.95em;
  margin: 0.6em 0 0.6em 0.8em;
  padding-left: 0.8em;
  border-left: 4px solid #D6D3C8;
  color: #6B6960;
}}
ul, ol {{ margin: 0.4em 0 0.6em 1.2em; padding: 0; }}
li {{ margin: 0 0 0.25em 0; }}
.li {{ margin-left: 1.2em; text-indent: -1.2em; }}
img {{ max-width: 100%; height: auto; }}
.fig {{ margin: 0.7em 0; text-align: center; page-break-inside: avoid; break-inside: avoid; }}
.fig img {{ display: block; margin: 0 auto; max-height: {max_height}px; }}
.cap {{
  font-size: 0.8em;
  line-height: 1.375;
  font-style: italic;
  text-align: center;
  color: #6B6960;
  margin: 0.25em 0 0 0;
}}
.pdfpage + .pdfpage {{ margin-top: 1.2em; }}
.cover {{ margin-top: 12%; text-align: left; }}
.cover .kicker {{ color: #6B6960; margin-bottom: 1.4em; }}
.cover h1 {{ font-size: 1.9em; line-height: 1.2; color: #171717; margin: 0.2em 0 0.8em 0; }}
.cover .meta {{ font-size: 0.8em; line-height: 1.5; color: #6B6960; margin: 0.35em 0; }}
table {{ font-size: 0.85em; border-collapse: collapse; margin: 0.5em 0; }}
td, th {{ border: 1px solid #666; padding: 0.15em 0.35em; }}
th {{ font-weight: bold; }}
@media (min-width: 600px) {{
  body {{ margin: 0 40px; }}
  .fig img {{ max-height: 430px; }}
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stylesheet_carries_the_figma_tokens() {
        let css = stylesheet(Orientation::Portrait);
        for token in [
            "#EBE8DF", // code block background
            "#6B6960", // muted captions and kicker
            "#D6D3C8", // hairline
            "#171717", // ink headings
            "#222222", // body
            "1.55em",  // h1 31px at a 20px base
            "1.25em",  // h2 25px
            "border-left: 4px solid #555",
            "@media (min-width: 600px)",
        ] {
            assert!(css.contains(token), "stylesheet is missing {token}");
        }
    }

    #[test]
    fn portrait_and_landscape_figure_limits_match_the_design() {
        assert!(stylesheet(Orientation::Portrait).contains("max-height: 704px"));
        let landscape = stylesheet(Orientation::Landscape);
        assert!(landscape.contains("margin: 0 40px"));
        assert!(landscape.contains("max-height: 430px"));
    }
}
