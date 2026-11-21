use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::layout::{Block, PageLayout};
use crate::model::Document;
use crate::util::{underline_for, wrap_text};

pub fn write(
    doc: &Document,
    layouts: &[PageLayout],
    orientation: &str,
    wrap_width: usize,
    out: &Path,
) -> Result<u64> {
    let mut text = String::new();
    text.push_str(&doc.title);
    text.push('\n');
    text.push_str(&underline_for(&doc.title));
    text.push_str("\n\n");
    text.push_str(&format!("Source: {}\n", doc.source.display()));
    text.push_str(&format!(
        "Xteink X4C text edition · {orientation} · images are listed, not embedded\n\n"
    ));

    for layout in layouts {
        text.push_str(&format!("\n[Page {}]\n\n", layout.number));
        for block in &layout.blocks {
            match block {
                Block::Heading {
                    level,
                    text: heading,
                    ..
                } => {
                    text.push('\n');
                    text.push_str(heading);
                    text.push('\n');
                    if *level <= 2 {
                        text.push_str(&underline_for(heading));
                        text.push('\n');
                    }
                    text.push('\n');
                }
                Block::Para {
                    text: paragraph,
                    list,
                    ..
                } => {
                    let prefix = if *list { "" } else { "" };
                    for line in wrap_text(paragraph, wrap_width) {
                        text.push_str(prefix);
                        text.push_str(&line);
                        text.push('\n');
                    }
                    text.push('\n');
                }
                Block::Code { text: code, .. } => {
                    for line in code.lines() {
                        text.push_str("    ");
                        text.push_str(line);
                        text.push('\n');
                    }
                    text.push('\n');
                }
                Block::Figure { spec, .. } => {
                    if spec.full_page {
                        text.push_str(&format!("[Page image {}]\n\n", spec.page));
                    } else if let Some(caption) = &spec.caption {
                        text.push_str(&format!("[Figure p{}: {}]\n\n", spec.page, caption));
                    } else {
                        text.push_str(&format!("[Image p{}]\n\n", spec.page));
                    }
                }
            }
        }
    }

    fs::write(out, text.as_bytes()).with_context(|| format!("writing {}", out.display()))?;
    let bytes = fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    Ok(bytes)
}
