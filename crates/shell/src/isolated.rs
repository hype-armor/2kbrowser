//! Rendering in a separate process.
//!
//! The `shell` half of ADR-0012: `sandbox` supplies the transport and knows
//! nothing about rendering; this supplies the rendering and knows nothing about
//! pipes. The two meet at [`sandbox::child::Render`].
//!
//! Nothing here changes what a page looks like. The child runs exactly the
//! pipeline [`crate::render`] runs in-process, and the test that matters says
//! so: the pixels that come back across the pipe are byte-identical to the ones
//! produced without one.

use sandbox::child::{Fetched, Render};
use sandbox::message::{Link, Missing, Mode, Rendered};
use sandbox::{Error, ToChild};
use text::FontStore;

/// Loads subresources by asking the parent.
///
/// The child has no sockets and no filesystem of its own (ADR-0012), so this is
/// the only way anything gets in. Every request crosses the pipe and the parent
/// applies ADR-0006's policy — which is the improvement worth having: the rule
/// is now enforced in a process a compromised renderer cannot reach.
struct PipeLoader<'a> {
    fetch: &'a mut dyn FnMut(&[String], net::RequestKind) -> Vec<Fetched>,
}

impl crate::render::Loader for PipeLoader<'_> {
    fn load(
        &mut self,
        url: &str,
        document: Option<&net::Origin>,
        kind: net::RequestKind,
    ) -> Option<crate::render::Loaded> {
        // One URL is a batch of one. There is no separate single-fetch path
        // over the pipe, so there is no second path to drift.
        self.load_many(&[url.to_owned()], document, kind)
            .pop()
            .flatten()
    }

    fn load_many(
        &mut self,
        urls: &[String],
        _document: Option<&net::Origin>,
        kind: net::RequestKind,
    ) -> Vec<Option<crate::render::Loaded>> {
        // The document origin is dropped rather than sent. The parent already
        // knows it — it is what the parent asked for a render of — and taking
        // it from the untrusted side would let a compromised renderer claim to
        // be an origin it is not, which is the whole policy defeated in one
        // field.
        (self.fetch)(urls, kind)
            .into_iter()
            .map(|resource| {
                resource.map(|got| crate::render::Loaded {
                    bytes: got.bytes,
                    content_type: got.content_type,
                })
            })
            .collect()
    }
}

/// Renders using this crate's pipeline. The child's half.
pub struct PageRenderer {
    fonts: FontStore,
    /// The page most recently rendered, kept so it can still be searched.
    ///
    /// This is the whole reason the child outlives a single message: the text
    /// and the box tree a find query searches never cross the boundary, so the
    /// only thing that can answer is the process holding them.
    page: Option<crate::render::Page>,
    /// Whether the reader overruled the document fallback for this page.
    ///
    /// Remembered because a band has to describe the page the same way the
    /// render did, and whether there is a decision to overrule depends on a
    /// choice the parent made rather than on anything in the pixels.
    force_authored: bool,
    /// Whether the reader asked for the document fallback on a page that did
    /// not need one. Remembered for the same reason as `force_authored`.
    force_document: bool,
    /// The zoom this page was rendered at, so a band matches the page it is
    /// part of. Remembered for the same reason as the overrides.
    zoom: f32,
    /// The render request this page came from (#110).
    ///
    /// Typing changes the layout, so it needs a whole render rather than a
    /// repaint — and a render needs everything the parent said the first time.
    /// Kept here rather than asked for again, because a keystroke that had to
    /// go back to the parent for the document's bytes would be a keystroke that
    /// crossed the boundary twice.
    last: Option<ToChild>,
    /// What has been typed into this page's controls, by node.
    ///
    /// The document is re-parsed on every render and these are re-applied to
    /// it. Node ids survive that because parsing the same bytes builds the same
    /// arena in the same order — and the bytes are the same bytes, since they
    /// are the ones in `last`.
    values: Vec<(dom::NodeId, String)>,
    /// What the reader has ticked, chosen or unticked, by node. Re-applied
    /// after a parse the same way, and for the same reason, as `values`.
    chosen: Vec<(dom::NodeId, bool)>,
    /// The control the keyboard is on, and whatever it needs remembering.
    focus: Option<Focus>,
    /// A form the reader asked to send, waiting for the next render to carry
    /// it out (#110).
    ///
    /// Taken rather than held: one press sends one form, and a submission left
    /// lying here would be re-sent by the next resize.
    submit: Option<sandbox::message::Submission>,
    /// A dropdown the reader opened, waiting for the next render to carry it
    /// out to the parent.
    ///
    /// Taken rather than held, like `submit`: one press opens one list, and a
    /// dropdown left lying here would spring open again on the next resize.
    open: Option<sandbox::message::Dropdown>,
}

/// What the keyboard is on, and what that control needs remembered about it.
///
/// Two shapes because there are two kinds of control and they need different
/// things kept. A field is being *edited*, so it carries a caret and a
/// selection that no re-render may lose. A checkbox is not being edited at
/// all: it is somewhere the keyboard is pointing, and the only thing worth
/// remembering is which one (#151).
///
/// Before this there was one shape — a node and a text editing state — and so
/// only the controls that had one could be reached. A form could be filled in
/// with a pointer and not with a keyboard.
#[derive(Debug, Clone)]
enum Focus {
    /// A text field or `<textarea>`, with what is being typed in it.
    Typing {
        /// The control.
        node: dom::NodeId,
        /// Its caret, selection and text.
        field: crate::field::Field,
    },
    /// A control that is pressed rather than typed in: a checkbox, a radio, a
    /// `<select>`, a button.
    Pressable {
        /// The control.
        node: dom::NodeId,
    },
}

impl Focus {
    /// Which control it is on, whichever shape it has.
    fn node(&self) -> dom::NodeId {
        match self {
            Focus::Typing { node, .. } | Focus::Pressable { node } => *node,
        }
    }
}

impl Default for PageRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl PageRenderer {
    /// A renderer with the bundled fonts loaded.
    pub fn new() -> Self {
        Self {
            fonts: FontStore::new(),
            page: None,
            force_authored: false,
            zoom: 1.0,
            force_document: false,
            last: None,
            values: Vec::new(),
            chosen: Vec::new(),
            focus: None,
            submit: None,
            open: None,
        }
    }

    /// Whether this page is showing a layout decision rather than the plain
    /// answer — either one classification made, or one the reader asked for.
    ///
    /// Both overrides count, and neither is redundant. `force_authored` is the
    /// case classification wanted a fallback and the reader said no, so the
    /// page reports `Authored` and nothing in the mode records that a decision
    /// was made. `force_document` is the reverse, and it is named rather than
    /// inferred from the mode so that a page whose forced render comes back
    /// `Authored` anyway — a frameset has no fallback to give — still offers
    /// the way back instead of stranding the reader with a button that has
    /// vanished under their pointer.
    fn can_toggle_layout(&self, page: &crate::render::Page) -> bool {
        self.force_authored
            || self.force_document
            || !matches!(page.mode, layout::RenderMode::Authored)
    }
}

/// How the page was rendered, in the protocol's terms.
///
/// The wire type deliberately mirrors `RenderMode` rather than being it, so a
/// new variant on either side is a compile error instead of a wire format that
/// quietly changed shape.
fn mode_of(page: &crate::render::Page) -> Mode {
    match &page.mode {
        layout::RenderMode::Authored => Mode::Authored,
        layout::RenderMode::Document { unsupported_share } => Mode::Document {
            unsupported_share: *unsupported_share,
        },
        layout::RenderMode::DocumentFrame { containers } => Mode::DocumentFrame {
            containers: *containers as u32,
        },
        layout::RenderMode::RequiresScripting => Mode::RequiresScripting,
    }
}

/// Every link on the page, flattened with its group so the parent can put the
/// pieces of a wrapped link back together.
fn links_of(page: &crate::render::Page) -> Vec<Link> {
    page.link_groups()
        .into_iter()
        .enumerate()
        .flat_map(|(group, link)| {
            let (url, jump_to, pinned) = (link.url, link.jump_to, link.pinned);
            link.rects.into_iter().map(move |rect| Link {
                rect,
                url: url.clone(),
                group: group as u32,
                jump_to,
                pinned,
            })
        })
        .collect()
}

/// Every placeholder on the page, for a parent that has to route a click to
/// one (#118).
///
/// The same shape as [`links_of`] and for the same reason: the parent has no
/// box tree, so a rectangle missing from this list is a placeholder that does
/// nothing when pressed.
fn missing_of(page: &crate::render::Page) -> Vec<Missing> {
    page.missing_images()
        .into_iter()
        .map(|(rect, url)| Missing { rect, url })
        .collect()
}

/// A canvas colour packed for the wire, as `0x00RRGGBB`.
///
/// The alpha is dropped rather than carried: the display list's canvas colour
/// is already composited over white, so it is opaque by construction, and the
/// window has nothing to blend it against anyway.
fn packed(colour: css::Color) -> u32 {
    (u32::from(colour.r) << 16) | (u32::from(colour.g) << 8) | u32::from(colour.b)
}

impl PageRenderer {
    /// Focuses whatever text control sits at `at`, or nothing (#110).
    ///
    /// Answered here because the box tree is the only thing that knows where a
    /// control is, and it never crosses the boundary. A press on nothing gives
    /// up the focus, which is what pressing the margin of a page means
    /// everywhere.
    fn focus_at(&mut self, at: (f32, f32)) {
        // A button first: it is a press rather than a place to type, and a
        // button that sat inside a field's rectangle would otherwise be
        // unreachable (#110).
        if let Some(button) = self
            .page
            .as_ref()
            .and_then(|page| page.button_at(at.0, at.1))
        {
            self.set_focus(Some(button));
            self.ask_to_send(button);
            return;
        }
        // Then a control that changes when it is pressed: a checkbox, a radio,
        // a row of a list box. It answers the press *instead of* taking the
        // typing, because there is nothing to type into one — and it takes the
        // keyboard, so that Tab carries on from what was just pressed rather
        // than from the top of the page (#151).
        let pressed = self
            .page
            .as_ref()
            .and_then(|page| page.choice_at(at.0, at.1));
        if let Some((control, changed)) = pressed {
            self.set_focus(Some(control));
            self.record(changed);
            return;
        }
        // Then a closed dropdown, which is opened rather than changed: what is
        // in it has to be drawn over the page, and only the parent has
        // anywhere to draw that.
        if let Some((node, rect, options, on)) = self
            .page
            .as_ref()
            .and_then(|page| page.dropdown_at(at.0, at.1))
        {
            self.set_focus(Some(node));
            self.open = Some(sandbox::message::Dropdown {
                rect,
                node: node.0 as u32,
                options,
                on: on as u32,
            });
            return;
        }
        let found = self
            .page
            .as_ref()
            .and_then(|page| page.control_at(at.0, at.1));
        self.set_focus(found);
    }

    /// Chooses one option of a `<select>`, named the way this side named it.
    ///
    /// Everything the parent hands back is checked rather than trusted. The id
    /// is an index into this process's arena and the index is a position in a
    /// list only this process has; either could be stale by the time it
    /// returns, because a render in between re-parsed the document. A name
    /// that no longer fits changes nothing, which is what a message that no
    /// longer makes sense should do.
    fn choose(&mut self, node: u32, index: u32) {
        let changed = self
            .page
            .as_ref()
            .map(|page| page.choose_in(dom::NodeId(node as usize), index as usize))
            .unwrap_or_default();
        self.record(changed);
    }

    /// Records what a press changed, for the next render to apply.
    ///
    /// Kept beside the document rather than written into it, the same way a
    /// typed value is and for the same reason: the document is re-parsed on
    /// every render, and anything written into the old one would be gone.
    fn record(&mut self, changes: Vec<(dom::NodeId, bool)>) {
        for (node, on) in changes {
            match self.chosen.iter_mut().find(|(at, _)| *at == node) {
                Some(entry) => entry.1 = on,
                None => self.chosen.push((node, on)),
            }
        }
    }

    /// Collects the form `node` is in, for the parent to send.
    ///
    /// Nothing happens here beyond the collecting. Where it goes and whether it
    /// goes at all is the parent's decision, because a stranger's page must not
    /// be able to make this process talk to a server of its choosing — only to
    /// ask (ADR-0012).
    fn ask_to_send(&mut self, node: dom::NodeId) {
        // What has been typed has to be in the document before the form is
        // read out of it, and it is only written there by a render.
        self.flush_focus();
        let Some(page) = self.page.as_ref() else {
            return;
        };
        let Some(collected) = page.submission_from(node) else {
            return;
        };
        self.submit = Some(sandbox::message::Submission {
            action: collected.action,
            post: collected.method == layout::forms::Method::Post,
            body: collected.body,
        });
    }

    /// Moves the focus to a control, in whichever shape that control needs.
    ///
    /// A field starts its editing state from what the control currently holds,
    /// so that focusing `<input value="Smith">` and pressing a key appends
    /// rather than replaces. Everything else is remembered by name alone: there
    /// is nothing in a checkbox to put a caret in.
    fn set_focus(&mut self, node: Option<dom::NodeId>) {
        self.flush_focus();
        self.focus = node.map(|node| {
            let page = self.page.as_ref();
            if page.is_some_and(|page| page.takes_typing(node)) {
                let value = page
                    .map(|page| page.control_value(node))
                    .unwrap_or_default();
                Focus::Typing {
                    node,
                    field: crate::field::Field::with_cursor_at_end(value),
                }
            } else {
                Focus::Pressable { node }
            }
        });
    }

    /// Writes what is being edited back into the values the next render reads.
    ///
    /// Nothing to write for a control that is not being typed in: what a
    /// checkbox holds was recorded when it was pressed, not while it was
    /// focused.
    fn flush_focus(&mut self) {
        let Some(Focus::Typing { node, field }) = &self.focus else {
            return;
        };
        let (node, text) = (*node, field.text().to_owned());
        match self.values.iter_mut().find(|(at, _)| *at == node) {
            Some(entry) => entry.1 = text,
            None => self.values.push((node, text)),
        }
    }

    /// Moves to the next control in document order, or the previous one.
    ///
    /// *Every* control, not only the ones with something to type in (#151): a
    /// form whose boxes can be ticked with a pointer and by no other means is
    /// a form half its readers cannot fill in.
    ///
    /// Each radio of a group is its own stop. A browser enters the group once
    /// and moves within it with the arrows, which is fewer stops to tab
    /// through; it is also a second rule to learn, and this way every control
    /// on the page behaves the same. Worth revisiting on a page with thirty
    /// radios in it.
    ///
    /// Past the end it gives up the focus rather than wrapping. Wrapping is
    /// what a browser does inside a *form*, and falling out hands Tab back to
    /// the window, which walks the page's links with it.
    fn step_focus(&mut self, back: bool) {
        let controls: Vec<dom::NodeId> = self
            .page
            .as_ref()
            .map(|page| page.focusable().into_iter().map(|(node, _)| node).collect())
            .unwrap_or_default();
        let at = self
            .focus
            .as_ref()
            .and_then(|focus| controls.iter().position(|it| *it == focus.node()));
        let next = match (at, back) {
            (Some(at), false) => controls.get(at + 1).copied(),
            (Some(at), true) => at.checked_sub(1).and_then(|at| controls.get(at).copied()),
            (None, false) => controls.first().copied(),
            (None, true) => controls.last().copied(),
        };
        self.set_focus(next);
    }

    /// Applies one keystroke to whatever is focused.
    fn apply(&mut self, key: &sandbox::message::Key) {
        use sandbox::message::Key;
        match key {
            Key::Tab { back } => return self.step_focus(*back),
            Key::Escape => return self.set_focus(None),
            _ => {}
        }
        // A control that is pressed rather than typed in answers a different
        // set of keys, so it is answered before the editing below rather than
        // inside it — there is no field there to reach.
        if let Some(Focus::Pressable { node }) = &self.focus {
            return self.press_key(*node, key);
        }
        let multiline = self
            .focus
            .as_ref()
            .zip(self.page.as_ref())
            .is_some_and(|(focus, page)| page.is_multiline(focus.node()));
        let mut submitting = false;
        let Some(Focus::Typing { node, field }) = &mut self.focus else {
            return;
        };
        let node = *node;
        match key {
            // A newline belongs in a `<textarea>` and nowhere else. The parent
            // sends the character and this decides, because the parent has no
            // idea what kind of control it is typing into and should not have
            // to. In a one-line field Enter means *send the form*, which is
            // how every search box on the era's web is used — and a literal
            // newline in one would be a field holding something no browser
            // would ever put there.
            Key::Insert(text) if text.contains('\n') && !multiline => {
                let without: String = text.chars().filter(|c| *c != '\n').collect();
                if !without.is_empty() {
                    field.insert(&without);
                }
                submitting = true;
            }
            Key::Insert(text) => field.insert(text),
            Key::Backspace => field.backspace(),
            Key::Delete => field.delete(),
            Key::Left { extend, word } if *word => field.word_left(*extend),
            Key::Left { extend, .. } => field.left(*extend),
            Key::Right { extend, word } if *word => field.word_right(*extend),
            Key::Right { extend, .. } => field.right(*extend),
            Key::Home { extend } => field.home(*extend),
            Key::End { extend } => field.end(*extend),
            Key::SelectAll => field.select_all(),
            // Handled above, or not this control's: the arrows belong to a
            // `<select>`, and in a field they are the window's, so the page
            // still scrolls under a caret.
            Key::Tab { .. } | Key::Escape | Key::Up | Key::Down => {}
        }
        self.flush_focus();
        if submitting {
            // Nothing was pressed, so no button is a successful control. That
            // is HTML's own rule and it is the difference between a search box
            // and a form with two buttons meaning opposite things.
            self.ask_to_send(node);
        }
    }

    /// What has the keyboard, in the terms the parent is told it in.
    fn focused(&self) -> sandbox::message::Focused {
        use sandbox::message::Focused;
        match &self.focus {
            None => Focused::Nothing,
            Some(Focus::Typing { .. }) => Focused::Typing,
            Some(Focus::Pressable { .. }) => Focused::Pressable,
        }
    }

    /// One keystroke on a control that is pressed rather than typed in (#151).
    ///
    /// The keys a reader already knows, and no others:
    ///
    /// * **Space** presses it — a box ticks, a radio is chosen, a button is
    ///   pressed, a dropdown opens. It arrives as a space character because the
    ///   parent has no idea what is focused and should not have to; deciding
    ///   what a space means is this side's job, exactly as it already is for a
    ///   newline in a one-line field.
    /// * **Enter** sends the form. On a button it presses *that* button, which
    ///   is the difference between a form with one button and a form with two
    ///   meaning opposite things.
    /// * **Up and Down** move a `<select>` through its options without opening
    ///   the list, which is what a dropdown has always done and is the quickest
    ///   way to answer one.
    ///
    /// Everything else falls through and does nothing, rather than being
    /// swallowed. A control with the keyboard must not stop the page scrolling
    /// with keys that mean nothing to it.
    fn press_key(&mut self, node: dom::NodeId, key: &sandbox::message::Key) {
        use sandbox::message::Key;
        let is_button = self.page.as_ref().is_some_and(|page| page.is_button(node));
        match key {
            Key::Insert(text) if text.contains('\n') => {
                if is_button {
                    return self.ask_to_send(node);
                }
                // Nothing was pressed, so no button is a successful control —
                // the same rule Enter in a one-line field follows.
                self.ask_to_send(node);
            }
            Key::Insert(text) if text == " " => {
                if is_button {
                    return self.ask_to_send(node);
                }
                if let Some((select, rect, options, on)) =
                    self.page.as_ref().and_then(|page| page.dropdown_of(node))
                {
                    self.open = Some(sandbox::message::Dropdown {
                        rect,
                        node: select.0 as u32,
                        options,
                        on: on as u32,
                    });
                    return;
                }
                let changed = self
                    .page
                    .as_ref()
                    .map(|page| page.press(node))
                    .unwrap_or_default();
                self.record(changed);
            }
            Key::Up | Key::Down => {
                let changed = self
                    .page
                    .as_ref()
                    .map(|page| page.step_option(node, matches!(key, Key::Up)))
                    .unwrap_or_default();
                self.record(changed);
            }
            _ => {}
        }
    }
}

impl Render for PageRenderer {
    fn render(
        &mut self,
        request: &ToChild,
        fetch: &mut dyn FnMut(&[String], net::RequestKind) -> Vec<Fetched>,
    ) -> Result<Rendered, String> {
        // Typing, focusing and choosing are re-renders of the page already
        // held, so they borrow the request it came from. Kept here rather than asked for
        // again: a keystroke that had to go back over the pipe for the
        // document's bytes would cross the boundary twice to move a cursor.
        let held;
        let request = match request {
            ToChild::Focus { at } => {
                self.focus_at(*at);
                held = self.last.clone();
                held.as_ref().ok_or("nothing has been rendered yet")?
            }
            ToChild::Type { key } => {
                self.apply(key);
                held = self.last.clone();
                held.as_ref().ok_or("nothing has been rendered yet")?
            }
            ToChild::Choose { node, index } => {
                self.choose(*node, *index);
                held = self.last.clone();
                held.as_ref().ok_or("nothing has been rendered yet")?
            }
            other => {
                self.last = Some(other.clone());
                other
            }
        };
        let ToChild::Render {
            body,
            content_type,
            width,
            top,
            height,
            origin,
            path,
            force_authored,
            force_document,
            zoom,
        } = request
        else {
            return Err("expected a render request".to_owned());
        };

        // Decoded here rather than by the parent, so the encoding sniffer stays
        // on the sandboxed side with every other parser.
        let (html, ..) = net::encoding::decode_document(body, content_type.as_deref());

        // Every subresource — images, stylesheets, `@import` chains, frames —
        // goes over the pipe. Nothing in this process opens a socket or a file.
        let mut loader = PipeLoader { fetch };
        let base = origin.as_ref().map(|origin| (origin, path.as_str()));
        // Both set is a request the parent never makes, and this side is where
        // messages from a stranger arrive — so it is decided rather than
        // assumed away. The author's layout wins, because it is the one that
        // shows the page as written; a reader given the wrong one of these can
        // at least see what they were denied.
        let page = crate::render::render_sized(
            &html,
            *width,
            *top,
            *height,
            crate::render::Settings {
                fill_height: false,
                force_authored: *force_authored,
                force_document: *force_document,
                zoom: *zoom,
                values: self.values.clone(),
                chosen: self.chosen.clone(),
                focus: self.focus.as_ref().map(|focus| match focus {
                    Focus::Typing { node, field } => (*node, Some(field.cursor())),
                    // A ring and no caret: there is nothing in a checkbox to
                    // put one in, and a caret drawn in one would be a cursor
                    // promising an insertion point that does not exist.
                    Focus::Pressable { node } => (*node, None),
                }),
            },
            &mut self.fonts,
            &mut loader,
            base,
        );

        self.force_authored = *force_authored;
        self.force_document = *force_document && !*force_authored;
        self.zoom = *zoom;
        let rendered = Rendered {
            pixels: page.pixmap.data().to_vec(),
            width: page.pixmap.width(),
            height: page.pixmap.height(),
            top: page.band_top,
            content_height: page.content_height,
            mode: mode_of(&page),
            title: page.title.clone(),
            links: links_of(&page),
            missing: missing_of(&page),
            buttons: page.buttons().into_iter().map(|(_, rect)| rect).collect(),
            pressables: page
                .pressables()
                .into_iter()
                .map(|(_, rect)| rect)
                .collect(),
            submit: self.submit.take(),
            open: self.open.take(),
            can_toggle_layout: self.can_toggle_layout(&page),
            focused: self.focused(),
            images_loaded: page.images_loaded as u32,
            background: packed(page.background),
        };
        // Kept for the questions that come after: find, and re-rendering at a
        // new width without re-fetching anything.
        self.page = Some(page);
        Ok(rendered)
    }

    fn band(&mut self, top: u32, height: u32) -> Result<Rendered, String> {
        // Remembered so that a keystroke re-renders the rows the reader is
        // looking at (#110). Typing is a re-render of the whole page, built
        // from the request the page came from — and that request names the band
        // the page *opened* at. Without this, clicking into a field halfway
        // down a long page would answer by painting the top of it.
        if let Some(ToChild::Render {
            top: at,
            height: rows,
            ..
        }) = &mut self.last
        {
            (*at, *rows) = (top, height);
        }
        let Some(page) = &self.page else {
            return Err("no page to paint a band of".to_owned());
        };
        // A frameset has no display list to repaint from, and needs none: its
        // canvas is its viewport, so there are no rows below the ones it holds.
        let Some(pixmap) = page.paint_band(&mut self.fonts, top, height) else {
            return Err("this page cannot be repainted a band at a time".to_owned());
        };
        Ok(Rendered {
            pixels: pixmap.data().to_vec(),
            width: pixmap.width(),
            height: pixmap.height(),
            top,
            content_height: page.content_height,
            // Unchanged by moving down the page, and re-sent because the
            // message is one shape: the parent replaces what it holds rather
            // than merging, so a band that omitted these would blank the tab's
            // title and every link on it.
            mode: mode_of(page),
            title: page.title.clone(),
            links: links_of(page),
            missing: missing_of(page),
            buttons: page.buttons().into_iter().map(|(_, rect)| rect).collect(),
            pressables: page
                .pressables()
                .into_iter()
                .map(|(_, rect)| rect)
                .collect(),
            // A band is a repaint of rows already laid out, and repainting is
            // not a thing anybody asked a form to be sent by, or a dropdown to
            // be opened by.
            submit: None,
            open: None,
            can_toggle_layout: self.can_toggle_layout(page),
            focused: self.focused(),
            images_loaded: page.images_loaded as u32,
            background: packed(page.background),
        })
    }

    fn accessibility(&mut self) -> sandbox::access::Tree {
        let Some(page) = &self.page else {
            return sandbox::access::Tree::default();
        };
        crate::access::tree_of_frames(
            page.frames
                .iter()
                .map(|frame| (&frame.doc, &frame.layout, (frame.rect.x, frame.rect.y))),
            self.focus.as_ref().map(Focus::node),
        )
    }

    fn find(&mut self, query: &str) -> Vec<layout::Rect> {
        // An empty query matches everything, which is not what a reader who has
        // just cleared the box wants to see.
        if query.trim().is_empty() {
            return Vec::new();
        }
        match &self.page {
            Some(page) => page.find(query),
            None => Vec::new(),
        }
    }

    fn select(&mut self, from: (f32, f32), to: (f32, f32)) -> (Vec<layout::Rect>, String) {
        match &self.page {
            Some(page) => {
                let selection = page.select(from, to);
                (selection.rects, selection.text)
            }
            None => (Vec::new(), String::new()),
        }
    }
}

/// Runs this process as a renderer child, reading from stdin and writing to
/// stdout.
///
/// Locked for the whole conversation: anything else writing to stdout would
/// interleave with a frame and corrupt it, and this is the one process where a
/// stray `println!` is a protocol violation rather than noise.
pub fn run_child() -> Result<(), Error> {
    // The renderer runs on a thread of its own, and a large one (#176).
    //
    // The cascade, layout and paint each recurse once per nesting level, so a
    // document's depth is the depth of three recursions in a row.
    // `dom::MAX_DEPTH` caps that depth; this is the other half of the same
    // decision, because a cap is only safe if a stack can hold it. Measured at
    // the cap: about 8 MiB in release and 32 MiB in debug, against a main
    // thread's 8 MiB — so release sat exactly on the edge and debug was well
    // past it.
    //
    // Reserved address space rather than memory: pages are committed as they
    // are touched, so an ordinary page costs what it always did.
    //
    // Spawned *before* the filter is installed, because `clone` is not on the
    // allowlist and putting it there to make room for this thread would put it
    // there for an attacker's thread too. The filter goes on inside the thread
    // instead, and reaches back over the main one.
    std::thread::Builder::new()
        .name("renderer".to_owned())
        .stack_size(dom::DEPTH_STACK)
        .spawn(render_until_the_parent_goes)
        .map_err(Error::Io)?
        .join()
        // A panic on the render thread has already said why on stderr.
        // Reported rather than resumed: resuming across a join loses the
        // original location and gains nothing.
        .unwrap_or(Err(Error::Died))
}

/// The renderer proper, on the thread [`run_child`] made for it.
fn render_until_the_parent_goes() -> Result<(), Error> {
    // Before anything is read. The very first frame carries the document, so it
    // is already attacker-influenced — confining afterwards would be confining
    // after the interesting bytes had arrived.
    //
    // The font store is built after this on purpose too: it reads only embedded
    // data (ADR-0010), and building it under the filter is the check that it
    // really does not touch the filesystem.
    //
    // Every thread and not just this one. The main thread is asleep in `join`
    // and is not where a compromise starts, but an attacker already inside the
    // renderer could aim at the address it returns to and land somewhere with
    // no filter on it. `TSYNC` closes that for the cost of a flag.
    let confinement = sandbox::confine::apply_to_every_thread();
    // Only when the platform *has* a sandbox and it failed anyway — a kernel
    // too old, or a container that forbids installing a filter. That is a fact
    // about this machine and worth a line every time.
    //
    // A platform with no implementation is a fact about the *build*, and saying
    // it here meant every spawned child said it: twenty lines in one test run,
    // which is how a warning becomes something people scroll past. The parent
    // says that one once, at startup.
    if confinement == sandbox::Confinement::Failed {
        // stderr, never stdout: stdout is the protocol.
        eprintln!("2kbrowser renderer: {}", confinement.describe());
    }

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    sandbox::child::serve(&mut input, &mut output, &mut PageRenderer::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(body: &[u8], width: u32) -> ToChild {
        ToChild::Render {
            body: body.to_vec(),
            content_type: None,
            width,
            top: 0,
            height: 2000,
            origin: None,
            path: String::new(),
            force_authored: false,
            force_document: false,
            zoom: 1.0,
        }
    }

    /// The same request, with one of the two layout overrides set.
    fn overriding(body: &[u8], width: u32, authored: bool, document: bool) -> ToChild {
        match request(body, width) {
            ToChild::Render {
                body,
                content_type,
                top,
                height,
                origin,
                path,
                ..
            } => ToChild::Render {
                body,
                content_type,
                width,
                top,
                height,
                origin,
                path,
                force_authored: authored,
                force_document: document,
                zoom: 1.0,
            },
            other => other,
        }
    }

    fn no_fetch(urls: &[String], _: net::RequestKind) -> Vec<Fetched> {
        vec![None; urls.len()]
    }

    /// A real PNG, so a decode that succeeds means the bytes arrived intact.
    fn tile() -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/ref/fixtures/assets/tile.png"),
        )
        .expect("the reference fixture tile")
    }

    #[test]
    fn the_child_renders_the_same_pixels_as_the_in_process_path() {
        // The property the whole boundary rests on: moving rendering across a
        // process changes nothing about what a page looks like. Byte-identical,
        // not merely similar — ADR-0005's determinism is what makes that a fair
        // thing to demand.
        let html = "<body bgcolor=\"#eef\"><h1>Heading</h1><p>Some <b>text</b> here.</p>\
                    <table border=1><tr><td>a</td><td>b</td></tr></table></body>";

        let mut fonts = FontStore::new();
        let direct = crate::render::render(html, 300, 2000, &mut fonts);

        let mut renderer = PageRenderer::new();
        let crossed = renderer
            .render(&request(html.as_bytes(), 300), &mut no_fetch)
            .expect("renders");

        assert_eq!(crossed.width, direct.pixmap.width());
        assert_eq!(crossed.height, direct.pixmap.height());
        assert_eq!(
            crossed.pixels,
            direct.pixmap.data(),
            "the child produced different pixels"
        );
        assert_eq!(crossed.content_height, direct.content_height);
        assert_eq!(crossed.title, direct.title);
    }

    #[test]
    fn a_title_and_a_mode_survive_the_crossing() {
        let html = "<title>Named</title><body><p>x</p></body>";
        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(&request(html.as_bytes(), 200), &mut no_fetch)
            .expect("renders");
        assert_eq!(page.title.as_deref(), Some("Named"));
        assert_eq!(page.mode, Mode::Authored);
        assert!(!page.can_toggle_layout, "nothing to overrule here");
    }

    #[test]
    fn the_document_fallback_crosses_with_its_share_intact() {
        // ADR-0009 forbids switching rendering mode silently, and the chrome
        // can only say what it was told — so the number has to survive.
        let html = "<body><div style=\"display: flex\">\
                    <div style=\"display: grid\">a</div></div></body>";
        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(&request(html.as_bytes(), 200), &mut no_fetch)
            .expect("renders");
        match page.mode {
            Mode::Document { unsupported_share } => {
                assert!(unsupported_share > 0.0, "{unsupported_share}");
                assert!(page.can_toggle_layout, "there is a decision to overrule");
            }
            other => panic!("expected the document fallback, got {other:?}"),
        }
    }

    #[test]
    fn an_ordinary_page_can_be_asked_for_the_document_fallback() {
        // The request that had nowhere to travel. `force_authored` returns a
        // fallback page to the author's layout; this is the other direction,
        // and it is not the absence of that one — an ordinary page classifies
        // as `Authored` and has no fallback to return to.
        let html = "<body><h1>Title</h1><p>An ordinary paragraph.</p></body>";

        let mut plain = PageRenderer::new();
        let ordinary = plain
            .render(&request(html.as_bytes(), 300), &mut no_fetch)
            .expect("renders");
        assert_eq!(
            ordinary.mode,
            Mode::Authored,
            "this fixture is only useful while it needs no fallback"
        );

        let mut forcing = PageRenderer::new();
        let forced = forcing
            .render(
                &overriding(html.as_bytes(), 300, false, true),
                &mut no_fetch,
            )
            .expect("renders");
        assert!(
            matches!(forced.mode, Mode::Document { .. }),
            "asking for the fallback across the boundary did not produce one: {:?}",
            forced.mode
        );
        // Not the same rendering wearing a different label: the reader sheet
        // replaces the author's, so the pixels have to differ.
        assert_ne!(
            ordinary.pixels, forced.pixels,
            "the forced fallback rendered identically to the author's layout"
        );
        assert!(
            forced.can_toggle_layout,
            "a reader who asked for this needs the way back"
        );
    }

    #[test]
    fn a_band_of_a_forced_page_still_offers_the_way_back() {
        // A band re-sends everything the bar reads, and the bar decides what
        // the toggle says. A band that forgot the override would blank the
        // control the moment the reader scrolled.
        let html = "<body><h1>Title</h1><p>An ordinary paragraph.</p></body>";
        let mut renderer = PageRenderer::new();
        renderer
            .render(
                &overriding(html.as_bytes(), 300, false, true),
                &mut no_fetch,
            )
            .expect("renders");
        let band = renderer.band(0, 200).expect("paints a band");
        assert!(
            band.can_toggle_layout,
            "the band lost the reader's override"
        );
    }

    #[test]
    fn a_band_carries_the_same_canvas_colour_as_the_page_it_is_part_of() {
        // For the same reason a band re-sends the mode and the title: the
        // parent replaces what it holds rather than merging. The window fills
        // the rows a band does not cover with this colour, so a band that
        // reported white would put a lit strip below every short page the
        // moment the reader scrolled — and the document fallback is dark.
        let html = "<body><h1>Title</h1><p>An ordinary paragraph.</p></body>";
        let mut renderer = PageRenderer::new();
        let whole = renderer
            .render(
                &overriding(html.as_bytes(), 300, false, true),
                &mut no_fetch,
            )
            .expect("renders");
        let band = renderer.band(0, 200).expect("paints a band");
        assert_eq!(
            band.background, whole.background,
            "the band reported a different canvas colour from its own page"
        );
    }

    #[test]
    fn a_forced_page_with_no_fallback_to_give_still_offers_the_way_back() {
        // A frameset is its own viewport and has no document fallback: it comes
        // back `Authored` however it was asked for. So the mode records nothing
        // about the reader having asked, and if the override is not remembered
        // separately the bar has no way to know there is anything to undo — it
        // goes on offering to simplify a page it has already been told to
        // simplify, and every press does nothing.
        let dir = std::env::temp_dir().join("2kbrowser-forced-frameset");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (origin, at) = net::parse_url(&net::file_url(&dir.join("page.html"))).expect("parses");
        let html = "<frameset cols=\"50%,50%\">\
                    <frame src=\"a.html\"><frame src=\"b.html\"></frameset>";

        let mut fetch = |urls: &[String], _kind: net::RequestKind| -> Vec<Fetched> {
            urls.iter()
                .map(|_| {
                    Some(sandbox::child::Resource {
                        bytes: b"<body><p>a frame</p></body>".to_vec(),
                        content_type: Some("text/html".to_owned()),
                    })
                })
                .collect()
        };

        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(
                &ToChild::Render {
                    body: html.as_bytes().to_vec(),
                    content_type: None,
                    width: 300,
                    top: 0,
                    height: 400,
                    origin: Some(origin),
                    path: at,
                    force_authored: false,
                    force_document: true,
                    zoom: 1.0,
                },
                &mut fetch,
            )
            .expect("renders");

        assert_eq!(
            page.mode,
            Mode::Authored,
            "a frameset has no fallback to give, which is what makes this the case worth pinning"
        );
        assert!(
            page.can_toggle_layout,
            "the reader's request left no trace, so the bar cannot offer to undo it"
        );
    }

    #[test]
    fn asking_for_both_layouts_at_once_gets_the_authors() {
        // The parent never sends this, and that is exactly why it is decided
        // here: the frame arrives on the untrusted side, and "cannot happen" is
        // not a property of a message somebody else wrote. The author's layout
        // wins, and the reported state has to agree with what was drawn rather
        // than with what was asked for.
        let html = "<body><h1>Title</h1><p>An ordinary paragraph.</p></body>";
        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(&overriding(html.as_bytes(), 300, true, true), &mut no_fetch)
            .expect("renders");
        assert_eq!(page.mode, Mode::Authored, "the author's layout lost");
        assert!(
            !renderer.force_document,
            "the losing override was still recorded, so the band would disagree"
        );
    }

    #[test]
    fn the_bytes_are_decoded_on_the_child_side() {
        // A page declaring nothing is windows-1252, and the sniffer belongs
        // with every other parser — on the far side of the boundary.
        let mut body = b"<body><p>".to_vec();
        // An em dash in windows-1252.
        body.push(0x97);
        body.extend_from_slice(b"</p></body>");

        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(&request(&body, 200), &mut no_fetch)
            .expect("renders");
        assert!(page.width > 0);
    }

    #[test]
    fn a_page_with_no_base_still_renders_and_carries_no_links() {
        // Link geometry is resolved against a base; without one there is
        // nothing to navigate to, so the parent gets none rather than getting
        // unresolved ones it would have to interpret.
        let html = "<body><a href=\"b.html\">there</a></body>";
        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(&request(html.as_bytes(), 200), &mut no_fetch)
            .expect("renders");
        assert!(page.links.is_empty());
    }

    #[test]
    fn links_cross_already_resolved_to_absolute_urls() {
        let dir = std::env::temp_dir().join("2kbrowser-isolated-tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("a.html");
        let html = "<body><p><a href=\"b.html\">there</a></p></body>";
        std::fs::write(&path, html).expect("write");
        let (origin, at) = net::parse_url(&net::file_url(&path)).expect("parses");

        let mut renderer = PageRenderer::new();
        let page = renderer
            .render(
                &ToChild::Render {
                    body: html.as_bytes().to_vec(),
                    content_type: None,
                    width: 300,
                    top: 0,
                    height: 2000,
                    origin: Some(origin),
                    path: at,
                    force_authored: false,
                    force_document: false,
                    zoom: 1.0,
                },
                &mut no_fetch,
            )
            .expect("renders");

        assert_eq!(page.links.len(), 1, "{:?}", page.links);
        assert!(
            page.links[0].url.ends_with("/b.html"),
            "{}",
            page.links[0].url
        );
        assert!(
            page.links[0].url.starts_with("file:///"),
            "resolved absolutely, so the parent never resolves anything: {}",
            page.links[0].url
        );
    }

    #[test]
    fn every_subresource_is_asked_for_rather_than_fetched() {
        // The property this whole change exists for. Nothing in the child may
        // open a socket or a file: a stylesheet, an `@import` inside it, and an
        // image all have to arrive as answers to requests.
        let dir = std::env::temp_dir().join("2kbrowser-pipe-loader");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (origin, at) = net::parse_url(&net::file_url(&dir.join("page.html"))).expect("parses");

        let html = "<html><head><link rel=\"stylesheet\" href=\"site.css\"></head>\
                    <body><img src=\"tile.png\"><p>text</p></body></html>";

        let mut asked: Vec<String> = Vec::new();
        let mut one = |url: &str| -> Fetched {
            asked.push(url.to_owned());
            if url.ends_with("site.css") {
                return Some(sandbox::child::Resource {
                    bytes: b"@import url(more.css); p { color: #ff0000 }".to_vec(),
                    content_type: Some("text/css".to_owned()),
                });
            }
            if url.ends_with("more.css") {
                return Some(sandbox::child::Resource {
                    bytes: b"body { background: #00ff00 }".to_vec(),
                    content_type: None,
                });
            }
            if url.ends_with("tile.png") {
                return Some(sandbox::child::Resource {
                    bytes: tile(),
                    content_type: Some("image/png".to_owned()),
                });
            }
            None
        };
        let mut fetch = |urls: &[String], _kind: net::RequestKind| -> Vec<Fetched> {
            urls.iter().map(|url| one(url)).collect()
        };

        let page = PageRenderer::new()
            .render(
                &ToChild::Render {
                    body: html.as_bytes().to_vec(),
                    content_type: None,
                    width: 300,
                    top: 0,
                    height: 600,
                    origin: Some(origin),
                    path: at,
                    force_authored: false,
                    force_document: false,
                    zoom: 1.0,
                },
                &mut fetch,
            )
            .expect("renders");

        assert!(
            asked.iter().any(|url| url.ends_with("site.css")),
            "the stylesheet was not asked for: {asked:?}"
        );
        assert!(
            asked.iter().any(|url| url.ends_with("more.css")),
            "the @import inside it was not asked for: {asked:?}"
        );
        assert!(
            asked.iter().any(|url| url.ends_with("tile.png")),
            "the image was not asked for: {asked:?}"
        );

        // And the answers were actually used. The imported sheet paints the
        // body green, which nothing else in this page does.
        let green = page
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] < 80 && p[1] > 150 && p[2] < 80)
            .count();
        assert!(green > 0, "the imported stylesheet did not reach the paint");
    }

    #[test]
    fn a_subresource_the_parent_refuses_is_simply_absent() {
        // A refusal and a failure look identical to the child, and both render
        // as "no image" rather than as an error. Nothing about the parent's
        // policy leaks across.
        let dir = std::env::temp_dir().join("2kbrowser-pipe-loader");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (origin, at) = net::parse_url(&net::file_url(&dir.join("page.html"))).expect("parses");

        let html = "<body><img src=\"https://tracker.example.net/pixel.gif\"><p>text</p></body>";
        let mut refused = 0usize;
        let mut fetch = |urls: &[String], _kind: net::RequestKind| -> Vec<Fetched> {
            refused += urls.len();
            vec![None; urls.len()]
        };

        let page = PageRenderer::new()
            .render(
                &ToChild::Render {
                    body: html.as_bytes().to_vec(),
                    content_type: None,
                    width: 200,
                    top: 0,
                    height: 200,
                    origin: Some(origin),
                    path: at,
                    force_authored: false,
                    force_document: false,
                    zoom: 1.0,
                },
                &mut fetch,
            )
            .expect("renders anyway");

        assert_eq!(refused, 1, "it was asked for exactly once");
        assert!(page.width > 0, "the page still rendered");
    }

    #[test]
    fn a_page_nested_far_past_the_cap_still_renders() {
        // #176, end to end. The cascade, layout and paint each recurse once per
        // nesting level, so this used to be three stack overflows waiting for a
        // page deep enough — and what died was the renderer process, taking the
        // page with it.
        //
        // Two things fixed it and both are load-bearing: `dom::MAX_DEPTH` caps
        // the tree the parser builds, and the renderer runs on a stack sized
        // for that cap. This test needs the second as much as the first,
        // because a libtest thread gets 2 MiB and the cap costs around 32 in a
        // debug build.
        let deep = format!(
            "<body>{}<p>bottom</p>{}</body>",
            "<div>".repeat(dom::MAX_DEPTH * 8),
            "</div>".repeat(dom::MAX_DEPTH * 8),
        );
        let rendered = std::thread::Builder::new()
            .stack_size(dom::DEPTH_STACK)
            .spawn(move || {
                let mut renderer = PageRenderer::new();
                renderer
                    .render(&request(deep.as_bytes(), 300), &mut no_fetch)
                    .map(|page| page.pixels.len())
            })
            .expect("a thread")
            .join()
            .expect("the renderer fell over");
        assert!(rendered.expect("renders") > 0);
    }
}
