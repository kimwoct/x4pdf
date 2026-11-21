use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use image::GrayImage;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::model::{Document, ImageBox, Page, Run};

#[derive(Debug, Default, Clone)]
pub struct PdfMeta {
    pub title: Option<String>,
    pub author: Option<String>,
}

/// Fail early with an actionable message if the Poppler helpers are missing.
pub fn ensure_tools() -> Result<()> {
    for tool in ["pdfinfo", "pdftohtml", "pdftoppm"] {
        match Command::new(tool).arg("-v").output() {
            Ok(_) => {}
            Err(_) => bail!("missing `{tool}` (Poppler). Install it with: brew install poppler"),
        }
    }
    Ok(())
}

pub fn extract(input: &Path, work: &Path) -> Result<Document> {
    let xml_path = work.join("doc.xml");
    let output = Command::new("pdftohtml")
        .args(["-xml", "-c", "-s", "-nodrm", "-q"])
        .arg(input)
        .arg(&xml_path)
        .output()
        .with_context(|| "running pdftohtml")?;
    if !output.status.success() {
        bail!(
            "pdftohtml failed for {}: {}",
            input.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let raw =
        fs::read_to_string(&xml_path).with_context(|| format!("reading {}", xml_path.display()))?;
    let mut pages = parse_xml(&raw).with_context(|| "parsing pdftohtml XML")?;
    if pages.is_empty() {
        bail!("no pages extracted from {}", input.display());
    }

    let meta = pdfinfo(input).unwrap_or_default();
    let title = meta
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| {
            input
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Untitled".to_string())
        });

    // pdftohtml numbers pages from 1; make sure the numbering is stable.
    for (index, page) in pages.iter_mut().enumerate() {
        page.number = index + 1;
        page.runs.retain(|r| !r.text.is_empty());
    }

    Ok(Document {
        source: input.to_path_buf(),
        title,
        author: meta.author.filter(|a| !a.trim().is_empty()),
        pages,
    })
}

fn pdfinfo(input: &Path) -> Result<PdfMeta> {
    let output = Command::new("pdfinfo")
        .arg(input)
        .output()
        .with_context(|| "running pdfinfo")?;
    if !output.status.success() {
        bail!("pdfinfo failed for {}", input.display());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut meta = PdfMeta::default();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            match key.trim() {
                "Title" if !value.is_empty() => meta.title = Some(value.to_string()),
                "Author" if !value.is_empty() => meta.author = Some(value.to_string()),
                _ => {}
            }
        }
    }
    Ok(meta)
}

#[derive(Debug)]
struct PendingText {
    top: f32,
    left: f32,
    width: f32,
    height: f32,
    font_id: String,
    bold: bool,
    italic: bool,
    text: String,
}

fn parse_xml(raw: &str) -> Result<Vec<Page>> {
    let mut reader = Reader::from_str(raw);
    reader.config_mut().trim_text(false);
    reader.config_mut().expand_empty_elements = false;

    let mut pages = Vec::new();
    let mut current: Option<Page> = None;
    let mut fonts: HashMap<String, (f32, String)> = HashMap::new();
    let mut pending: Option<PendingText> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"page" => {
                    current = Some(Page {
                        number: number_attr(&e, b"number").unwrap_or(1),
                        width: float_attr(&e, b"width").unwrap_or(595.0),
                        height: float_attr(&e, b"height").unwrap_or(842.0),
                        runs: Vec::new(),
                        images: Vec::new(),
                    });
                }
                b"text" => {
                    pending = Some(PendingText {
                        top: float_attr(&e, b"top").unwrap_or(0.0),
                        left: float_attr(&e, b"left").unwrap_or(0.0),
                        width: float_attr(&e, b"width").unwrap_or(0.0),
                        height: float_attr(&e, b"height").unwrap_or(0.0),
                        font_id: string_attr(&e, b"font").unwrap_or_default(),
                        bold: false,
                        italic: false,
                        text: String::new(),
                    });
                }
                b"b" => {
                    if let Some(p) = pending.as_mut() {
                        p.bold = true;
                    }
                }
                b"i" => {
                    if let Some(p) = pending.as_mut() {
                        p.italic = true;
                    }
                }
                b"fontspec" => {
                    let id = string_attr(&e, b"id").unwrap_or_default();
                    let size = float_attr(&e, b"size").unwrap_or(0.0);
                    let family = string_attr(&e, b"family").unwrap_or_default();
                    fonts.insert(id, (size, family));
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"fontspec" => {
                    let id = string_attr(&e, b"id").unwrap_or_default();
                    let size = float_attr(&e, b"size").unwrap_or(0.0);
                    let family = string_attr(&e, b"family").unwrap_or_default();
                    fonts.insert(id, (size, family));
                }
                b"image" => {
                    if let Some(page) = current.as_mut() {
                        let width = float_attr(&e, b"width").unwrap_or(0.0);
                        let height = float_attr(&e, b"height").unwrap_or(0.0);
                        if width > 0.0 && height > 0.0 {
                            page.images.push(ImageBox {
                                top: float_attr(&e, b"top").unwrap_or(0.0),
                                left: float_attr(&e, b"left").unwrap_or(0.0),
                                width,
                                height,
                            });
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if let Some(p) = pending.as_mut() {
                    p.text.push_str(&e.unescape().unwrap_or_default());
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"text" => {
                    if let (Some(page), Some(p)) = (current.as_mut(), pending.take()) {
                        let (font_size, family) = fonts
                            .get(&p.font_id)
                            .cloned()
                            .unwrap_or((p.height * 0.75, String::new()));
                        page.runs.push(Run {
                            top: p.top,
                            left: p.left,
                            width: p.width,
                            height: p.height,
                            font_size: if font_size > 0.5 {
                                font_size
                            } else {
                                p.height * 0.75
                            },
                            family,
                            text: p.text,
                            bold: p.bold,
                            italic: p.italic,
                        });
                    }
                }
                b"b" | b"i" => {}
                b"page" => {
                    if let Some(page) = current.take() {
                        pages.push(page);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => bail!("XML error at byte {}: {e}", reader.buffer_position()),
            _ => {}
        }
    }

    Ok(pages)
}

fn string_attr(e: &BytesStart<'_>, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn float_attr(e: &BytesStart<'_>, key: &[u8]) -> Option<f32> {
    string_attr(e, key).and_then(|v| v.trim().parse::<f32>().ok())
}

fn number_attr(e: &BytesStart<'_>, key: &[u8]) -> Option<usize> {
    string_attr(e, key).and_then(|v| v.trim().parse::<usize>().ok())
}

/// Renders PDF pages on demand and caches them for the length of one conversion.
pub struct Renderer {
    input: PathBuf,
    work: PathBuf,
    dpi: u32,
    page_width_pt: f32,
    cache: HashMap<usize, Rc<GrayImage>>,
}

impl Renderer {
    pub fn new(input: &Path, work: &Path, target_width_px: u32, page_width_pt: f32) -> Self {
        // 3x supersampling, capped so a single page stays below ~4096 px.
        let desired = (target_width_px as f32 * 3.0) / page_width_pt * 72.0;
        let dpi = desired.clamp(150.0, 300.0).round() as u32;
        Self {
            input: input.to_path_buf(),
            work: work.to_path_buf(),
            dpi,
            page_width_pt,
            cache: HashMap::new(),
        }
    }

    pub fn dpi(&self) -> u32 {
        self.dpi
    }

    pub fn page(&mut self, page: usize) -> Result<Rc<GrayImage>> {
        if let Some(img) = self.cache.get(&page) {
            return Ok(Rc::clone(img));
        }
        let prefix = self.work.join(format!("render_{page:04}"));
        let output = Command::new("pdftoppm")
            .args([
                "-f",
                &page.to_string(),
                "-l",
                &page.to_string(),
                "-r",
                &self.dpi.to_string(),
                "-gray",
                "-png",
                "-singlefile",
            ])
            .arg(&self.input)
            .arg(&prefix)
            .output()
            .with_context(|| format!("rendering page {page}"))?;
        if !output.status.success() {
            bail!(
                "pdftoppm failed on page {page}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let path = prefix.with_extension("png");
        let image = image::open(&path)
            .with_context(|| format!("opening rendered page {}", path.display()))?
            .to_luma8();
        let rc = Rc::new(image);
        self.cache.insert(page, Rc::clone(&rc));
        Ok(rc)
    }

    /// Width in pixels of the rendered page. Useful to map XML coordinates.
    pub fn render_scale(&self, page_width_xml: f32) -> f32 {
        (self.page_width_pt * self.dpi as f32 / 72.0) / page_width_xml
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fontspecs_persist_across_pages() {
        // pdftohtml only emits <fontspec> when the font set changes, so page 2
        // must inherit page 1's font map instead of losing it.
        let xml = r#"<?xml version="1.0"?>
<pdf2xml>
<page number="1" width="612" height="792">
<fontspec id="0" size="16" family="SourceCodePro"/>
<text top="10" left="10" width="100" height="16" font="0">fn main() {</text>
</page>
<page number="2" width="612" height="792">
<text top="10" left="10" width="100" height="16" font="0">let x = 1;</text>
</page>
</pdf2xml>"#;
        let pages = parse_xml(xml).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[1].runs[0].family, "SourceCodePro");
        assert!(pages[1].runs[0].is_mono());
    }
}
