use std::path::PathBuf;

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Orientation {
    Portrait,
    Landscape,
}

impl Orientation {
    /// Native Xteink X4C panel: 480x800 (portrait) / 800x480 (landscape).
    pub fn canvas(self) -> (u32, u32) {
        match self {
            Orientation::Portrait => (480, 800),
            Orientation::Landscape => (800, 480),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Orientation::Portrait => "portrait",
            Orientation::Landscape => "landscape",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Dither {
    /// Choose per image: no dithering for flat UI screenshots, FS for photos.
    Auto,
    /// Keep 8-bit grayscale; the reader's own rendering decides the levels.
    None,
    /// Floyd-Steinberg dither down to the panel's 16 gray levels.
    Fs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Epub,
    Txt,
    Both,
}

impl Format {
    pub fn wants_epub(self) -> bool {
        matches!(self, Format::Epub | Format::Both)
    }
    pub fn wants_txt(self) -> bool {
        matches!(self, Format::Txt | Format::Both)
    }
}

#[derive(Debug, Clone)]
pub struct Run {
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub height: f32,
    pub font_size: f32,
    pub family: String,
    pub text: String,
    pub bold: bool,
    pub italic: bool,
}

impl Run {
    pub fn is_mono(&self) -> bool {
        is_mono_family(&self.family)
    }

    pub fn is_bold(&self) -> bool {
        self.bold || is_bold_family(&self.family)
    }
}

#[derive(Debug, Clone)]
pub struct ImageBox {
    pub top: f32,
    pub left: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone)]
pub struct Page {
    pub number: usize,
    pub width: f32,
    pub height: f32,
    pub runs: Vec<Run>,
    pub images: Vec<ImageBox>,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub source: PathBuf,
    pub title: String,
    pub author: Option<String>,
    pub pages: Vec<Page>,
}

impl Document {
    pub fn page(&self, number: usize) -> Option<&Page> {
        self.pages.iter().find(|p| p.number == number)
    }

    pub fn page_width_pt(&self) -> f32 {
        self.pages
            .first()
            .map(|p| p.width / 1.5)
            .filter(|w| *w > 1.0)
            .unwrap_or(595.0)
    }
}

pub fn is_mono_family(family: &str) -> bool {
    let f = family.to_ascii_lowercase();
    [
        "mono",
        "courier",
        "menlo",
        "consolas",
        "inconsolata",
        "sourcecode",
        "source code",
        "firacode",
        "fira code",
        "lucida console",
        "andale mono",
        "dejavu sans mono",
        "ibm plex mono",
        "sfmono",
        "sf mono",
        "robotomono",
        "roboto mono",
        "jetbrainsmono",
        "jetbrains mono",
        "cascadia",
    ]
    .iter()
    .any(|needle| f.contains(needle))
}

pub fn is_bold_family(family: &str) -> bool {
    let f = family.to_ascii_lowercase();
    ["bold", "black", "semibold", "demi", "heavy", "extrabold"]
        .iter()
        .any(|needle| f.contains(needle))
}
