//! The browser window.
//!
//! **UNVERIFIED.** This module compiles and is covered by unit tests for its
//! pure logic, but it has never been run: the environment it was written in has
//! no display server, so nobody has watched it open a window or draw a frame.
//! Treat first-run behaviour as untested. Everything it depends on — parsing,
//! cascade, layout, shaping, painting — *is* tested, and the pipeline it drives
//! is the same one the reference tests exercise, so the untested surface is
//! this file's event handling and blitting rather than the rendering itself.
//!
//! Deliberately thin. Tabs, a URL bar, history, and the mode banner are M3
//! (ADR-0009); this is a viewport onto an already-rendered page.

use std::num::NonZeroU32;
use std::rc::Rc;

use layout::RenderMode;
use text::FontStore;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Pixels scrolled per arrow-key press.
const SCROLL_STEP: f32 = 60.0;
/// Multiplier applied to line-based mouse wheel deltas.
const WHEEL_LINE_HEIGHT: f32 = 40.0;

/// Clamps a scroll offset to the scrollable range.
///
/// Pure, and therefore testable without a display — which is most of what this
/// module gets wrong when it gets anything wrong.
fn clamp_scroll(offset: f32, content_height: f32, viewport_height: f32) -> f32 {
    let max = (content_height - viewport_height).max(0.0);
    offset.clamp(0.0, max)
}

/// Window title for a page, including its rendering mode.
///
/// ADR-0009 forbids switching rendering mode silently. Until M3 provides a
/// banner, the title bar is where the browser says what it did.
fn title_for(source: &str, mode: &RenderMode) -> String {
    match mode {
        RenderMode::Authored => format!("{source} — 2kbrowser"),
        RenderMode::Document { .. } => format!("{source} — rendered as document — 2kbrowser"),
        RenderMode::RequiresScripting => format!("{source} — needs JavaScript — 2kbrowser"),
    }
}

/// Everything the event loop needs between frames.
struct App {
    html: String,
    source: String,
    fonts: FontStore,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    page: Option<crate::render::Page>,
    scroll: f32,
    size: (u32, u32),
}

impl App {
    /// Re-renders at the current width. Called on open and on resize, because
    /// layout depends on viewport width and nothing else here does.
    fn rerender(&mut self) {
        let (width, height) = self.size;
        if width == 0 || height == 0 {
            return;
        }
        // The canvas is the full document height, not the viewport height:
        // scrolling then costs a blit offset rather than a re-layout.
        let page = crate::render::render(&self.html, width, u32::MAX, &mut self.fonts);
        if let Some(window) = &self.window {
            window.set_title(&title_for(&self.source, &page.mode));
        }
        self.scroll = clamp_scroll(self.scroll, page.content_height, height as f32);
        self.page = Some(page);
    }

    fn scroll_by(&mut self, delta: f32) {
        let Some(page) = &self.page else { return };
        let before = self.scroll;
        self.scroll = clamp_scroll(self.scroll + delta, page.content_height, self.size.1 as f32);
        if self.scroll != before
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    /// Copies the rendered page into the window surface at the current scroll.
    fn draw(&mut self) {
        let (Some(surface), Some(page)) = (&mut self.surface, &self.page) else {
            return;
        };
        let (Some(width), Some(height)) =
            (NonZeroU32::new(self.size.0), NonZeroU32::new(self.size.1))
        else {
            return;
        };
        if surface.resize(width, height).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };

        let pixmap = &page.pixmap;
        let offset = self.scroll as u32;
        let viewport_width = width.get() as usize;

        for row in 0..height.get() {
            let source_row = row + offset;
            let start = row as usize * viewport_width;
            if source_row >= pixmap.height() {
                // Past the end of the document: white, not stale pixels.
                buffer[start..start + viewport_width].fill(0x00ff_ffff);
                continue;
            }
            let pixels = pixmap.pixels();
            let source_start = source_row as usize * pixmap.width() as usize;
            for column in 0..viewport_width {
                let value = match pixels.get(source_start + column) {
                    // softbuffer wants 0RGB packed into a u32; tiny-skia stores
                    // premultiplied RGBA, and demultiplying is unnecessary here
                    // because the page is composited over opaque white already.
                    Some(pixel) => {
                        (u32::from(pixel.red()) << 16)
                            | (u32::from(pixel.green()) << 8)
                            | u32::from(pixel.blue())
                    }
                    None => 0x00ff_ffff,
                };
                buffer[start + column] = value;
            }
        }

        let _ = buffer.present();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = Window::default_attributes()
            .with_title(&self.source)
            .with_inner_size(winit::dpi::LogicalSize::new(self.size.0, self.size.1));
        let Ok(window) = event_loop.create_window(attributes) else {
            event_loop.exit();
            return;
        };
        let window = Rc::new(window);

        let context = match softbuffer::Context::new(window.clone()) {
            Ok(context) => context,
            Err(_) => {
                event_loop.exit();
                return;
            }
        };
        match softbuffer::Surface::new(&context, window.clone()) {
            Ok(surface) => self.surface = Some(surface),
            Err(_) => {
                event_loop.exit();
                return;
            }
        }

        let size = window.inner_size();
        self.size = (size.width.max(1), size.height.max(1));
        self.window = Some(window);
        self.rerender();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.size = (size.width.max(1), size.height.max(1));
                self.rerender();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::MouseWheel { delta, .. } => {
                let pixels = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => -lines * WHEEL_LINE_HEIGHT,
                    MouseScrollDelta::PixelDelta(position) => -position.y as f32,
                };
                self.scroll_by(pixels);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let viewport = self.size.1 as f32;
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::ArrowDown) => self.scroll_by(SCROLL_STEP),
                    Key::Named(NamedKey::ArrowUp) => self.scroll_by(-SCROLL_STEP),
                    Key::Named(NamedKey::PageDown) | Key::Named(NamedKey::Space) => {
                        self.scroll_by(viewport * 0.9);
                    }
                    Key::Named(NamedKey::PageUp) => self.scroll_by(-viewport * 0.9),
                    Key::Named(NamedKey::Home) => self.scroll_by(f32::NEG_INFINITY),
                    Key::Named(NamedKey::End) => self.scroll_by(f32::INFINITY),
                    Key::Character(ref c) if c == "q" => event_loop.exit(),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Opens a window showing `html`, and runs until the user closes it.
///
/// `source` is shown in the title bar. Blocks for the lifetime of the window.
pub fn open(html: String, source: String, width: u32, height: u32) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|error| {
        format!("could not start the event loop ({error}); is a display available?")
    })?;
    // Wait rather than poll: a document browser has nothing to animate, and
    // polling would burn CPU against the resource-weight goal for no benefit.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App {
        html,
        source,
        fonts: FontStore::new(),
        window: None,
        surface: None,
        page: None,
        scroll: 0.0,
        size: (width.max(1), height.max(1)),
    };
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_is_clamped_to_the_document() {
        // Cannot scroll above the top.
        assert_eq!(clamp_scroll(-50.0, 1000.0, 600.0), 0.0);
        // Nor past the last screenful.
        assert_eq!(clamp_scroll(9999.0, 1000.0, 600.0), 400.0);
        assert_eq!(clamp_scroll(150.0, 1000.0, 600.0), 150.0);
    }

    #[test]
    fn a_document_shorter_than_the_window_does_not_scroll() {
        assert_eq!(clamp_scroll(100.0, 300.0, 600.0), 0.0);
        assert_eq!(clamp_scroll(f32::INFINITY, 300.0, 600.0), 0.0);
    }

    #[test]
    fn the_title_states_the_rendering_mode() {
        // ADR-0009: never switch rendering mode silently. Until M3 has a
        // banner, the title bar carries it.
        assert_eq!(
            title_for("a.html", &RenderMode::Authored),
            "a.html — 2kbrowser"
        );
        assert!(
            title_for(
                "a.html",
                &RenderMode::Document {
                    unsupported_share: 0.9
                }
            )
            .contains("rendered as document")
        );
        assert!(title_for("a.html", &RenderMode::RequiresScripting).contains("needs JavaScript"));
    }
}
