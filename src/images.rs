use std::path::Path;

use anyhow::{bail, Context, Result};
use image::codecs::png::PngEncoder;
use image::{GrayImage, ImageEncoder};

use crate::layout::FigureSpec;
use crate::model::{Dither, Document};
use crate::pdf::Renderer;

pub struct FigureOptions {
    pub content_width: u32,
    pub max_height: u32,
    pub dither: Dither,
}

pub struct Slice {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

pub struct RenderedFigure {
    pub slices: Vec<Slice>,
    pub caption: Option<String>,
}

impl RenderedFigure {
    pub fn parts(&self) -> usize {
        self.slices.len()
    }
}

pub fn render_figure(
    doc: &Document,
    renderer: &mut Renderer,
    spec: &FigureSpec,
    options: &FigureOptions,
    stem: &str,
) -> Result<RenderedFigure> {
    let page = doc
        .page(spec.page)
        .with_context(|| format!("page {} missing", spec.page))?;
    let rendered = renderer.page(page.number)?;

    let scale_x = rendered.width() as f32 / page.width.max(1.0);
    let scale_y = rendered.height() as f32 / page.height.max(1.0);

    let x = (spec.bbox.left * scale_x).floor().max(0.0) as u32;
    let y = (spec.bbox.top * scale_y).floor().max(0.0) as u32;
    let w = (spec.bbox.width * scale_x).ceil() as u32;
    let h = (spec.bbox.height * scale_y).ceil() as u32;
    let w = w.min(rendered.width().saturating_sub(x)).max(1);
    let h = h.min(rendered.height().saturating_sub(y)).max(1);
    if w < 8 || h < 8 {
        bail!("figure crop too small on page {}", spec.page);
    }

    let crop = image::imageops::crop_imm(rendered.as_ref(), x, y, w, h).to_image();
    let display_w = if spec.full_page {
        options.content_width
    } else {
        ((spec.bbox.width / page.width) * options.content_width as f32).round() as u32
    }
    .clamp(80, options.content_width);
    let display_h = ((crop.height() as f32 / crop.width() as f32) * display_w as f32)
        .round()
        .max(1.0) as u32;

    let resized = image::imageops::resize(
        &crop,
        display_w,
        display_h,
        image::imageops::FilterType::Lanczos3,
    );

    let max_h = options.max_height.max(64);
    let overlap = 16u32;
    let slices: Vec<GrayImage> = if resized.height() <= max_h {
        vec![resized]
    } else {
        let step = max_h.saturating_sub(overlap).max(32);
        let mut parts = Vec::new();
        let mut y0 = 0u32;
        while y0 < resized.height() {
            let y1 = (y0 + max_h).min(resized.height());
            let part =
                image::imageops::crop_imm(&resized, 0, y0, resized.width(), y1 - y0).to_image();
            parts.push(part);
            if y1 >= resized.height() {
                break;
            }
            y0 += step;
        }
        parts
    };

    let total = slices.len();
    let mut out = Vec::with_capacity(total);
    for (index, slice) in slices.into_iter().enumerate() {
        let prepared = prepare_grayscale(slice, options.dither);
        let name = if total > 1 {
            format!("{stem}_{}.png", index + 1)
        } else {
            format!("{stem}.png")
        };
        let png = encode_png(&prepared)?;
        out.push(Slice {
            name,
            width: prepared.width(),
            height: prepared.height(),
            png,
        });
    }

    Ok(RenderedFigure {
        slices: out,
        caption: spec.caption.clone(),
    })
}

fn encode_png(image: &GrayImage) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    PngEncoder::new(&mut data)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::L8,
        )
        .context("encoding PNG")?;
    Ok(data)
}

fn prepare_grayscale(image: GrayImage, dither: Dither) -> GrayImage {
    let use_fs = match dither {
        Dither::Fs => true,
        Dither::None => false,
        Dither::Auto => !looks_flat(&image),
    };
    if use_fs {
        quantize_fs(image, 16)
    } else {
        quantize_flat(image, 16)
    }
}

/// UI screenshots are dominated by a handful of tones; photos are not.
/// Dithering a screenshot adds speckle around small text, so skip it there.
fn looks_flat(image: &GrayImage) -> bool {
    let mut histogram = [0usize; 256];
    let total = (image.width() * image.height()) as usize;
    if total == 0 {
        return true;
    }
    for pixel in image.pixels() {
        histogram[pixel[0] as usize] += 1;
    }
    let mut bins = histogram;
    bins.sort_unstable_by(|a, b| b.cmp(a));
    let top: usize = bins.iter().take(4).sum();
    // A screenshot with a white page, black text and a few greys is usually >0.9.
    (top as f32 / total as f32) > 0.88
}

fn quantize_flat(mut image: GrayImage, levels: u16) -> GrayImage {
    let step = 255.0 / (levels - 1) as f32;
    for pixel in image.pixels_mut() {
        let q = (pixel[0] as f32 / step).round() * step;
        pixel[0] = q.clamp(0.0, 255.0) as u8;
    }
    image
}

fn quantize_fs(image: GrayImage, levels: u16) -> GrayImage {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width == 0 || height == 0 {
        return image;
    }
    let mut buffer: Vec<f32> = image.pixels().map(|p| p[0] as f32).collect();
    let step = 255.0 / (levels - 1) as f32;

    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let old = buffer[index];
            let new = ((old / step).round() * step).clamp(0.0, 255.0);
            let error = old - new;
            buffer[index] = new;

            if x + 1 < width {
                buffer[index + 1] += error * 7.0 / 16.0;
            }
            if y + 1 < height {
                let below = index + width;
                if x > 0 {
                    buffer[below - 1] += error * 3.0 / 16.0;
                }
                buffer[below] += error * 5.0 / 16.0;
                if x + 1 < width {
                    buffer[below + 1] += error * 1.0 / 16.0;
                }
            }
        }
    }

    let data: Vec<u8> = buffer
        .into_iter()
        .map(|v| v.clamp(0.0, 255.0) as u8)
        .collect();
    GrayImage::from_raw(image.width(), image.height(), data)
        .unwrap_or_else(|| GrayImage::new(image.width(), image.height()))
}

/// Used by the cover builder: render a whole page without slicing.
pub fn render_cover(
    doc: &Document,
    renderer: &mut Renderer,
    page_number: usize,
    width: u32,
    max_height: u32,
    dither: Dither,
    path: &Path,
) -> Result<()> {
    let page = doc
        .page(page_number)
        .with_context(|| format!("page {page_number} missing"))?;
    let rendered = renderer.page(page.number)?;
    let display_h =
        ((rendered.height() as f32 / rendered.width() as f32) * width as f32).round() as u32;
    let resized = image::imageops::resize(
        rendered.as_ref(),
        width,
        display_h.min(max_height),
        image::imageops::FilterType::Lanczos3,
    );
    let prepared = prepare_grayscale(resized, dither);
    let png = encode_png(&prepared)?;
    std::fs::write(path, png).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
