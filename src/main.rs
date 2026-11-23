mod epub;
mod images;
mod layout;
mod model;
mod pdf;
mod textout;
mod util;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::Parser;

use crate::layout::PageLayout;
use crate::model::{Dither, Document, Format, Orientation};

#[derive(Debug, Parser)]
#[command(
    name = "x4pdf",
    version,
    about = "PDF → EPUB/TXT for the Xteink X4C (480×800, 16-level grayscale)"
)]
struct Cli {
    /// PDF files to convert (one or more).
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Output format.
    #[arg(short, long, value_enum, default_value_t = Format::Epub)]
    format: Format,

    /// Reader orientation: portrait (480×800) or landscape (800×480).
    #[arg(long, value_enum, default_value_t = Orientation::Portrait)]
    orientation: Orientation,

    /// Base reading font size in points on the 220 ppi panel (8–12.5).
    #[arg(long, default_value_t = 10.5)]
    font_pt: f32,

    /// Grayscale/dither policy for figures.
    #[arg(long, value_enum, default_value_t = Dither::Auto)]
    dither: Dither,

    /// Render every page as a full-page image (comics, magazines, complex layouts).
    #[arg(long)]
    rasterize: bool,

    /// Split landscape spreads into left/right pages (use with --rasterize).
    #[arg(long)]
    spread: bool,

    /// Output file, or output directory for several inputs.
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Override the EPUB title (single input only).
    #[arg(long)]
    title: Option<String>,

    /// Override the EPUB author.
    #[arg(long)]
    author: Option<String>,

    /// EPUB language tag (default: auto-detect Chinese/English).
    #[arg(long)]
    lang: Option<String>,

    /// Hard-wrap TXT paragraphs at N columns (0 = leave to the reader).
    #[arg(long, default_value_t = 0)]
    wrap: usize,

    /// Overwrite existing output files.
    #[arg(long)]
    force: bool,

    /// Parse and classify pages without writing output.
    #[arg(long)]
    dry_run: bool,

    /// Keep the temporary extraction/rendering directory.
    #[arg(long)]
    keep_temp: bool,

    /// Print per-page classification.
    #[arg(short, long)]
    verbose: bool,
}

struct Job {
    input: PathBuf,
    epub: Option<PathBuf>,
    txt: Option<PathBuf>,
}

struct Summary {
    pages: usize,
    figures: usize,
    rasterized: usize,
    code_blocks: usize,
    epub: Option<epub::EpubSummary>,
    txt_bytes: Option<u64>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    if !(8.0..=12.5).contains(&cli.font_pt) {
        bail!("--font-pt must be between 8 and 12.5");
    }
    if cli.spread && !cli.rasterize {
        bail!("--spread requires --rasterize");
    }
    pdf::ensure_tools()?;
    for input in &cli.inputs {
        if !input.is_file() {
            bail!("not a PDF file: {}", input.display());
        }
    }

    let jobs = plan_jobs(&cli)?;
    let total = jobs.len();
    let mut failures = 0usize;

    for (index, job) in jobs.iter().enumerate() {
        if total > 1 {
            println!("[{}/{}] {}", index + 1, total, job.input.display());
        }
        match convert(job, &cli, index) {
            Ok(summary) => print_summary(job, &summary, &cli),
            Err(error) => {
                failures += 1;
                eprintln!("  failed: {error:#}");
            }
        }
    }

    if failures > 0 {
        bail!("{failures} of {total} conversion(s) failed");
    }
    Ok(())
}

fn convert(job: &Job, cli: &Cli, _index: usize) -> Result<Summary> {
    let temp = tempfile::Builder::new()
        .prefix("x4pdf-")
        .tempdir()
        .context("creating temporary directory")?;
    let work = temp.path().to_path_buf();

    let doc = pdf::extract(&job.input, &work)?;
    let (canvas_w, _canvas_h) = cli.orientation.canvas();
    let margin = match cli.orientation {
        Orientation::Portrait => 16,
        Orientation::Landscape => 40,
    };
    let content_width = canvas_w - margin * 2;
    let max_image_height = match cli.orientation {
        Orientation::Portrait => 704,
        Orientation::Landscape => 430,
    };
    let body_px = (cli.font_pt * 220.0 / 72.0).round() as u32;
    let code_columns = epub::code_columns(content_width, body_px);
    let layouts = if cli.rasterize {
        layout::rasterize_pages(&doc, cli.spread)
    } else {
        layout::paginate(&doc, code_columns)
    };

    let summary = Summary {
        pages: layouts.len(),
        figures: layouts.iter().map(|p| p.stats.figures).sum(),
        rasterized: layouts.iter().filter(|p| p.stats.rasterized).count(),
        code_blocks: layouts.iter().map(|p| p.stats.code_blocks).sum(),
        epub: None,
        txt_bytes: None,
    };

    if cli.verbose {
        println!(
            "  type: {:.1} pt · {} px body · {} code column(s)",
            cli.font_pt, body_px, code_columns
        );
    }
    if cli.verbose || cli.dry_run {
        report_pages(&layouts);
    }

    if cli.dry_run {
        return Ok(summary);
    }

    let language = cli.lang.clone().unwrap_or_else(|| detect_language(&doc));
    let title = if cli.inputs.len() == 1 {
        cli.title.clone().unwrap_or_else(|| doc.title.clone())
    } else {
        doc.title.clone()
    };

    let mut epub_summary = None;
    if let Some(out) = &job.epub {
        check_writable(out, cli.force)?;
        let mut renderer =
            pdf::Renderer::new(&job.input, &work, content_width, doc.page_width_pt());
        let options = epub::EpubOptions {
            title,
            author: cli.author.clone(),
            language,
            orientation: cli.orientation,
            dither: cli.dither,
            content_width,
            max_image_height,
            body_px,
            rasterize: cli.rasterize,
        };
        let result = epub::write(&doc, &layouts, &mut renderer, &options, out)?;
        epub_summary = Some(result);
    }

    let mut txt_bytes = None;
    if let Some(out) = &job.txt {
        check_writable(out, cli.force)?;
        txt_bytes = Some(textout::write(
            &doc,
            &layouts,
            cli.orientation.label(),
            cli.wrap,
            out,
        )?);
    }

    if cli.keep_temp {
        let kept = temp.keep();
        println!("  temp kept: {}", kept.display());
    }

    Ok(Summary {
        epub: epub_summary,
        txt_bytes,
        ..summary
    })
}

fn report_pages(layouts: &[PageLayout]) {
    for page in layouts {
        let mut kinds = Vec::new();
        if page.stats.rasterized {
            kinds.push("rasterized page");
        }
        if page.stats.figures > 0 {
            kinds.push("figures");
        }
        if page.stats.code_blocks > 0 {
            kinds.push("code");
        }
        println!(
            "  page {:>3}: {:>5} chars, {} image box(es), {} figure(s){}",
            page.number,
            page.stats.chars,
            page.stats.images,
            page.stats.figures,
            if kinds.is_empty() {
                String::new()
            } else {
                format!(", {}", kinds.join(" + "))
            }
        );
    }
}

fn print_summary(job: &Job, summary: &Summary, cli: &Cli) {
    if cli.dry_run {
        println!(
            "  dry run: {} pages, {} figures ({} rasterized), {} code block(s)",
            summary.pages, summary.figures, summary.rasterized, summary.code_blocks
        );
        return;
    }
    if let Some(epub) = &summary.epub {
        if let Some(path) = &job.epub {
            println!(
                "  EPUB: {} ({} chapters, {} images, {:.1} MB)",
                path.display(),
                epub.chapters,
                epub.images,
                epub.bytes as f64 / 1_048_576.0
            );
        }
    }
    if let Some(bytes) = summary.txt_bytes {
        if let Some(path) = &job.txt {
            println!(
                "  TXT:  {} ({:.1} KB)",
                path.display(),
                bytes as f64 / 1024.0
            );
        }
    }
    println!(
        "  {} pages · {} figures · {} rasterized · {} code block(s)",
        summary.pages, summary.figures, summary.rasterized, summary.code_blocks
    );
}

fn check_writable(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    Ok(())
}

fn plan_jobs(cli: &Cli) -> Result<Vec<Job>> {
    let multiple = cli.inputs.len() > 1;
    let mut output_is_dir = false;
    let mut output_base: Option<PathBuf> = None;

    if let Some(output) = &cli.output {
        if multiple {
            if output.exists() && !output.is_dir() {
                bail!("with several inputs, --output must be a directory");
            }
            fs::create_dir_all(output).with_context(|| format!("creating {}", output.display()))?;
            output_is_dir = true;
        } else if output.is_dir() {
            output_is_dir = true;
        } else if output.exists() {
            // Existing file: exact destination for one format, base for both.
            output_base = Some(output.clone());
        } else {
            let ext = output
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("epub") | Some("txt")) {
                output_base = Some(output.clone());
            } else if cli.format == Format::Both {
                output_base = Some(output.clone());
            } else {
                output_is_dir = true;
                fs::create_dir_all(output)
                    .with_context(|| format!("creating {}", output.display()))?;
            }
        }
    }

    let mut jobs = Vec::new();
    for input in &cli.inputs {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "book".to_string());
        let parent = input.parent().unwrap_or_else(|| Path::new("."));

        let (epub, txt) = if let Some(base) = &output_base {
            if cli.format == Format::Both {
                let stem = base
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| stem.clone());
                let dir = base.parent().unwrap_or_else(|| Path::new("."));
                (
                    Some(dir.join(format!("{stem}.epub"))),
                    Some(dir.join(format!("{stem}.txt"))),
                )
            } else if cli.format.wants_epub() {
                (Some(base.clone()), None)
            } else {
                (None, Some(base.clone()))
            }
        } else if output_is_dir {
            let dir = cli.output.as_deref().unwrap_or(parent);
            (
                cli.format
                    .wants_epub()
                    .then(|| dir.join(format!("{stem}.x4.epub"))),
                cli.format
                    .wants_txt()
                    .then(|| dir.join(format!("{stem}.x4.txt"))),
            )
        } else {
            (
                cli.format
                    .wants_epub()
                    .then(|| parent.join(format!("{stem}.x4.epub"))),
                cli.format
                    .wants_txt()
                    .then(|| parent.join(format!("{stem}.x4.txt"))),
            )
        };

        jobs.push(Job {
            input: input.clone(),
            epub,
            txt,
        });
    }
    Ok(jobs)
}

fn detect_language(doc: &Document) -> String {
    let mut cjk = 0usize;
    let mut total = 0usize;
    for page in &doc.pages {
        for run in &page.runs {
            for ch in run.text.chars() {
                if ch.is_whitespace() {
                    continue;
                }
                total += 1;
                if util::is_cjk(ch) {
                    cjk += 1;
                }
            }
        }
    }
    if total > 0 && cjk * 5 > total {
        "zh".to_string()
    } else {
        "en".to_string()
    }
}
