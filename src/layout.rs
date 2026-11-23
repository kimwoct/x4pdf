use std::collections::{HashMap, HashSet};

use crate::model::{Document, Page, Run};
use crate::util::{char_width, escape, is_cjk, leading_spaces};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn right(self) -> f32 {
        self.left + self.width
    }
    pub fn bottom(self) -> f32 {
        self.top + self.height
    }
    pub fn area(self) -> f32 {
        self.width * self.height
    }
    pub fn union(self, other: Rect) -> Rect {
        let left = self.left.min(other.left);
        let top = self.top.min(other.top);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect {
            top,
            left,
            width: right - left,
            height: bottom - top,
        }
    }
    fn overlaps_x(self, other: Rect, min_fraction: f32) -> bool {
        let overlap = self.right().min(other.right()) - self.left.max(other.left);
        overlap > 0.0 && overlap / self.width.min(other.width).max(1.0) >= min_fraction
    }
    fn iou(self, other: Rect) -> f32 {
        let ix = (self.right().min(other.right()) - self.left.max(other.left)).max(0.0);
        let iy = (self.bottom().min(other.bottom()) - self.top.max(other.top)).max(0.0);
        let inter = ix * iy;
        let union = self.area() + other.area() - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }
}

#[derive(Debug, Clone)]
pub struct FigureSpec {
    pub page: usize,
    pub bbox: Rect,
    pub caption: Option<String>,
    pub full_page: bool,
    pub index: usize,
}

#[derive(Debug, Clone)]
pub enum Block {
    Heading {
        level: u8,
        text: String,
        html: String,
        top: f32,
    },
    Para {
        text: String,
        html: String,
        top: f32,
        list: bool,
    },
    Code {
        text: String,
        top: f32,
    },
    Figure {
        spec: FigureSpec,
        top: f32,
    },
}

impl Block {
    pub fn top(&self) -> f32 {
        match self {
            Block::Heading { top, .. }
            | Block::Para { top, .. }
            | Block::Code { top, .. }
            | Block::Figure { top, .. } => *top,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PageStats {
    pub chars: usize,
    pub images: usize,
    pub figures: usize,
    pub code_blocks: usize,
    pub rasterized: bool,
}

#[derive(Debug, Clone)]
pub struct PageLayout {
    pub number: usize,
    pub blocks: Vec<Block>,
    pub title: String,
    pub stats: PageStats,
}

#[derive(Debug, Clone)]
struct Line {
    top: f32,
    left: f32,
    right: f32,
    height: f32,
    size: f32,
    text: String,
    html: String,
    mono_ratio: f32,
    bold_ratio: f32,
}

impl Line {
    fn bottom(&self) -> f32 {
        self.top + self.height
    }
    fn chars(&self) -> usize {
        self.text.chars().filter(|c| !c.is_whitespace()).count()
    }
}

pub fn paginate(doc: &Document, code_columns: usize) -> Vec<PageLayout> {
    let mut all_lines: Vec<Vec<Line>> = doc.pages.iter().map(build_lines).collect();
    let boilerplate = find_boilerplate(&doc.pages, &all_lines);

    for (page, lines) in doc.pages.iter().zip(all_lines.iter_mut()) {
        lines.retain(|line| {
            let trimmed = line.text.trim();
            if trimmed.is_empty() {
                return false;
            }
            let in_band = line.top < page.height * 0.07 || line.bottom() > page.height * 0.90;
            if in_band {
                if trimmed.chars().all(|c| c.is_ascii_digit()) {
                    return false;
                }
                if boilerplate.contains(&normalize(trimmed)) {
                    return false;
                }
            }
            true
        });
    }

    let repeated = image_dimension_counts(&doc.pages);
    let page_count = doc.pages.len().max(1);

    doc.pages
        .iter()
        .zip(all_lines.into_iter())
        .map(|(page, lines)| build_page(page, lines, &repeated, page_count, code_columns))
        .collect()
}

/// One page image per PDF page, for comics, magazines, and fixed layouts that
/// fall apart when text is reflowed. With `split_spreads`, a landscape page is
/// cut into left and right book pages.
pub fn rasterize_pages(doc: &Document, split_spreads: bool) -> Vec<PageLayout> {
    let mut layouts = Vec::new();
    let mut book_page = 0usize;

    for page in &doc.pages {
        let spread = split_spreads && page.width > page.height * 1.15;
        let halves = if spread { 2 } else { 1 };
        for half in 0..halves {
            book_page += 1;
            let (left, width) = if spread {
                (page.width / 2.0 * half as f32, page.width / 2.0)
            } else {
                (0.0, page.width)
            };
            layouts.push(PageLayout {
                number: book_page,
                blocks: vec![Block::Figure {
                    top: 0.0,
                    spec: FigureSpec {
                        page: page.number,
                        bbox: Rect {
                            top: 0.0,
                            left,
                            width,
                            height: page.height,
                        },
                        caption: None,
                        full_page: true,
                        index: half + 1,
                    },
                }],
                title: format!("Page {book_page}"),
                stats: PageStats {
                    chars: 0,
                    images: page.images.len(),
                    figures: 1,
                    code_blocks: 0,
                    rasterized: true,
                },
            });
        }
    }

    layouts
}

fn build_page(
    page: &Page,
    lines: Vec<Line>,
    repeated: &HashMap<(i32, i32), usize>,
    page_count: usize,
    code_columns: usize,
) -> PageLayout {
    let chars: usize = lines.iter().map(|l| l.chars()).sum();
    let (body_size, body_height) = body_metrics(&lines);
    if std::env::var_os("X4PDF_DEBUG").is_some() {
        eprintln!(
            "page {}: {} runs, {} lines, {} chars, body {:.1}/{:.1}",
            page.number,
            page.runs.len(),
            lines.len(),
            chars,
            body_size,
            body_height
        );
    }

    let largest_image_area = page
        .images
        .iter()
        .map(|i| i.width * i.height)
        .fold(0.0f32, f32::max);
    let page_area = (page.width * page.height).max(1.0);
    let rasterize_page = lines.is_empty() || (chars < 30 && largest_image_area / page_area > 0.30);

    if rasterize_page && !page.images.is_empty() {
        let spec = FigureSpec {
            page: page.number,
            bbox: Rect {
                top: 0.0,
                left: 0.0,
                width: page.width,
                height: page.height,
            },
            caption: None,
            full_page: true,
            index: 1,
        };
        return PageLayout {
            number: page.number,
            blocks: vec![Block::Figure { top: 0.0, spec }],
            title: format!("Page {}", page.number),
            stats: PageStats {
                chars,
                images: page.images.len(),
                figures: 1,
                code_blocks: 0,
                rasterized: true,
            },
        };
    }

    let mut consumed: HashSet<usize> = HashSet::new();
    let figures = select_figures(
        page,
        &lines,
        repeated,
        page_count,
        body_height,
        &mut consumed,
    );
    let figure_tops: Vec<f32> = figures.iter().map(|f| f.bbox.top).collect();
    let crosses_figure = |top: f32, bottom: f32| {
        figure_tops
            .iter()
            .any(|f| *f > top - 2.0 && *f < bottom + 2.0)
    };

    let mut blocks = Vec::new();
    let mut code_blocks = 0usize;
    let mut i = 0usize;

    while i < lines.len() {
        if consumed.contains(&i) {
            i += 1;
            continue;
        }
        let line = &lines[i];

        if let Some(level) = heading_level(line, body_size) {
            let mut heading = line.text.trim().to_string();
            let mut heading_html = line.html.trim().to_string();
            let mut j = i + 1;
            while j < lines.len() {
                let next = &lines[j];
                if heading_level(next, body_size) != Some(level) {
                    break;
                }
                if (next.size - line.size).abs() > 0.6 {
                    break;
                }
                if (next.left - line.left).abs() > body_size * 0.8 {
                    break;
                }
                if next.top - lines[j - 1].bottom() > body_height * 1.35 {
                    break;
                }
                if heading.ends_with(['.', '?', '!', ':', ';']) {
                    break;
                }
                heading.push(' ');
                heading.push_str(next.text.trim());
                heading_html.push(' ');
                heading_html.push_str(next.html.trim());
                j += 1;
            }
            blocks.push(Block::Heading {
                level,
                text: heading,
                html: heading_html,
                top: line.top,
            });
            i = j;
            continue;
        }

        if is_code_line(line, body_size) {
            let mut code_lines: Vec<String> = vec![line.text.trim_end().to_string()];
            let mut last_bottom = line.bottom();
            let mut block_top = line.top;
            let code_h = line.height.max(8.0);
            let mut j = i + 1;
            while j < lines.len() {
                if consumed.contains(&j) {
                    break;
                }
                let candidate = &lines[j];
                if !is_code_line(candidate, body_size) {
                    break;
                }
                if candidate.top < last_bottom - 2.0 {
                    break;
                }
                if candidate.top - last_bottom > code_h * 2.5 {
                    break;
                }
                if crosses_figure(last_bottom, candidate.top) {
                    break;
                }
                let gap = candidate.top - last_bottom;
                if gap > code_h * 1.45 {
                    code_lines.push(String::new());
                }
                code_lines.push(candidate.text.trim_end().to_string());
                last_bottom = candidate.bottom();
                block_top = block_top.min(candidate.top);
                j += 1;
            }

            // The design only treats two or more consecutive fixed-pitch lines as a
            // code block; a lone mono line is usually inline code in prose.
            if code_lines
                .iter()
                .filter(|line| !line.trim().is_empty())
                .count()
                < 2
            {
                // Fall through to paragraph handling.
            } else {
                // Remove only the shared layout indent; relative indentation survives.
                let min_indent = code_lines
                    .iter()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| leading_spaces(l))
                    .min()
                    .unwrap_or(0);
                let normalized: Vec<String> = code_lines
                    .iter()
                    .map(|l| {
                        if l.len() >= min_indent && l.chars().take(min_indent).all(|c| c == ' ') {
                            l[min_indent..].to_string()
                        } else {
                            l.clone()
                        }
                    })
                    .collect();
                let mut wrapped: Vec<String> = Vec::new();
                for code_line in &normalized {
                    if code_line.is_empty() {
                        wrapped.push(String::new());
                    } else {
                        wrapped.extend(crate::util::wrap_code_line(code_line, code_columns));
                    }
                }

                blocks.push(Block::Code {
                    text: wrapped.join("\n"),
                    top: block_top,
                });
                code_blocks += 1;
                i = j;
                continue;
            }
        }

        if is_list_item(&line.text) {
            blocks.push(Block::Para {
                text: line.text.trim().to_string(),
                html: escape(line.text.trim()),
                top: line.top,
                list: true,
            });
            i += 1;
            continue;
        }

        // A body paragraph: merge following lines while the page layout says they belong together.
        let mut text = line.text.trim().to_string();
        let mut html = line.html.trim().to_string();
        let mut bottom = line.bottom();
        let first_left = line.left;
        let mut j = i + 1;

        while j < lines.len() {
            if consumed.contains(&j) {
                break;
            }
            let next = &lines[j];
            if heading_level(next, body_size).is_some()
                || is_code_line(next, body_size)
                || is_list_item(&next.text)
            {
                break;
            }
            if next.size < body_size * 0.82 || next.size > body_size * 1.16 {
                break;
            }
            if (next.left - first_left).abs() > body_size * 1.3 {
                break;
            }
            let gap = next.top - bottom;
            if gap < -2.0 || gap > body_height * 1.65 + 4.0 {
                break;
            }
            if crosses_figure(bottom, next.top) {
                break;
            }

            let next_text = next.text.trim();
            let dehyphenate = text.ends_with('-')
                && next_text
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_lowercase())
                    .unwrap_or(false);
            let cjk_join = text.chars().last().map(is_cjk).unwrap_or(false)
                && next_text.chars().next().map(is_cjk).unwrap_or(false);

            if dehyphenate {
                text.pop();
                if html.ends_with('-') {
                    html.pop();
                }
                text.push_str(next_text);
                html.push_str(next.html.trim());
            } else if cjk_join {
                text.push_str(next_text);
                html.push_str(next.html.trim());
            } else {
                text.push(' ');
                text.push_str(next_text);
                html.push(' ');
                html.push_str(next.html.trim());
            }
            bottom = next.bottom();
            j += 1;
        }

        blocks.push(Block::Para {
            text,
            html,
            top: line.top,
            list: false,
        });
        i = j;
    }

    for figure in &figures {
        blocks.push(Block::Figure {
            top: figure.bbox.top,
            spec: figure.clone(),
        });
    }
    blocks.sort_by(|a, b| a.top().total_cmp(&b.top()));

    let title = blocks
        .iter()
        .find_map(|b| match b {
            Block::Heading { level, text, .. } if *level <= 2 => {
                Some(crate::util::truncate_chars(text, 60))
            }
            _ => None,
        })
        .unwrap_or_else(|| format!("Page {}", page.number));

    PageLayout {
        number: page.number,
        blocks,
        title,
        stats: PageStats {
            chars,
            images: page.images.len(),
            figures: figures.len(),
            code_blocks,
            rasterized: false,
        },
    }
}

fn build_lines(page: &Page) -> Vec<Line> {
    let mut runs: Vec<&Run> = page.runs.iter().collect();
    runs.sort_by(|a, b| {
        a.top
            .total_cmp(&b.top)
            .then_with(|| a.left.total_cmp(&b.left))
    });

    let mut groups: Vec<Vec<&Run>> = Vec::new();
    for run in runs {
        if let Some(last) = groups.last_mut() {
            let top = last.iter().map(|r| r.top).fold(f32::INFINITY, f32::min);
            let left = last.iter().map(|r| r.left).fold(f32::INFINITY, f32::min);
            let right = last
                .iter()
                .map(|r| r.left + r.width)
                .fold(f32::NEG_INFINITY, f32::max);
            let same_line = (run.top - top).abs() <= 8.0;
            let not_column_gap = run.left <= right + page.width * 0.02
                && run.left + run.width >= left - page.width * 0.02;
            if same_line && not_column_gap {
                last.push(run);
                continue;
            }
        }
        groups.push(vec![run]);
    }

    let groups = merge_close_groups(groups, (page.width * 0.02).max(8.0));

    let mut lines = Vec::new();
    for group in groups {
        let mut group: Vec<Run> = group.into_iter().cloned().collect();
        group.sort_by(|a, b| a.left.total_cmp(&b.left));
        if let Some(line) = make_line(&group) {
            lines.push(line);
        }
    }
    lines.sort_by(|a, b| {
        a.top
            .total_cmp(&b.top)
            .then_with(|| a.left.total_cmp(&b.left))
    });
    lines
}

/// Merge fragments on the same baseline when only a small horizontal gap
/// separates them. This keeps inline code such as `get-dependencies` inside
/// the surrounding sentence instead of turning it into its own paragraph.
fn merge_close_groups<'a>(mut groups: Vec<Vec<&'a Run>>, max_gap: f32) -> Vec<Vec<&'a Run>> {
    let bounds = |group: &[&Run]| {
        let top = group.iter().map(|r| r.top).fold(f32::INFINITY, f32::min);
        let left = group.iter().map(|r| r.left).fold(f32::INFINITY, f32::min);
        let right = group
            .iter()
            .map(|r| r.left + r.width)
            .fold(f32::NEG_INFINITY, f32::max);
        (top, left, right)
    };

    let mut changed = true;
    while changed {
        changed = false;
        'outer: for i in 0..groups.len() {
            let (ti, li, ri) = bounds(&groups[i]);
            for j in (i + 1)..groups.len() {
                let (tj, lj, rj) = bounds(&groups[j]);
                if (ti - tj).abs() > 8.0 {
                    continue;
                }
                let gap = if lj > ri {
                    lj - ri
                } else if rj < li {
                    li - rj
                } else {
                    0.0
                };
                if gap <= max_gap {
                    let merged = groups[i].iter().chain(groups[j].iter()).copied().collect();
                    groups[i] = merged;
                    groups.remove(j);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    groups
}

fn make_line(runs: &[Run]) -> Option<Line> {
    if runs.is_empty() {
        return None;
    }
    let top = runs.iter().map(|r| r.top).fold(f32::INFINITY, f32::min);
    let left = runs.iter().map(|r| r.left).fold(f32::INFINITY, f32::min);
    let right = runs
        .iter()
        .map(|r| r.left + r.width)
        .fold(f32::NEG_INFINITY, f32::max);
    let height = runs.iter().map(|r| r.height).fold(0.0f32, f32::max);

    let mut text = String::new();
    let mut html = String::new();
    let mut prev_right: Option<f32> = None;
    let mut mono_chars = 0usize;
    let mut bold_chars = 0usize;
    let mut total_chars = 0usize;
    let mut size_sum = 0.0f32;
    let mut size_weight = 0.0f32;

    for run in runs {
        let raw = run.text.as_str();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            if !text.is_empty() && !text.ends_with(' ') {
                text.push(' ');
                html.push(' ');
            }
            continue;
        }

        if let Some(prev) = prev_right {
            let gap = run.left - prev;
            if !text.is_empty() && gap > 2.0 && !text.ends_with(' ') && !raw.starts_with(' ') {
                text.push(' ');
                html.push(' ');
            }
        }

        text.push_str(raw);
        let mut h = escape(raw);
        if run.is_mono() {
            h = format!("<code>{h}</code>");
        }
        if run.is_bold() {
            h = format!("<strong>{h}</strong>");
        }
        if run.italic {
            h = format!("<em>{h}</em>");
        }
        html.push_str(&h);

        let count = trimmed.chars().count();
        total_chars += count;
        if run.is_mono() {
            mono_chars += count;
        }
        if run.is_bold() {
            bold_chars += count;
        }
        size_sum += run.font_size * count as f32;
        size_weight += count as f32;
        prev_right = Some(run.left + run.width);
    }

    let text = text.trim_end().to_string();
    if text.trim().is_empty() {
        return None;
    }
    let size = if size_weight > 0.0 {
        size_sum / size_weight
    } else {
        height * 0.75
    };
    let mono_ratio = if total_chars > 0 {
        mono_chars as f32 / total_chars as f32
    } else {
        0.0
    };
    let bold_ratio = if total_chars > 0 {
        bold_chars as f32 / total_chars as f32
    } else {
        0.0
    };

    Some(Line {
        top,
        left,
        right,
        height,
        size,
        text,
        html: html.trim_end().to_string(),
        mono_ratio,
        bold_ratio,
    })
}

fn body_metrics(lines: &[Line]) -> (f32, f32) {
    let mut sizes: Vec<(f32, f32)> = lines
        .iter()
        .filter(|l| l.mono_ratio < 0.5 && l.chars() >= 3)
        .map(|l| (l.size, l.chars() as f32))
        .collect();
    if sizes.is_empty() {
        return (16.0, 20.0);
    }
    sizes.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f32 = sizes.iter().map(|s| s.1).sum();
    let mut acc = 0.0;
    let mut body = sizes[sizes.len() / 2].0;
    for (size, weight) in &sizes {
        acc += *weight;
        if acc >= total / 2.0 {
            body = *size;
            break;
        }
    }

    let mut heights: Vec<f32> = lines
        .iter()
        .filter(|l| l.mono_ratio < 0.5 && (l.size - body).abs() <= body * 0.18)
        .map(|l| l.height)
        .collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    let body_height = heights
        .get(heights.len() / 2)
        .copied()
        .unwrap_or(body * 1.3);
    (body.max(1.0), body_height.max(1.0))
}

fn heading_level(line: &Line, body: f32) -> Option<u8> {
    if line.mono_ratio > 0.6 || line.text.trim().chars().count() > 180 {
        return None;
    }
    let ratio = line.size / body;
    if ratio >= 1.55 {
        Some(1)
    } else if ratio >= 1.28 {
        Some(2)
    } else if ratio >= 1.14 && line.bold_ratio >= 0.5 {
        Some(3)
    } else {
        None
    }
}

fn is_code_line(line: &Line, body: f32) -> bool {
    let chars = line.chars();
    if chars == 0 {
        return false;
    }
    let lead = leading_spaces(&line.text);
    line.mono_ratio >= 0.8 && line.size <= body * 1.12 && (lead >= 2 || line.mono_ratio >= 0.98)
}

fn is_list_item(text: &str) -> bool {
    let t = text.trim_start();
    if t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("•")
        || t.starts_with("·")
        || t.starts_with("◦")
    {
        return true;
    }
    let mut chars = t.chars().peekable();
    let mut digits = 0;
    while let Some(c) = chars.peek() {
        if c.is_ascii_digit() {
            digits += 1;
            chars.next();
        } else {
            break;
        }
    }
    if digits == 0 || digits > 3 {
        return false;
    }
    matches!(chars.next(), Some('.') | Some(')'))
        && chars.next().map(|c| c == ' ' || c == '\t').unwrap_or(true)
}

fn select_figures(
    page: &Page,
    lines: &[Line],
    repeated: &HashMap<(i32, i32), usize>,
    page_count: usize,
    body_height: f32,
    consumed: &mut HashSet<usize>,
) -> Vec<FigureSpec> {
    let page_area = (page.width * page.height).max(1.0);
    let mut candidates: Vec<Rect> = Vec::new();

    for image in &page.images {
        let rect = Rect {
            top: image.top,
            left: image.left,
            width: image.width,
            height: image.height,
        };
        let area_frac = rect.area() / page_area;
        let width_frac = rect.width / page.width;
        let height_frac = rect.height / page.height;
        if area_frac < 0.02 || width_frac < 0.12 || height_frac < 0.04 {
            continue;
        }
        if area_frac > 0.90 {
            continue;
        }
        let key = (rect.width.round() as i32, rect.height.round() as i32);
        let count = repeated.get(&key).copied().unwrap_or(0);
        let is_repeat = count >= 4 && count as f32 >= page_count as f32 * 0.35 && area_frac < 0.12;
        if is_repeat {
            continue;
        }
        candidates.push(rect);
    }

    candidates.sort_by(|a, b| {
        a.top
            .total_cmp(&b.top)
            .then_with(|| a.left.total_cmp(&b.left))
    });

    // Merge stacked slices of one screenshot (same width, tiny vertical gap).
    let mut merged: Vec<Rect> = Vec::new();
    for rect in candidates {
        if let Some(last) = merged.last_mut() {
            let similar_width =
                (last.width - rect.width).abs() <= last.width.max(rect.width) * 0.18;
            let gap = rect.top - last.bottom();
            if similar_width && gap >= -2.0 && gap <= 14.0 && last.overlaps_x(rect, 0.5) {
                *last = last.union(rect);
                continue;
            }
        }
        merged.push(rect);
    }

    let mut deduped: Vec<Rect> = Vec::new();
    for rect in merged {
        if deduped.iter().any(|r| r.iou(rect) > 0.8) {
            continue;
        }
        deduped.push(rect);
    }

    let mut figures = Vec::new();
    for (index, bbox) in deduped.into_iter().enumerate() {
        let caption = find_caption(lines, bbox, body_height).map(|(line_index, text)| {
            consumed.insert(line_index);
            text
        });
        figures.push(FigureSpec {
            page: page.number,
            bbox,
            caption,
            full_page: false,
            index: index + 1,
        });
    }
    figures
}

fn find_caption(lines: &[Line], bbox: Rect, body_height: f32) -> Option<(usize, String)> {
    let prefixes = [
        "figure",
        "fig.",
        "table",
        "screenshot",
        "diagram",
        "example:",
        "圖",
        "图",
        "表",
        "截圖",
        "截图",
    ];
    for (index, line) in lines.iter().enumerate() {
        let gap = line.top - bbox.bottom();
        if !(0.0..=48.0).contains(&gap) {
            continue;
        }
        if line.height > body_height * 1.15 {
            continue;
        }
        let text = line.text.trim();
        if text.chars().count() > 220 {
            continue;
        }
        let lower = text.to_ascii_lowercase();
        if prefixes.iter().any(|p| lower.starts_with(p)) {
            return Some((index, text.to_string()));
        }
    }
    None
}

fn find_boilerplate(pages: &[Page], all_lines: &[Vec<Line>]) -> HashSet<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (page, lines) in pages.iter().zip(all_lines.iter()) {
        for line in lines {
            let in_band = line.top < page.height * 0.07 || line.bottom() > page.height * 0.90;
            if !in_band {
                continue;
            }
            let text = line.text.trim();
            if text.is_empty() || text.chars().count() > 100 {
                continue;
            }
            *counts.entry(normalize(text)).or_default() += 1;
        }
    }
    let threshold = ((pages.len() as f32) * 0.3).ceil().max(3.0) as usize;
    counts
        .into_iter()
        .filter(|(_, count)| *count >= threshold)
        .map(|(text, _)| text)
        .collect()
}

fn normalize(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&word.to_lowercase());
    }
    out
}

fn image_dimension_counts(pages: &[Page]) -> HashMap<(i32, i32), usize> {
    let mut counts: HashMap<(i32, i32), usize> = HashMap::new();
    for page in pages {
        for image in &page.images {
            let key = (image.width.round() as i32, image.height.round() as i32);
            *counts.entry(key).or_default() += 1;
        }
    }
    counts
}

// Keep this helper near the code that uses the same width logic.
#[allow(dead_code)]
fn text_columns(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn page(number: usize, width: f32, height: f32) -> Page {
        Page {
            number,
            width,
            height,
            runs: Vec::new(),
            images: Vec::new(),
        }
    }

    #[test]
    fn rasterize_splits_landscape_pages() {
        let doc = Document {
            source: PathBuf::from("test.pdf"),
            title: "Test".to_string(),
            author: None,
            pages: vec![
                page(1, 800.0, 1200.0),
                page(2, 1200.0, 800.0),
                page(3, 800.0, 1200.0),
            ],
        };
        let layouts = rasterize_pages(&doc, true);
        assert_eq!(layouts.len(), 4);
        assert_eq!(layouts[0].number, 1);
        assert_eq!(layouts[1].number, 2);
        assert_eq!(layouts[2].number, 3);
        assert_eq!(layouts[3].number, 4);

        match &layouts[1].blocks[0] {
            Block::Figure { spec, .. } => {
                assert_eq!(spec.page, 2);
                assert_eq!(spec.bbox.left, 0.0);
                assert_eq!(spec.bbox.width, 600.0);
                assert_eq!(spec.index, 1);
            }
            _ => panic!("expected a figure"),
        }
        match &layouts[2].blocks[0] {
            Block::Figure { spec, .. } => {
                assert_eq!(spec.page, 2);
                assert_eq!(spec.bbox.left, 600.0);
                assert_eq!(spec.bbox.width, 600.0);
                assert_eq!(spec.index, 2);
            }
            _ => panic!("expected a figure"),
        }
    }
}
