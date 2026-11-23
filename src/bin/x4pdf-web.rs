use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde_json::{json, Value};
use tempfile::TempDir;
use tower_http::services::{ServeDir, ServeFile};
use uuid::Uuid;

const MAX_UPLOAD: usize = 100 * 1024 * 1024;
const MAX_BODY: usize = MAX_UPLOAD + 2 * 1024 * 1024;
const MAX_JOBS: usize = 20;
const JOB_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Parser, Debug)]
#[command(name = "x4pdf-web", about = "Local mobile web UI for x4pdf")]
struct Args {
    /// Local port for the web UI.
    #[arg(long, default_value_t = 4820)]
    port: u16,
    /// Path to the x4pdf converter binary.
    #[arg(long)]
    bin: Option<PathBuf>,
    /// Directory with index.html, app.js, styles.css and icons.
    #[arg(long)]
    web: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    jobs: Arc<Mutex<HashMap<String, Job>>>,
    tx: mpsc::Sender<JobRequest>,
}

#[derive(Debug, Clone)]
struct ConvertOptions {
    orientation: String,
    font_pt: f32,
    rasterize: bool,
    spread: bool,
    dither: String,
}

struct JobRequest {
    id: String,
    input: PathBuf,
    options: ConvertOptions,
}

struct Job {
    status: String,
    message: String,
    filename: String,
    epub: Option<PathBuf>,
    _workdir: Option<TempDir>,
    created: Instant,
    pages: Option<usize>,
    bytes: Option<u64>,
}

type ApiResult<T> = Result<T, (StatusCode, String)>;

fn bad<T>(code: u16, message: impl Into<String>) -> ApiResult<T> {
    Err((
        StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST),
        message.into(),
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web = args.web.unwrap_or_else(|| root.join("web"));
    if !web.join("index.html").is_file() {
        anyhow::bail!("web UI not found at {}", web.display());
    }

    let bin = args.bin.unwrap_or_else(|| {
        let sibling = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("x4pdf")))
            .unwrap_or_default();
        if sibling.is_file() {
            sibling
        } else {
            root.join("target/release/x4pdf")
        }
    });
    if !bin.is_file() {
        anyhow::bail!(
            "converter binary not found at {} (build with `cargo build --release`)",
            bin.display()
        );
    }

    let (tx, rx) = mpsc::channel::<JobRequest>();
    let jobs = Arc::new(Mutex::new(HashMap::new()));
    spawn_worker(rx, Arc::clone(&jobs), bin.clone());
    spawn_cleanup(Arc::clone(&jobs));

    let state = AppState {
        jobs: Arc::clone(&jobs),
        tx,
    };

    let app = Router::new()
        .route("/api/health", get(health))
        .route(
            "/api/convert",
            post(convert).layer(DefaultBodyLimit::max(MAX_BODY)),
        )
        .route("/api/jobs/{id}", get(job_status))
        .route("/api/download/{id}", get(download))
        .fallback_service(
            ServeDir::new(&web).not_found_service(ServeFile::new(web.join("index.html"))),
        )
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("x4pdf web: http://{addr}");
    println!("converter: {}", bin.display());
    println!("web root:  {}", web.display());
    axum::serve(listener, app).await?;
    Ok(())
}

fn spawn_worker(
    rx: mpsc::Receiver<JobRequest>,
    jobs: Arc<Mutex<HashMap<String, Job>>>,
    bin: PathBuf,
) {
    std::thread::spawn(move || {
        while let Ok(request) = rx.recv() {
            update_job(&jobs, &request.id, |job| {
                job.status = "running".to_string();
                job.message = "Converting on the Mac…".to_string();
            });

            let output_path = request.input.with_file_name("output.epub");
            let mut command = Command::new(&bin);
            command
                .arg(&request.input)
                .arg("--format")
                .arg("epub")
                .arg("--orientation")
                .arg(&request.options.orientation)
                .arg("--font-pt")
                .arg(format!("{:.2}", request.options.font_pt))
                .arg("--dither")
                .arg(&request.options.dither)
                .arg("--output")
                .arg(&output_path)
                .arg("--force");
            if request.options.rasterize {
                command.arg("--rasterize");
            }
            if request.options.spread {
                command.arg("--spread");
            }

            match command.output() {
                Ok(output) if output.status.success() && output_path.is_file() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let pages = parse_pages(&stdout);
                    let bytes = fs::metadata(&output_path).ok().map(|m| m.len());
                    update_job(&jobs, &request.id, |job| {
                        job.status = "done".to_string();
                        job.message = match pages {
                            Some(1) => "1 page ready".to_string(),
                            Some(n) => format!("{n} pages ready"),
                            None => "Ready".to_string(),
                        };
                        job.epub = Some(output_path);
                        job.pages = pages;
                        job.bytes = bytes;
                    });
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let detail = if stderr.trim().is_empty() {
                        stdout
                    } else {
                        stderr
                    };
                    let message: String = detail.trim().chars().take(600).collect();
                    update_job(&jobs, &request.id, |job| {
                        job.status = "error".to_string();
                        job.message = if message.is_empty() {
                            "Conversion failed".to_string()
                        } else {
                            message
                        };
                    });
                }
                Err(error) => {
                    update_job(&jobs, &request.id, |job| {
                        job.status = "error".to_string();
                        job.message = format!("Could not start the converter: {error}");
                    });
                }
            }
        }
    });
}

fn spawn_cleanup(jobs: Arc<Mutex<HashMap<String, Job>>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(10 * 60));
        if let Ok(mut jobs) = jobs.lock() {
            jobs.retain(|_, job| {
                job.created.elapsed() < JOB_TTL
                    || matches!(job.status.as_str(), "queued" | "running")
            });
        }
    });
}

fn update_job(jobs: &Arc<Mutex<HashMap<String, Job>>>, id: &str, update: impl FnOnce(&mut Job)) {
    let mut jobs = jobs.lock().unwrap();
    if let Some(job) = jobs.get_mut(id) {
        update(job);
    }
}

fn parse_pages(stdout: &str) -> Option<usize> {
    stdout.lines().find_map(|line| {
        let line = line.trim();
        if line.contains(" pages ·") {
            line.split_whitespace().next()?.parse().ok()
        } else {
            None
        }
    })
}

async fn health() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn convert(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    let mut data: Option<Vec<u8>> = None;
    let mut original_name = "book.pdf".to_string();
    let mut options = ConvertOptions {
        orientation: "portrait".to_string(),
        font_pt: 10.5,
        rasterize: false,
        spread: false,
        dither: "auto".to_string(),
    };

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "file" => {
                if let Some(filename) = field.file_name() {
                    original_name = filename.to_string();
                }
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
                if bytes.len() > MAX_UPLOAD {
                    return bad(413, "PDF is larger than 100 MB");
                }
                data = Some(bytes.to_vec());
            }
            "orientation" => {
                let value = text_field(field).await?;
                if value != "portrait" && value != "landscape" {
                    return bad(400, "orientation must be portrait or landscape");
                }
                options.orientation = value;
            }
            "font_pt" => {
                let value = text_field(field).await?;
                let size = value.parse::<f32>().map_err(|_| {
                    (
                        StatusCode::BAD_REQUEST,
                        "font_pt must be a number".to_string(),
                    )
                })?;
                if !(8.0..=12.5).contains(&size) {
                    return bad(400, "font_pt must be between 8 and 12.5");
                }
                options.font_pt = size;
            }
            "rasterize" => options.rasterize = parse_bool(&text_field(field).await?),
            "spread" => options.spread = parse_bool(&text_field(field).await?),
            "dither" => {
                let value = text_field(field).await?;
                if !matches!(value.as_str(), "auto" | "none" | "fs") {
                    return bad(400, "dither must be auto, none or fs");
                }
                options.dither = value;
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let data = match data {
        Some(data) => data,
        None => return bad(400, "No PDF file was uploaded"),
    };
    if !looks_like_pdf(&data) {
        return bad(400, "That file is not a PDF");
    }
    if options.spread && !options.rasterize {
        return bad(400, "Split spreads requires page-image mode");
    }

    let workdir = TempDir::new().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let input = workdir.path().join("input.pdf");
    fs::write(&input, &data).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let id = Uuid::new_v4().simple().to_string();
    let filename = format!("{}.x4.epub", clean_stem(&original_name));

    {
        let mut jobs = state.jobs.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "job lock poisoned".to_string(),
            )
        })?;
        jobs.retain(|_, job| {
            job.created.elapsed() < JOB_TTL || matches!(job.status.as_str(), "queued" | "running")
        });
        if jobs.len() >= MAX_JOBS {
            return bad(429, "Too many conversions queued; wait a minute");
        }
        jobs.insert(
            id.clone(),
            Job {
                status: "queued".to_string(),
                message: "Waiting for the converter…".to_string(),
                filename,
                epub: None,
                _workdir: Some(workdir),
                created: Instant::now(),
                pages: None,
                bytes: None,
            },
        );
    }

    state
        .tx
        .send(JobRequest {
            id: id.clone(),
            input,
            options,
        })
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "worker is offline".to_string(),
            )
        })?;

    Ok(Json(json!({ "id": id })))
}

async fn text_field(field: axum::extract::multipart::Field<'_>) -> ApiResult<String> {
    field
        .text()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

fn parse_bool(value: &str) -> bool {
    matches!(value.trim(), "1" | "true" | "on" | "yes")
}

fn looks_like_pdf(data: &[u8]) -> bool {
    data.windows(5).take(1024).any(|window| window == b"%PDF-")
}

fn clean_stem(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "book".to_string());
    let mut cleaned: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else if c.is_whitespace() {
                '-'
            } else {
                '_'
            }
        })
        .collect();
    cleaned = cleaned.trim_matches(['-', '_', '.']).to_string();
    if cleaned.is_empty() {
        cleaned = "book".to_string();
    }
    cleaned.chars().take(60).collect()
}

async fn job_status(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let jobs = state.jobs.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "job lock poisoned".to_string(),
        )
    })?;
    let job = jobs
        .get(&id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Unknown job".to_string()))?;

    let mut value = json!({
        "id": id,
        "status": job.status,
        "message": job.message,
    });
    if let Some(pages) = job.pages {
        value["pages"] = json!(pages);
    }
    if let Some(bytes) = job.bytes {
        value["bytes"] = json!(bytes);
    }
    if job.status == "done" {
        value["download"] = json!(format!("/api/download/{id}"));
        value["filename"] = json!(job.filename);
    }
    Ok(Json(value))
}

async fn download(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> ApiResult<Response> {
    let (path, filename) = {
        let jobs = state.jobs.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "job lock poisoned".to_string(),
            )
        })?;
        let job = jobs
            .get(&id)
            .ok_or_else(|| (StatusCode::NOT_FOUND, "Unknown job".to_string()))?;
        if job.status != "done" {
            return bad(409, "The EPUB is not ready yet");
        }
        (
            job.epub
                .clone()
                .ok_or_else(|| (StatusCode::NOT_FOUND, "EPUB missing".to_string()))?,
            job.filename.clone(),
        )
    };

    let bytes = fs::read(&path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/epub+zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_pdf_magic() {
        assert!(looks_like_pdf(b"%PDF-1.7\n"));
        assert!(looks_like_pdf(b"junk\n%PDF-1.4\n"));
        assert!(!looks_like_pdf(b"not a pdf"));
    }

    #[test]
    fn cleans_output_names() {
        assert_eq!(clean_stem("My Book!.pdf"), "My-Book");
        assert_eq!(clean_stem("../../etc/passwd"), "passwd");
        assert_eq!(clean_stem("書.pdf"), "book");
    }

    #[test]
    fn reads_page_count_from_cli_output() {
        assert_eq!(parse_pages("  30 pages · 30 figures\n"), Some(30));
        assert_eq!(parse_pages("  1 pages · 1 figure\n"), Some(1));
        assert_eq!(parse_pages("no summary\n"), None);
    }
}
