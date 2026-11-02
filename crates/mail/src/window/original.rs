//! A message's original HTML, shown in macOS's own WebKit over the reader,
//! the way Gmail shows it. Mail stays in charge of it:
//!
//! - A content policy blocks scripts, frames, forms, fonts and anything
//!   fetched from the internet. Remote images, which tell the sender you
//!   opened the message, load only after you choose "Load images".
//! - Every link click goes to Mail, which opens web and email links in your
//!   browser; the view itself never navigates.
//! - Keys pressed while the view has focus come back to the window, so the
//!   shortcuts keep working; only ⌘C and ⌘A stay for copying.
//!
//! WebKit draws on top of the window, so the view hides whenever an overlay
//! or menu is open.

use std::{
    borrow::Cow,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
};

use egui::{Event, Key, Modifiers};
use serde::Deserialize;
use wry::{
    NewWindowResponse, Rect, WebView, WebViewBuilder,
    dpi::{LogicalPosition, LogicalSize},
    http::{Response, StatusCode},
};

use super::files;

/// What the view shows: one message, with or without its remote images,
/// and with as many of the pictures sent inside it as Mail has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub message_id: String,
    pub images: bool,
    pub inline: usize,
}

/// Forwards key presses from the page to the window. Runs as WebKit's own
/// script, which the page's policy doesn't block; the page can't run any.
const FORWARD_KEYS: &str = r#"
document.addEventListener('keydown', function (event) {
  if (event.isComposing) return;
  // Copying and selecting stay with the page.
  if (event.metaKey && (event.key === 'c' || event.key === 'a')) return;
  event.preventDefault();
  event.stopPropagation();
  window.ipc.postMessage(JSON.stringify({
    key: event.key, shift: event.shiftKey, ctrl: event.ctrlKey,
    alt: event.altKey, meta: event.metaKey, repeat: event.repeat
  }));
}, true);
"#;

#[derive(Deserialize)]
struct KeyMessage {
    key: String,
    shift: bool,
    ctrl: bool,
    alt: bool,
    meta: bool,
    repeat: bool,
}

/// Pages are served from Mail's own `mail://` address rather than loaded as
/// bare HTML: WebKit then gets the content policy as a header the message
/// can't override, and key messages carry an address wry accepts.
const ORIGIN: &str = "mail://localhost/";

/// The page being shown: its path under [`ORIGIN`], its policy and its HTML.
#[derive(Default)]
struct Served {
    path: String,
    policy: String,
    html: String,
}

pub struct Original {
    webview: Option<WebView>,
    served: Arc<Mutex<Served>>,
    /// Counts pages, so each gets an address of its own.
    loads: u64,
    shown: Option<Page>,
    bounds: Option<egui::Rect>,
    visible: bool,
    keys_sent: Sender<Vec<Event>>,
    keys: Receiver<Vec<Event>>,
}

impl Original {
    pub fn new() -> Self {
        let (keys_sent, keys) = mpsc::channel();
        Self {
            webview: None,
            served: Arc::default(),
            loads: 0,
            shown: None,
            bounds: None,
            visible: false,
            keys_sent,
            keys,
        }
    }

    /// Shows `page` over `rect`, in window points. `html` is called only
    /// when the page changes.
    pub fn show(
        &mut self,
        frame: &eframe::Frame,
        ctx: &egui::Context,
        rect: egui::Rect,
        page: &Page,
        html: impl FnOnce() -> String,
    ) -> anyhow::Result<()> {
        if self.webview.is_none() {
            self.webview = Some(self.create(frame, ctx)?);
        }
        let webview = self.webview.as_ref().expect("created above");
        if self.shown.as_ref() != Some(page) {
            self.loads += 1;
            let path = format!("/message/{}", self.loads);
            *self.served.lock().expect("never poisoned") = Served {
                path: path.clone(),
                policy: policy(page.images),
                html: document(&html(), page.images),
            };
            webview.load_url(&format!("mail://localhost{path}"))?;
            self.shown = Some(page.clone());
        }
        if self.bounds != Some(rect) {
            webview.set_bounds(Rect {
                position: LogicalPosition::new(rect.left(), rect.top()).into(),
                size: LogicalSize::new(rect.width(), rect.height()).into(),
            })?;
            self.bounds = Some(rect);
        }
        if !self.visible {
            webview.set_visible(true)?;
            self.visible = true;
        }
        Ok(())
    }

    pub fn hide(&mut self) {
        if self.visible
            && let Some(webview) = &self.webview
        {
            let _ = webview.set_visible(false);
            // Keys go back to the window, not to a hidden page.
            let _ = webview.focus_parent();
            self.visible = false;
        }
    }

    /// Gives the keyboard back to the window.
    pub fn release_focus(&self) {
        if let Some(webview) = &self.webview {
            let _ = webview.focus_parent();
        }
    }

    /// Scrolls the page by `pages` screens, for Space and ⇧Space.
    pub fn scroll(&self, pages: f32) {
        if self.visible
            && let Some(webview) = &self.webview
        {
            let script = format!("window.scrollBy(0, window.innerHeight * 0.85 * {pages})");
            let _ = webview.evaluate_script(&script);
        }
    }

    /// Keys pressed in the page since the last call, as egui events.
    pub fn take_keys(&self) -> Vec<Event> {
        self.keys.try_iter().flatten().collect()
    }

    fn create(&self, frame: &eframe::Frame, ctx: &egui::Context) -> anyhow::Result<WebView> {
        let keys = self.keys_sent.clone();
        let wake = ctx.clone();
        let served = self.served.clone();
        let webview = WebViewBuilder::new()
            .with_custom_protocol("mail".into(), move |_, request| {
                let served = served.lock().expect("never poisoned");
                // Only the page itself: anything it asks for besides is refused.
                if request.uri().path() != served.path {
                    return Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Cow::Borrowed(&[][..]))
                        .expect("a valid response");
                }
                Response::builder()
                    .header("Content-Type", "text/html; charset=utf-8")
                    .header("Content-Security-Policy", served.policy.as_str())
                    .body(Cow::Owned(served.html.clone().into_bytes()))
                    .expect("a valid response")
            })
            .with_visible(false)
            .with_focused(false)
            .with_background_color((255, 255, 255, 255))
            .with_initialization_script(FORWARD_KEYS)
            .with_ipc_handler(move |request| {
                if let Ok(message) = serde_json::from_str::<KeyMessage>(request.body()) {
                    let _ = keys.send(key_events(&message));
                    wake.request_repaint();
                }
            })
            // Only the page Mail loads itself; links go to the browser.
            .with_navigation_handler(|url| {
                if url.starts_with(ORIGIN) {
                    return true;
                }
                let _ = files::open_link(&url);
                false
            })
            .with_new_window_req_handler(|url, _| {
                let _ = files::open_link(&url);
                NewWindowResponse::Deny
            })
            .with_back_forward_navigation_gestures(false)
            .build_as_child(frame)?;
        Ok(webview)
    }
}

/// Nothing but inline styles and embedded images, and remote images once
/// allowed. No scripts, frames, fonts, forms or requests of any other kind.
fn policy(images: bool) -> String {
    let images = if images {
        "data: https: http:"
    } else {
        "data:"
    };
    format!(
        "default-src 'none'; style-src 'unsafe-inline'; img-src {images}; \
         font-src data:; form-action 'none'; base-uri 'none'"
    )
}

/// The message's HTML with Mail's policy and a white page in front. Emails
/// are designed for white paper, whatever the window's appearance.
pub fn document(html: &str, images: bool) -> String {
    let head = format!(
        "<meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{}\">\
         <meta name=\"color-scheme\" content=\"light\">\
         <style>html {{ background: #ffffff; }}</style>\n",
        policy(images)
    );
    files::with_head(html, &head)
}

/// A key from the page as the events egui would have made for it.
fn key_events(message: &KeyMessage) -> Vec<Event> {
    let modifiers = Modifiers {
        alt: message.alt,
        ctrl: message.ctrl,
        shift: message.shift,
        mac_cmd: message.meta,
        command: message.meta,
    };
    let mut events = Vec::new();
    if let Some(key) = Key::from_name(&message.key) {
        events.push(Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: message.repeat,
            modifiers,
        });
    }
    // Typed characters, such as "#" and "?", also arrive as text.
    let mut characters = message.key.chars();
    if let (Some(character), None) = (characters.next(), characters.next())
        && !message.meta
        && !message.ctrl
        && !character.is_control()
    {
        events.push(Event::Text(character.to_string()));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_remote_loads_until_images_are_allowed() {
        let blocked = document("<!DOCTYPE html><html><body>Hi</body></html>", false);
        assert!(blocked.starts_with("<!DOCTYPE html><meta charset"));
        assert!(blocked.contains("default-src 'none'"));
        assert!(blocked.contains("img-src data:;"));
        let allowed = document("<p>Hi</p>", true);
        assert!(allowed.contains("img-src data: https: http:;"));
        // Scripts stay blocked either way.
        assert!(!allowed.contains("script-src"));
        assert!(allowed.contains("default-src 'none'"));
    }

    #[test]
    fn keys_from_the_page_become_window_events() {
        let key = |key: &str, shift: bool, meta: bool| KeyMessage {
            key: key.into(),
            shift,
            ctrl: false,
            alt: false,
            meta,
            repeat: false,
        };
        let events = key_events(&key("j", false, false));
        assert!(matches!(events[0], Event::Key { key: Key::J, .. }));
        assert_eq!(events[1], Event::Text("j".into()));

        // Shift+3 is "#": text only, which is how # is matched.
        assert_eq!(
            key_events(&key("#", true, false)),
            [Event::Text("#".into())]
        );

        let events = key_events(&key("Backspace", false, true));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            Event::Key { key: Key::Backspace, modifiers, .. } if modifiers.command
        ));
    }
}
