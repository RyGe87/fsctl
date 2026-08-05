//! Which tools this machine actually has, decided once at startup.
//!
//! The house rule is "ask whoever knows" — but who knows depends on where you
//! are. macOS answers with `plutil` and `sips`; a Linux box answers with
//! `python3` and ImageMagick, or with nothing at all, in which case the feature
//! says so instead of failing in a puzzling way.
//!
//! Resolution is by absolute path rather than through `$PATH`: a file manager
//! that copies your files should not be steered by an environment variable.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where programs live on the systems this runs on. Homebrew first, so a Mac
/// with a newer tool installed uses it.
const DIRS: [&str; 5] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
];

fn which(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        for dir in DIRS {
            let candidate = Path::new(dir).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// How to copy, which differs in more than the program name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyStyle {
    /// `cp -Rc`: clones on APFS, keeps extended attributes by default.
    Bsd,
    /// `cp -a --reflink=auto`: `-a` keeps attributes, the reflink clones where
    /// the filesystem can.
    Gnu,
}

/// How a picture becomes pixels we can place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageTool {
    Sips,
    ImageMagick,
}

/// How JSON gets laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonTool {
    Plutil,
    Python,
    Jq,
}

/// Who turns a page into text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlTool {
    /// macOS, and it is WebKit's own importer underneath.
    Textutil,
    W3m,
    Lynx,
    Html2text,
}

impl HtmlTool {
    pub fn name(self) -> &'static str {
        match self {
            HtmlTool::Textutil => "textutil",
            HtmlTool::W3m => "w3m",
            HtmlTool::Lynx => "lynx",
            HtmlTool::Html2text => "html2text",
        }
    }
}

/// Where a deleted file goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashStyle {
    /// Finder does the moving, so "Put Back" keeps working.
    Finder,
    /// The freedesktop spec: a files/ and info/ pair we write ourselves.
    Freedesktop,
}

pub struct Toolbox {
    pub cp: Option<PathBuf>,
    pub copy_style: CopyStyle,
    pub mv: Option<PathBuf>,
    pub sh: Option<PathBuf>,
    pub date: Option<PathBuf>,
    pub find: Option<PathBuf>,
    pub git: Option<PathBuf>,
    pub open: Option<PathBuf>,
    pub unzip: Option<PathBuf>,
    pub zip: Option<PathBuf>,
    pub tar: Option<PathBuf>,
    pub xmllint: Option<PathBuf>,
    pub json: Option<(JsonTool, PathBuf)>,
    /// Property lists are an Apple format; elsewhere there is nothing to ask.
    pub plutil: Option<PathBuf>,
    pub image: Option<(ImageTool, PathBuf)>,
    pub html: Option<(HtmlTool, PathBuf)>,
    pub trash: TrashStyle,
    pub osascript: Option<PathBuf>,
    /// BSD `stat` reports file flags; GNU `stat -f` means something else
    /// entirely, so the cloud markers only exist where the question does.
    pub file_flags: Option<PathBuf>,
}

pub fn get() -> &'static Toolbox {
    static BOX: OnceLock<Toolbox> = OnceLock::new();
    BOX.get_or_init(|| {
        let apple = Path::new("/usr/bin/sw_vers").is_file();
        let plutil = which(&["plutil"]);
        let osascript = which(&["osascript"]);

        Toolbox {
            cp: which(&["cp"]),
            copy_style: if apple {
                CopyStyle::Bsd
            } else {
                CopyStyle::Gnu
            },
            mv: which(&["mv"]),
            sh: which(&["sh"]),
            date: which(&["date"]),
            find: which(&["find"]),
            git: which(&["git"]),
            // xdg-open is the Linux answer to the same question.
            open: which(&["open", "xdg-open"]).filter(|p| apple || p.ends_with("xdg-open")),
            unzip: which(&["unzip"]),
            zip: which(&["zip"]),
            tar: which(&["tar"]),
            xmllint: which(&["xmllint"]),
            json: plutil
                .clone()
                .map(|p| (JsonTool::Plutil, p))
                .or_else(|| which(&["jq"]).map(|p| (JsonTool::Jq, p)))
                .or_else(|| which(&["python3"]).map(|p| (JsonTool::Python, p))),
            plutil,
            image: which(&["sips"])
                .map(|p| (ImageTool::Sips, p))
                .or_else(|| which(&["magick", "convert"]).map(|p| (ImageTool::ImageMagick, p))),
            html: which(&["textutil"])
                .map(|p| (HtmlTool::Textutil, p))
                .or_else(|| which(&["w3m"]).map(|p| (HtmlTool::W3m, p)))
                .or_else(|| which(&["lynx"]).map(|p| (HtmlTool::Lynx, p)))
                .or_else(|| which(&["html2text"]).map(|p| (HtmlTool::Html2text, p))),
            trash: if osascript.is_some() {
                TrashStyle::Finder
            } else {
                TrashStyle::Freedesktop
            },
            osascript,
            file_flags: if apple { which(&["stat"]) } else { None },
        }
    })
}

/// What `--doctor` prints: every feature, what answers for it, and what is off.
pub fn report() -> String {
    let t = get();
    let mut out = String::new();
    let line = |out: &mut String, what: &str, found: Option<String>, note: &str| {
        match found {
            Some(p) => out.push_str(&format!("  {:<10} ✓  {:<28} {}\n", what, p, note)),
            None => out.push_str(&format!("  {:<10} ✗  {:<28} {}\n", what, "not found", note)),
        };
    };
    let show = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());

    out.push_str("fsctl — what this machine can do\n\n");
    line(
        &mut out,
        "copy",
        show(&t.cp),
        match t.copy_style {
            CopyStyle::Bsd => "cp -Rc (APFS clone, keeps xattrs)",
            CopyStyle::Gnu => "cp -a --reflink=auto",
        },
    );
    line(&mut out, "move", show(&t.mv), "mv -f");
    line(
        &mut out,
        "open",
        show(&t.open),
        "hands a file to the desktop",
    );
    line(&mut out, "git", show(&t.git), "the repository sources");
    line(&mut out, "find", show(&t.find), "finding repositories");
    line(
        &mut out,
        "trash",
        Some(
            match t.trash {
                TrashStyle::Finder => "Finder (Put Back works)",
                TrashStyle::Freedesktop => "~/.local/share/Trash",
            }
            .to_string(),
        ),
        "",
    );
    line(
        &mut out,
        "archives",
        show(&t.unzip),
        "looking inside zip and friends",
    );
    line(&mut out, "packing", show(&t.zip), "z makes a zip");
    line(
        &mut out,
        "json",
        t.json.as_ref().map(|(k, p)| {
            format!(
                "{} ({})",
                p.display(),
                match k {
                    JsonTool::Plutil => "plutil",
                    JsonTool::Jq => "jq",
                    JsonTool::Python => "python3",
                }
            )
        }),
        "formatting in the preview",
    );
    line(
        &mut out,
        "xml",
        show(&t.xmllint),
        "formatting in the preview",
    );
    line(
        &mut out,
        "html",
        t.html
            .as_ref()
            .map(|(k, p)| format!("{} ({})", p.display(), k.name())),
        "reading a page in the preview",
    );
    line(
        &mut out,
        "images",
        t.image.as_ref().map(|(k, p)| {
            format!(
                "{} ({})",
                p.display(),
                match k {
                    ImageTool::Sips => "sips",
                    ImageTool::ImageMagick => "imagemagick",
                }
            )
        }),
        "thumbnails in the preview",
    );
    line(
        &mut out,
        "cloud",
        show(&t.file_flags),
        "the ☁ for files that are not here yet",
    );
    out.push_str("\nA missing tool turns off one feature; nothing else changes.\n");
    out
}
