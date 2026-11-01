//! Hands links and files to macOS: the browser, Preview, Finder. Each call
//! follows a click or a key press; nothing here opens anything by itself.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

use anyhow::Context;

use super::links;

/// Opens a link from a message in the default browser or email app.
pub fn open_link(url: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        links::is_openable(url),
        "Mail only opens web and email links"
    );
    open([url.into()]);
    Ok(())
}

/// What happened to an attachment after it was saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opened {
    InApp,
    /// Shown in Finder instead: opening it could run a stranger's code.
    InFinder,
}

/// Opens a saved attachment if it is a document, image or media file, and
/// otherwise shows it in Finder.
pub fn open_download(path: &Path) -> Opened {
    let opened = opens_safely(path);
    match opened {
        Opened::InApp => open([path.into()]),
        Opened::InFinder => open(["-R".into(), path.into()]),
    }
    opened
}

/// Types that open in a viewer or editor without running anything. Apps,
/// scripts, installers, disk images, archives, web pages and unknown types
/// are shown in Finder, where macOS's own checks apply when you open them.
fn opens_safely(path: &Path) -> Opened {
    const VIEWABLE: &[&str] = &[
        "pdf", "png", "jpg", "jpeg", "gif", "heic", "heif", "webp", "tif", "tiff", "bmp", "txt",
        "csv", "md", "rtf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pages", "numbers", "key",
        "odt", "ods", "odp", "mp3", "m4a", "wav", "aac", "mp4", "mov", "m4v", "ics", "vcf",
    ];
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if VIEWABLE.contains(&extension.as_str()) {
        Opened::InApp
    } else {
        Opened::InFinder
    }
}

/// Writes a message's HTML to a temporary file and opens it in the browser,
/// for mail that needs its layout. Remote images load there, which tells
/// the sender the message was opened; scripts are blocked.
pub fn open_html(message_id: &str, html: &str) -> anyhow::Result<PathBuf> {
    let folder = std::env::temp_dir().join("mail-html");
    fs::create_dir_all(&folder)
        .with_context(|| format!("Could not create {}", folder.display()))?;
    let name: String = message_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let path = folder.join(format!("message-{name}.html"));
    fs::write(&path, html_page(html))
        .with_context(|| format!("Could not write {}", path.display()))?;
    open([path.as_path().into()]);
    Ok(path)
}

/// The message's HTML with two tags in front. The file is UTF-8 whatever the
/// message declared, and the first charset a browser sees wins. The policy
/// blocks scripts, plugins, frames and forms but lets images and styles load.
fn html_page(html: &str) -> String {
    const HEAD: &str = concat!(
        "<meta charset=\"utf-8\">",
        "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'none'; ",
        "object-src 'none'; frame-src 'none'; form-action 'none'; base-uri 'none'\">\n",
    );
    with_head(html, HEAD)
}

/// Puts `head` at the top of a message's HTML: before everything but a
/// doctype, which must stay first or WebKit falls back to quirks mode.
pub fn with_head(html: &str, head: &str) -> String {
    let trimmed = html.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    let after_doctype = trimmed
        .get(..9)
        .filter(|start| start.eq_ignore_ascii_case("<!doctype"))
        .and_then(|_| trimmed.find('>'))
        .map_or(0, |end| end + 1);
    let (doctype, rest) = trimmed.split_at(after_doctype);
    format!("{doctype}{head}{rest}")
}

/// Asks for an image with the macOS file picker. `None` if you cancel.
/// Blocks until you choose, so call it off the window's thread.
pub fn choose_image(prompt: &str) -> anyhow::Result<Option<PathBuf>> {
    // The prompt is ours, never text from mail, so quoting it is safe.
    let script = format!(
        "POSIX path of (choose file with prompt \"{prompt}\" of type {{\"public.image\"}})"
    );
    let output = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .context("Could not show the file picker")?;
    // Cancelling makes osascript fail with "User canceled".
    if !output.status.success() {
        return Ok(None);
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((!path.is_empty()).then(|| PathBuf::from(path)))
}

/// Runs `open` on a thread of its own, so a slow Launch Services lookup
/// never stalls the window.
fn open<const N: usize>(args: [OsString; N]) {
    thread::spawn(move || {
        if let Err(error) = Command::new("open").args(args).status() {
            eprintln!("mail: could not run open: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_open_and_anything_that_could_run_is_shown_in_finder() {
        for name in ["ticket.pdf", "photo.JPG", "notes.docx", "invite.ics"] {
            assert_eq!(opens_safely(Path::new(name)), Opened::InApp, "{name}");
        }
        for name in [
            "setup.app",
            "run.command",
            "install.pkg",
            "disk.dmg",
            "archive.zip",
            "page.html",
            "script.sh",
            "Makefile",
        ] {
            assert_eq!(opens_safely(Path::new(name)), Opened::InFinder, "{name}");
        }
    }

    #[test]
    fn html_pages_declare_utf8_and_block_scripts_first() {
        let page = html_page("<html><head><meta charset=\"iso-8859-1\"></head><body>Hi</body>");
        assert!(page.starts_with("<meta charset=\"utf-8\">"));
        assert!(page.contains("script-src 'none'"));
        assert!(page.find("utf-8").unwrap() < page.find("iso-8859-1").unwrap());
        assert!(page.ends_with("<body>Hi</body>"));
    }

    #[test]
    fn a_doctype_stays_in_front() {
        let page = html_page("\n<!DOCTYPE html>\n<html><body>Hi</body></html>");
        assert!(page.starts_with("<!DOCTYPE html><meta charset=\"utf-8\">"));
        assert!(page.ends_with("<html><body>Hi</body></html>"));
    }

    #[test]
    fn links_other_than_web_and_email_are_refused() {
        assert!(open_link("file:///Applications/Calculator.app").is_err());
    }
}
