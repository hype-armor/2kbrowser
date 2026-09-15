//! Form controls, as boxes on a line.
//!
//! The era's web is full of forms — a search box, a login, a mailing-list
//! signup — and until now this engine drew none of them. An `<input>` is a void
//! inline element with no content, so it laid out as nothing at all: a search
//! box was not an empty box, it was absent, and a login form was a column of
//! labels with no fields beside them. That reads as a broken page rather than
//! as a browser that cannot submit forms, which is the honest thing for it to
//! look like.
//!
//! # Why they are atomic inlines
//!
//! A control is the same shape of thing as an image: it sits *on* a line, takes
//! a size of its own, and has no inline content the line breaker can look
//! inside. That is exactly [`text::ReplacedInline`], which images already use,
//! so controls reuse it rather than needing `display: inline-block` — which is
//! itself unimplemented here and would have made this wait on it.
//!
//! What a control does *not* share with an image is where its picture comes
//! from. An image is painted from decoded bytes; a control is painted from its
//! own style — a border and a background out of the UA stylesheet — plus a
//! label shaped into it. So the box is built here and marked as carrying no
//! image, and paint draws it like any other bordered box.
//!
//! # What is here and what is not
//!
//! Text fields and `<textarea>`s can be typed into (#110), a checkbox or a
//! radio can be ticked, a dropdown can be chosen from, and a form can be sent.
//! The interaction itself is not here — it lives with the renderer child, which
//! owns the focus and the pointer — but this is where a control's *answer* is
//! read, and that is where the two meet: what a reader has typed or ticked
//! comes from the document's own record of it, and only then from the markup.
//! The two are different things in HTML and are kept different here, because
//! `<input value="x" checked>` is the field's default rather than its contents.
//!
//! [`press`] is the other half of that: what a press *means* for a control,
//! which differs for each and is one function so that no caller has to know
//! which rule applies to the thing under the pointer.
//!
//! What is still missing is the keyboard. Tab reaches the text controls and
//! stops there, so a checkbox can be ticked with a pointer and by no other
//! means — which is a gap in reach rather than in what a form can say, and is
//! filed as #151 rather than fixed here.
//!
//! What *is* here besides the controls themselves is
//! [`break_the_rule_for_a_legend`], because a `<fieldset>`'s rule and the
//! `<legend>` that breaks it are the same piece of HTML furniture as the
//! controls they surround.

use css::style::ComputedStyle;
use dom::{Document, NodeId};

use crate::LayoutBox;

/// A form control this engine knows how to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// A single-line text field, including the types that are one in all but
    /// name — `search`, `email`, `url`, `tel`, `password` and the rest.
    Text,
    /// A text field that shows bullets instead of what it holds.
    Password,
    /// A push button: `<button>`, or an `input` of type `submit`, `reset` or
    /// `button`.
    Button,
    /// A checkbox.
    Checkbox,
    /// A radio button, drawn round so that it is not mistaken for a checkbox.
    Radio,
    /// A multi-line text field.
    TextArea,
    /// A dropdown. Which option it shows is decided in the cascade, by hiding
    /// the others; this only draws the box around it.
    Select,
}

/// Default width of a text field, in characters, when `size` says nothing.
///
/// Twenty is what HTML has always specified and what every browser uses.
const DEFAULT_TEXT_COLUMNS: f32 = 20.0;

/// Default `<textarea>` size, in characters and lines.
const DEFAULT_AREA_COLUMNS: f32 = 20.0;
/// Rows a `<textarea>` shows when it does not say.
const DEFAULT_AREA_ROWS: f32 = 2.0;

/// Nominal width of a character, as a fraction of the font size.
///
/// A form field is sized in *characters* by markup written before anyone could
/// measure one, so this converts the era's unit into ours. It is an
/// approximation by construction: the attribute means "about this many
/// characters" even in a browser, since the field is not monospaced.
const CHARACTER_WIDTH: f32 = 0.5;

/// Side of a checkbox or radio, as a fraction of the font size.
const TICK_BOX: f32 = 0.8;

/// Which control an element is, if it is one.
///
/// `hidden` is deliberately absent: it is given `display: none` by the UA
/// stylesheet, so it never reaches layout and does not need a variant here.
/// `image` and `file` are absent too — the first is an image with a form
/// attached and would want the image path, the second a control this engine has
/// no business pretending to offer.
pub fn control_of(doc: &Document, node: NodeId) -> Option<Control> {
    let element = doc.element(node)?;
    match element.local_name() {
        "textarea" => Some(Control::TextArea),
        "select" => Some(Control::Select),
        "button" => Some(Control::Button),
        "input" => {
            // A missing or unknown `type` is a text field, which is what the
            // HTML specification says and what makes a page using an input type
            // this engine has never heard of still draw something sensible.
            let kind = element
                .attr("type")
                .map(|value| value.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "text".to_owned());
            match kind.as_str() {
                "hidden" | "image" | "file" => None,
                "checkbox" => Some(Control::Checkbox),
                "radio" => Some(Control::Radio),
                "submit" | "reset" | "button" => Some(Control::Button),
                "password" => Some(Control::Password),
                _ => Some(Control::Text),
            }
        }
        _ => None,
    }
}

/// The text a control shows, if it shows any.
///
/// A `<button>` and a `<textarea>` carry their label as content, an `input`
/// carries it in an attribute, and a `submit` with neither still says `Submit`
/// because that is what it does — a blank button is not what the markup meant.
pub fn label_of(doc: &Document, node: NodeId, control: Control) -> Option<String> {
    let element = doc.element(node)?;
    match control {
        Control::Checkbox | Control::Radio => None,
        // The box is atomic, so the options are no longer laid out as content
        // of their own — whatever the control shows, it shows as its label.
        // A closed dropdown shows the one it is open on; a list box shows them
        // stacked, which is why they arrive separated by newlines and are
        // shaped as preformatted text.
        Control::Select => {
            let options = options_of(doc, node);
            if is_list_box(element) {
                let text = options
                    .iter()
                    .map(|&id| descendant_text(doc, id).trim().to_owned())
                    .collect::<Vec<_>>()
                    .join("\n");
                return (!text.trim().is_empty()).then_some(text);
            }
            selected_option(doc, node).map(|id| descendant_text(doc, id).trim().to_owned())
        }
        // `<button>Label</button>`, whose label is its content. A `<textarea>`
        // was in this arm too and is not any more: it reads what has been typed
        // in it before it reads what the markup said, and this arm reads only
        // the markup — so it quietly shadowed the one below and nothing typed
        // into a textarea ever appeared (#110).
        Control::Button if element.local_name() != "input" => {
            let text = descendant_text(doc, node);
            (!text.trim().is_empty()).then_some(text)
        }
        Control::Button => {
            let default = match element.attr("type").map(str::trim) {
                Some("reset") => "Reset",
                Some("button") => "",
                _ => "Submit",
            };
            let value = element.attr("value").unwrap_or(default).to_owned();
            (!value.is_empty()).then_some(value)
        }
        Control::Password => {
            // Bullets, not the value. A password field that renders its own
            // contents over the shoulder of whoever is reading the page is the
            // one way this could be worse than drawing nothing.
            let len = value_of(doc, node).chars().count();
            (len > 0).then(|| "\u{2022}".repeat(len))
        }
        Control::Text => {
            let value = value_of(doc, node);
            (!value.is_empty()).then_some(value)
        }
        Control::TextArea => {
            let text = value_of(doc, node);
            // Not trimmed, unlike the button labels above. A field somebody has
            // emptied holds an empty string, and treating that as "say nothing"
            // would put the markup's original text back on screen the moment
            // the last character was deleted (#110).
            match doc.value_of(node) {
                Some(_) => Some(text),
                None => (!text.trim().is_empty()).then_some(text),
            }
        }
    }
}

/// What a text control holds: what has been typed in it, or what the markup
/// said (#110).
///
/// The two are different things in HTML and this keeps them that way — the
/// attribute is the field's default, and what a reader has typed is a property
/// of the control. Which is also why this reads the document's own record
/// rather than the attribute once anything has been typed.
pub fn value_of(doc: &Document, node: NodeId) -> String {
    let Some(element) = doc.element(node) else {
        return String::new();
    };
    if let Some(typed) = doc.value_of(node) {
        return typed.to_owned();
    }
    if element.local_name() == "textarea" {
        return descendant_text(doc, node);
    }
    element.attr("value").unwrap_or_default().to_owned()
}

/// All the text under a node, joined.
fn descendant_text(doc: &Document, node: NodeId) -> String {
    let mut out = String::new();
    collect_text(doc, node, &mut out);
    out
}

fn collect_text(doc: &Document, node: NodeId, out: &mut String) {
    for &child in doc.children(node) {
        if let Some(text) = doc.text(child) {
            out.push_str(text);
        } else {
            collect_text(doc, child, out);
        }
    }
}

/// The size a control asks for, as a *content* box in pixels.
///
/// Sized from the era's own units — `size`, `cols` and `rows` are counts of
/// characters and lines — rather than from a table of pixel defaults, so a
/// field follows the font it is set in rather than staying 13px tall on a page
/// that asked for large text.
pub fn intrinsic_size(
    doc: &Document,
    node: NodeId,
    style: &ComputedStyle,
    control: Control,
    label: Option<&str>,
) -> (f32, f32) {
    let font_size = style.font_size;
    // `line-height: normal` is the face's own ascent, descent and line gap, and
    // measuring those needs the shaper, which a control's *intrinsic* size is
    // computed too far from to reach. The constant the cascade used to apply to
    // everything stands in, which is about 1.5% tall for the faces bundled here
    // — a rounding error on a box whose height is `rows` lines of a field
    // nobody can type in, and which this module already sizes in the era's own
    // approximate units.
    let line = style
        .line_height
        .resolve(font_size, font_size * css::style::NORMAL_LINE_HEIGHT)
        .max(font_size);
    let columns = |name: &str, fallback: f32| -> f32 {
        doc.element(node)
            .and_then(|element| element.attr(name))
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|count| *count > 0.0)
            .unwrap_or(fallback)
    };

    match control {
        Control::Checkbox | Control::Radio => {
            let side = (font_size * TICK_BOX).round().max(1.0);
            (side, side)
        }
        Control::Text | Control::Password => {
            let width = columns("size", DEFAULT_TEXT_COLUMNS) * font_size * CHARACTER_WIDTH;
            (width, line)
        }
        Control::TextArea => {
            let width = columns("cols", DEFAULT_AREA_COLUMNS) * font_size * CHARACTER_WIDTH;
            (width, columns("rows", DEFAULT_AREA_ROWS) * line)
        }
        Control::Button => {
            // Wide enough for its label, with a floor so an empty button is
            // still a button rather than a hairline.
            let characters = label.map(|text| text.chars().count() as f32).unwrap_or(0.0);
            let width = (characters * font_size * CHARACTER_WIDTH).max(font_size);
            (width, line)
        }
        Control::Select => {
            // As wide as what it shows, plus room for the arrow a browser
            // draws. Nothing draws the arrow here and the room is kept anyway,
            // so the box is not tight against its own text.
            let widest = label
                .map(|text| {
                    text.lines()
                        .map(|line| line.chars().count())
                        .max()
                        .unwrap_or(0) as f32
                })
                .unwrap_or(0.0);
            let width = (widest * font_size * CHARACTER_WIDTH).max(font_size) + font_size;
            let list = doc.element(node).is_some_and(is_list_box);
            let rows = if list {
                // `size` if it gave one, else however many options there are —
                // a list box shows its list rather than a slice of it.
                let shown = columns("size", options_of(doc, node).len().max(1) as f32);
                shown.max(1.0)
            } else {
                1.0
            };
            (width, line * rows)
        }
    }
}

/// Whether a `select` is drawn as a list rather than a closed dropdown.
///
/// The same rule the cascade uses to decide which options to hide. Stated twice
/// because the two answer different questions from different places, and a
/// `select` that measured as a list and drew as a dropdown would be worse than
/// either.
pub fn is_list_box(select: &dom::ElementData) -> bool {
    select.attr("multiple").is_some()
        || select
            .attr("size")
            .and_then(|value| value.trim().parse::<u32>().ok())
            .is_some_and(|size| size > 1)
}

/// Whether a control shows one line of text or several.
///
/// This decides whether a label that does not fit is wrapped or cut. A text
/// field has no second line to put anything on: a value too long for the box
/// shows its beginning and stops at the border. Wrapping instead draws the rest
/// of the value straight through the border and over whatever is below it,
/// which is what this engine did until a reference fixture put a long value in
/// a short field.
pub fn is_single_line(doc: &Document, node: NodeId, control: Control) -> bool {
    match control {
        Control::TextArea => false,
        Control::Select => !doc.element(node).is_some_and(is_list_box),
        Control::Text
        | Control::Password
        | Control::Button
        | Control::Checkbox
        | Control::Radio => true,
    }
}

/// How a form's data reaches the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// In the URL's query string.
    Get,
    /// In a request body.
    Post,
}

/// A form, collected and ready to send (#110).
///
/// Assembled on this side of the renderer boundary because the form is part of
/// the document, and the document never leaves it (ADR-0012). What crosses is
/// this: a destination as the markup wrote it, a method, and the encoded pairs.
/// The parent resolves the destination, applies the network policy to it, and
/// decides whether anything is sent at all — which is the half that must not be
/// decided by a stranger's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// The `action` attribute as written, empty when it said nothing.
    ///
    /// Empty means the document's own URL, which is what HTML says and what the
    /// era's forms rely on most.
    pub action: String,
    /// `get` or `post`.
    pub method: Method,
    /// The successful controls, `application/x-www-form-urlencoded`.
    pub body: String,
}

/// What pressing a control should change, as entries to record.
///
/// Returns nothing when the press changes nothing, so a caller can skip a
/// repaint. It reads the document rather than writing it, for the same reason
/// typing does: what a reader has changed is kept beside the document and
/// applied to it on the next render, so that a re-parse of the same markup
/// lands in the same state rather than losing everything they answered.
///
/// One entry point, because the caller holding the document should not have to
/// know which of three rules applies to the thing under the pointer:
///
/// * A **checkbox** flips. Both directions matter equally — the case this was
///   written for is a form with a pre-ticked "send me email" box, which until
///   now could only be submitted with the box still ticked.
/// * A **radio** turns on, and turns off every other radio in its group. That
///   exclusion is the whole difference between a radio and a second checkbox,
///   and it belongs here rather than in the caller because a browser that
///   forgot it would send two answers to a question that has one.
/// * An **option** turns on. In a closed dropdown it turns its siblings off,
///   for the same reason; in a `<select multiple>` list box it flips, because
///   there the reader is picking a set.
///
/// Turning a radio *off* by pressing it again is deliberately not offered.
/// HTML cannot express "none of these" once one is chosen, so a reader who
/// reached that state would be submitting something no server expects.
pub fn press(doc: &Document, node: NodeId) -> Vec<(NodeId, bool)> {
    let Some(control) = control_of(doc, node) else {
        if doc
            .element(node)
            .is_some_and(|element| element.local_name() == "option")
        {
            return press_option(doc, node);
        }
        return Vec::new();
    };
    match control {
        Control::Checkbox => vec![(node, !doc.is_on(node))],
        Control::Radio if doc.is_on(node) => Vec::new(),
        Control::Radio => radio_group(doc, node)
            .into_iter()
            .map(|other| (other, other == node))
            .collect(),
        _ => Vec::new(),
    }
}

/// Choosing one option of a `<select>`.
fn press_option(doc: &Document, option: NodeId) -> Vec<(NodeId, bool)> {
    let Some(select) = enclosing_select(doc, option) else {
        return Vec::new();
    };
    if doc.element(select).is_some_and(is_list_box) {
        return vec![(option, !doc.is_on(option))];
    }
    if selected_option(doc, select) == Some(option) {
        return Vec::new();
    }
    options_of(doc, select)
        .into_iter()
        .map(|other| (other, other == option))
        .collect()
}

/// The `<select>` an option is in, descending back out through any `<optgroup>`.
pub fn enclosing_select(doc: &Document, option: NodeId) -> Option<NodeId> {
    doc.ancestors(option).find(|&id| {
        doc.element(id)
            .is_some_and(|it| it.local_name() == "select")
    })
}

/// Every radio a radio shares its question with.
///
/// Same `name`, same form — which is what HTML says a group is. A radio with no
/// name is in no group and is only ever itself: it can never be submitted, so
/// grouping the unnamed ones together would make pressing one silently unset
/// another for no gain.
fn radio_group(doc: &Document, node: NodeId) -> Vec<NodeId> {
    let name = doc
        .element(node)
        .and_then(|element| element.attr("name"))
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let Some(name) = name else {
        return vec![node];
    };
    let form = form_of(doc, node);
    doc.descendants(doc.root())
        .into_iter()
        .filter(|&id| control_of(doc, id) == Some(Control::Radio))
        .filter(|&id| form_of(doc, id) == form)
        .filter(|&id| {
            doc.element(id)
                .and_then(|element| element.attr("name"))
                .is_some_and(|other| other == name)
        })
        .collect()
}

/// The `<form>` a control belongs to, if any.
///
/// By containment only. HTML5's `form` attribute, which lets a control name a
/// form it is not inside, is not read — the era's markup does not use it, and a
/// control that claimed to belong to a form somewhere else would be a way for a
/// page to send one form's contents to another's destination.
pub fn form_of(doc: &Document, node: NodeId) -> Option<NodeId> {
    doc.ancestors(node).find(|&id| {
        doc.element(id)
            .is_some_and(|element| element.local_name() == "form")
    })
}

/// Collects a form's successful controls, in document order.
///
/// "Successful" is HTML 4 §17.13.2's word and its rules: a control contributes
/// only if it has a `name`, is not disabled, and — for a checkbox or a radio —
/// is checked. A submit button contributes only when it is the one that was
/// pressed, which is why `submitter` is asked for rather than inferred: a form
/// with `<button name="action" value="delete">` beside `value="save"` means
/// entirely different things depending on which was pressed, and guessing would
/// pick one of them.
///
/// Not here: `type="file"`, which this engine does not draw and must not
/// pretend to offer, and `type="image"`, which submits coordinates.
pub fn submission(doc: &Document, form: NodeId, submitter: Option<NodeId>) -> Submission {
    let element = doc.element(form);
    let action = element
        .and_then(|form| form.attr("action"))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let method = match element
        .and_then(|form| form.attr("method"))
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("post") => Method::Post,
        // Anything else is `get`, including a method this browser has never
        // heard of. HTML says an invalid value is the default, and the default
        // is the one that cannot change anything on the far side.
        _ => Method::Get,
    };

    let mut pairs: Vec<(String, String)> = Vec::new();
    for node in doc.descendants(form) {
        // A control inside a nested form belongs to that one. Nested forms are
        // invalid HTML and the parser usually drops the inner one, but a
        // document arrives from a stranger and this is cheaper than trusting it.
        if form_of(doc, node) != Some(form) {
            continue;
        }
        let Some(control) = control_of(doc, node) else {
            continue;
        };
        let Some(element) = doc.element(node) else {
            continue;
        };
        if element.attr("disabled").is_some() {
            continue;
        }
        let Some(name) = element.attr("name").filter(|name| !name.is_empty()) else {
            continue;
        };
        let name = name.to_owned();
        match control {
            Control::Checkbox | Control::Radio => {
                if doc.is_on(node) {
                    // HTML's default for a ticked box with no value of its own.
                    pairs.push((name, element.attr("value").unwrap_or("on").to_owned()));
                }
            }
            Control::Button => {
                // Only the button that was pressed, and never a reset.
                if submitter == Some(node) && !is_reset(element) {
                    pairs.push((name, button_value(doc, node, element)));
                }
            }
            Control::Select => {
                for option in selected_options(doc, node) {
                    pairs.push((name.clone(), option_value(doc, option)));
                }
            }
            Control::Text | Control::Password | Control::TextArea => {
                pairs.push((name, value_of(doc, node)));
            }
        }
    }

    Submission {
        action,
        method,
        body: urlencoded(&pairs),
    }
}

/// Whether a button resets rather than submits.
fn is_reset(element: &dom::ElementData) -> bool {
    element
        .attr("type")
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("reset"))
}

/// What a button sends: its `value`, or its content for a `<button>`.
fn button_value(doc: &Document, node: NodeId, element: &dom::ElementData) -> String {
    element
        .attr("value")
        .map(str::to_owned)
        .unwrap_or_else(|| match element.local_name() {
            "button" => descendant_text(doc, node).trim().to_owned(),
            _ => String::new(),
        })
}

/// Every option a `<select>` sends.
///
/// One for a dropdown, and however many are marked for a `multiple` list. A
/// list box with nothing marked sends nothing, which is where it differs from
/// the dropdown that shows its first option by default.
fn selected_options(doc: &Document, select: NodeId) -> Vec<NodeId> {
    let element = doc.element(select);
    if element.is_some_and(is_list_box) {
        return options_of(doc, select)
            .into_iter()
            .filter(|&id| doc.is_on(id))
            .collect();
    }
    selected_option(doc, select).into_iter().collect()
}

/// What an `<option>` reads as: the text it shows, or its `label` attribute
/// where it has one and no text.
///
/// The label a dropdown's list has to draw, which is not always what it sends —
/// `<option value="uk">United Kingdom</option>` sends one and shows the other,
/// and a list drawn from the values would be a list of codes.
pub fn option_label(doc: &Document, option: NodeId) -> String {
    let text = descendant_text(doc, option).trim().to_owned();
    if !text.is_empty() {
        return text;
    }
    doc.element(option)
        .and_then(|element| element.attr("label"))
        .unwrap_or_default()
        .to_owned()
}

/// What an `<option>` sends: its `value`, or the text it shows.
fn option_value(doc: &Document, option: NodeId) -> String {
    doc.element(option)
        .and_then(|element| element.attr("value"))
        .map(str::to_owned)
        .unwrap_or_else(|| descendant_text(doc, option).trim().to_owned())
}

/// Encodes pairs as `application/x-www-form-urlencoded`.
///
/// The one encoding here. `multipart/form-data` exists for file upload, which
/// this engine does not offer, and `text/plain` is a curiosity almost nothing
/// reads — offering either would be more shapes of "sent it wrong" for no page
/// that needs them.
fn urlencoded(pairs: &[(String, String)]) -> String {
    let mut out = String::new();
    for (name, value) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        encode_into(name, &mut out);
        out.push('=');
        encode_into(value, &mut out);
    }
    out
}

/// One field, percent-encoded with a space as `+`.
fn encode_into(text: &str, out: &mut String) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            // Everything else by its bytes, which is what makes a value in a
            // language other than English arrive as what was typed rather than
            // as question marks.
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
}

/// Every `option` under a `select`, in document order.
pub fn options_of(doc: &Document, node: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    collect_options(doc, node, &mut out);
    out
}

/// The option a closed dropdown displays: the last `selected`, or the first.
///
/// The cascade hides the others by the same rule. That is a *style* decision
/// and this is measuring and labelling, so the rule is applied here rather than
/// read back out of the computed styles.
fn selected_option(doc: &Document, node: NodeId) -> Option<NodeId> {
    let options = options_of(doc, node);
    options
        .iter()
        .rev()
        .find(|&&id| doc.is_on(id))
        .or(options.first())
        .copied()
}

/// Lifts a `<fieldset>`'s `<legend>` into the top rule and cuts the rule around
/// it. Returns how far the fieldset's border box has to move down to make room.
///
/// CSS 2.1 says nothing about either element; this is HTML's rendering section,
/// and it is the one piece of form furniture that no combination of CSS 2.1
/// properties can express. The rule runs through the legend's *middle*, so half
/// the legend is above the fieldset's border box and half is below it:
///
/// ```text
///   ┌─ A legend ────────────────┐      the rule stops either side of the text
///   │                           │      and the text straddles it
/// ```
///
/// Three things follow, and all three are done here rather than in the caller,
/// because they are one rule and only make sense together:
///
/// * The legend shrinks to fit. A full-width legend would cut the whole rule
///   away and leave the box open at the top. Its width comes from the text it
///   already laid out — the widest line it produced — which is shrink-to-fit
///   for anything that fitted on one line, and every legend does.
/// * The legend moves up to straddle the rule, and the rest of the group's
///   contents move up by what is left of the legend's slot in flow. What
///   remains of that slot is the half of the legend standing above the rule,
///   which is exactly the distance the whole box then moves down — so the
///   legend's top edge ends up where the fieldset's border box began, and
///   nothing above the fieldset is trodden on.
/// * The span of the top border the legend crosses is recorded on the box, for
///   paint to leave undrawn.
///
/// Only the fieldset's *first* child is a legend in this sense, which is what
/// HTML says and what stops a second one from cutting a second hole.
pub(crate) fn break_the_rule_for_a_legend(
    doc: &Document,
    node: NodeId,
    border_top: f32,
    box_: &mut LayoutBox,
) -> f32 {
    if doc
        .element(node)
        .is_none_or(|element| element.local_name() != "fieldset")
    {
        return 0.0;
    }
    let is_legend = |child: &LayoutBox| {
        child
            .node
            .and_then(|id| doc.element(id))
            .is_some_and(|element| element.local_name() == "legend")
    };
    let Some((legend, rest)) = box_
        .children
        .split_first_mut()
        .filter(|(a, _)| is_legend(a))
    else {
        return 0.0;
    };

    // Shrink to fit, keeping the left edge where flow put it.
    if let Some(text) = &legend.text {
        let surround = legend.rect.width - legend.content_width;
        legend.content_width = text.width;
        legend.rect.width = text.width + surround;
    }

    let height = legend.rect.height;
    // Half the legend stands above the rule; the other half, plus the rule
    // itself, is the part of its slot in flow that nothing needs any more.
    let above = ((height - border_top) / 2.0).max(0.0);
    let reclaimed = (height - above).max(0.0);

    legend.rect.y = -above;
    box_.top_border_gap = Some((legend.rect.x, legend.rect.x + legend.rect.width));

    for child in rest {
        child.rect.y -= reclaimed;
    }
    box_.rect.height = (box_.rect.height - reclaimed).max(0.0);
    above
}

fn collect_options(doc: &Document, node: NodeId, out: &mut Vec<NodeId>) {
    for &child in doc.children(node) {
        let Some(element) = doc.element(child) else {
            continue;
        };
        if element.local_name() == "option" {
            out.push(child);
        } else {
            collect_options(doc, child, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_of(html: &str, tag: &str) -> (Document, NodeId) {
        let doc = dom::parse(html);
        let node = doc.find_element(tag).expect("the element");
        (doc, node)
    }

    #[test]
    fn an_input_with_no_type_is_a_text_field() {
        // What HTML says, and what keeps a page using a type this engine has
        // never heard of drawing something sensible rather than nothing.
        let (doc, node) = node_of("<input>", "input");
        assert_eq!(control_of(&doc, node), Some(Control::Text));
        let (doc, node) = node_of(r#"<input type="colour-picker-9000">"#, "input");
        assert_eq!(control_of(&doc, node), Some(Control::Text));
    }

    #[test]
    fn the_types_that_generate_no_box_are_refused() {
        for kind in ["hidden", "image", "file"] {
            let (doc, node) = node_of(&format!(r#"<input type="{kind}">"#), "input");
            assert_eq!(control_of(&doc, node), None, "type={kind}");
        }
    }

    #[test]
    fn a_password_field_shows_bullets_and_never_its_value() {
        // The one way drawing a control could be worse than drawing nothing.
        let (doc, node) = node_of(r#"<input type="password" value="hunter2">"#, "input");
        let label = label_of(&doc, node, Control::Password).expect("bullets");
        assert_eq!(label, "\u{2022}".repeat(7));
        assert!(!label.contains("hunter"), "the value reached the page");
    }

    #[test]
    fn a_submit_button_with_no_value_still_says_what_it_does() {
        let (doc, node) = node_of(r#"<input type="submit">"#, "input");
        assert_eq!(
            label_of(&doc, node, Control::Button).as_deref(),
            Some("Submit")
        );
        let (doc, node) = node_of(r#"<input type="reset">"#, "input");
        assert_eq!(
            label_of(&doc, node, Control::Button).as_deref(),
            Some("Reset")
        );
        // A plain `button` type has no job to name, so it stays blank rather
        // than being given a word the markup never used.
        let (doc, node) = node_of(r#"<input type="button">"#, "input");
        assert_eq!(label_of(&doc, node, Control::Button), None);
    }

    #[test]
    fn a_button_element_takes_its_label_from_its_content() {
        let (doc, node) = node_of("<button>Cancel <b>now</b></button>", "button");
        assert_eq!(
            label_of(&doc, node, Control::Button).as_deref(),
            Some("Cancel now")
        );
    }

    #[test]
    fn a_field_is_sized_in_characters_and_follows_its_font() {
        // `size` and `cols` are counts of characters, so a field set in large
        // text has to grow with it rather than staying the size a table of
        // pixel defaults would have fixed it at.
        let (doc, node) = node_of(r#"<input size="10">"#, "input");
        let small = ComputedStyle {
            font_size: 10.0,
            line_height: css::style::LineHeight::Px(12.0),
            ..ComputedStyle::default()
        };
        let large = ComputedStyle {
            font_size: 20.0,
            line_height: css::style::LineHeight::Px(24.0),
            ..ComputedStyle::default()
        };
        let (narrow, short) = intrinsic_size(&doc, node, &small, Control::Text, None);
        let (wide, tall) = intrinsic_size(&doc, node, &large, Control::Text, None);
        assert!((wide - narrow * 2.0).abs() < 0.01, "{wide} vs {narrow}");
        assert!(tall > short);
    }

    #[test]
    fn a_textarea_is_sized_by_its_rows_and_columns() {
        let (doc, node) = node_of(r#"<textarea cols="30" rows="4"></textarea>"#, "textarea");
        let style = ComputedStyle::default();
        let (width, height) = intrinsic_size(&doc, node, &style, Control::TextArea, None);
        let (default_w, default_h) = {
            let (doc, node) = node_of("<textarea></textarea>", "textarea");
            intrinsic_size(&doc, node, &style, Control::TextArea, None)
        };
        assert!(width > default_w, "cols did not widen it");
        assert!(height > default_h, "rows did not deepen it");
    }

    #[test]
    fn a_select_is_measured_from_the_option_it_shows() {
        // Not the widest option and not all of them joined: a closed dropdown
        // is as wide as what it displays.
        let style = ComputedStyle::default();
        // Measured through the label, as the real caller does: what a closed
        // dropdown shows is what decides how wide it has to be, and passing
        // `None` here would have both cases agree and prove nothing.
        let width_of = |markup: &str| {
            let (doc, node) = node_of(markup, "select");
            let label = label_of(&doc, node, Control::Select);
            intrinsic_size(&doc, node, &style, Control::Select, label.as_deref()).0
        };
        let wide = width_of(
            "<select><option>short</option><option selected>a much longer one</option></select>",
        );
        let narrow = width_of(
            "<select><option selected>short</option><option>a much longer one</option></select>",
        );
        assert!(wide > narrow, "{wide} vs {narrow}");
    }

    /// The one control in a fixture.
    fn only_control(doc: &Document) -> (NodeId, Control) {
        doc.descendants(doc.root())
            .into_iter()
            .find_map(|node| control_of(doc, node).map(|control| (node, control)))
            .expect("a control")
    }

    #[test]
    fn what_has_been_typed_wins_over_what_the_markup_said() {
        // The two are different things in HTML — `value` is the field's default
        // and what a reader has typed is a property of the control — and this
        // is the seam where the difference shows up (#110).
        for markup in [
            "<input type=\"text\" value=\"Ada\">",
            "<input type=\"password\" value=\"Ada\">",
            "<textarea>Ada</textarea>",
        ] {
            let mut doc = dom::parse(&format!("<body>{markup}</body>"));
            let (node, control) = only_control(&doc);
            assert_eq!(
                label_of(&doc, node, control).as_deref(),
                Some(if control == Control::Password {
                    "•••"
                } else {
                    "Ada"
                }),
                "before anything is typed, in {markup}"
            );

            doc.set_value(node, "Byron");
            let shown = label_of(&doc, node, control).expect("a label");
            if control == Control::Password {
                // Bullets, not the value. A password field that renders its own
                // contents over the shoulder of whoever is reading the page is
                // the one way this could be worse than drawing nothing — and
                // typing into it must not be the thing that gives that up.
                assert_eq!(shown, "•••••", "in {markup}");
            } else {
                assert_eq!(shown, "Byron", "in {markup}");
            }
        }
    }

    #[test]
    fn a_textarea_emptied_by_the_reader_stays_empty() {
        // The arm that reads a `<textarea>`'s content used to sit above the one
        // that reads what was typed in it, and shadowed it completely: nothing
        // typed into a textarea ever appeared, and the failure looked exactly
        // like the keystrokes not arriving. Emptying one is the sharper half —
        // treating "" as "say nothing" would put the markup's own text back on
        // screen the moment the last character was deleted.
        let mut doc = dom::parse("<body><textarea>note</textarea></body>");
        let (node, control) = only_control(&doc);
        doc.set_value(node, "");
        assert_eq!(label_of(&doc, node, control).as_deref(), Some(""));
    }

    /// The one `<form>` in a fixture, and its submission with nothing pressed.
    fn sent(html: &str) -> Submission {
        let doc = dom::parse(html);
        let form = doc
            .descendants(doc.root())
            .into_iter()
            .find(|&node| {
                doc.element(node)
                    .is_some_and(|it| it.local_name() == "form")
            })
            .expect("a form");
        submission(&doc, form, None)
    }

    #[test]
    fn a_form_sends_its_named_controls_in_document_order() {
        let sent = sent(
            "<form action=\"/search\" method=\"get\">\
             <input name=\"q\" value=\"hello world\">\
             <input name=\"page\" value=\"2\">\
             </form>",
        );
        assert_eq!(sent.action, "/search");
        assert_eq!(sent.method, Method::Get);
        // A space is a plus and not `%20`, which is the one thing about this
        // encoding that surprises everybody who meets it.
        assert_eq!(sent.body, "q=hello+world&page=2");
    }

    #[test]
    fn a_control_with_no_name_sends_nothing() {
        // HTML 4 §17.13.2's first rule, and the one that matters most in
        // practice: the era's forms are full of unnamed decoration.
        let sent = sent("<form><input value=\"kept out\"><input name=\"in\" value=\"1\"></form>");
        assert_eq!(sent.body, "in=1");
    }

    #[test]
    fn a_disabled_control_sends_nothing() {
        let sent = sent(
            "<form><input name=\"off\" value=\"1\" disabled><input name=\"on\" value=\"2\"></form>",
        );
        assert_eq!(sent.body, "on=2");
    }

    #[test]
    fn a_box_sends_only_when_it_is_ticked() {
        let sent = sent(
            "<form>\
             <input type=\"checkbox\" name=\"a\" checked>\
             <input type=\"checkbox\" name=\"b\">\
             <input type=\"radio\" name=\"c\" value=\"yes\" checked>\
             <input type=\"radio\" name=\"c\" value=\"no\">\
             </form>",
        );
        // `on` is HTML's default for a ticked box that carries no value.
        assert_eq!(sent.body, "a=on&c=yes");
    }

    #[test]
    fn only_the_button_that_was_pressed_is_sent() {
        // The rule with the sharpest consequence: a form with `name="action"`
        // on two buttons means entirely different things depending on which was
        // pressed, so the submitter is asked for rather than guessed at.
        let doc = dom::parse(
            "<form>\
             <input name=\"q\" value=\"x\">\
             <button name=\"do\" value=\"save\">Save</button>\
             <button name=\"do\" value=\"delete\">Delete</button>\
             </form>",
        );
        let form = doc.find_element("form").expect("a form");
        let buttons: Vec<NodeId> = doc
            .descendants(form)
            .into_iter()
            .filter(|&node| {
                doc.element(node)
                    .is_some_and(|it| it.local_name() == "button")
            })
            .collect();

        assert_eq!(submission(&doc, form, Some(buttons[0])).body, "q=x&do=save");
        assert_eq!(
            submission(&doc, form, Some(buttons[1])).body,
            "q=x&do=delete"
        );
        // Enter in a text field presses nothing, so neither button is sent.
        assert_eq!(submission(&doc, form, None).body, "q=x");
    }

    #[test]
    fn a_reset_button_is_never_sent_even_when_it_is_the_one_pressed() {
        let doc = dom::parse(
            "<form><input name=\"q\" value=\"x\">\
             <input type=\"reset\" name=\"clear\" value=\"Clear\"></form>",
        );
        let form = doc.find_element("form").expect("a form");
        let reset = doc
            .descendants(form)
            .into_iter()
            .find(|&node| {
                doc.element(node)
                    .is_some_and(|it| it.attr("type") == Some("reset"))
            })
            .expect("a reset");
        assert_eq!(submission(&doc, form, Some(reset)).body, "q=x");
    }

    #[test]
    fn a_dropdown_sends_what_it_shows_and_a_list_sends_what_is_marked() {
        // A closed dropdown always sends something — its first option when
        // nothing is marked, which is what it is showing. A list box with
        // nothing marked sends nothing, because it is showing nothing chosen.
        let dropdown = sent(
            "<form><select name=\"s\"><option>one<option value=\"2\" selected>two</select></form>",
        );
        assert_eq!(dropdown.body, "s=2");

        let unmarked = sent("<form><select name=\"s\"><option>one<option>two</select></form>");
        assert_eq!(unmarked.body, "s=one", "a dropdown shows its first option");

        let list = sent(
            "<form><select name=\"s\" multiple>\
             <option selected>one<option>two<option selected>three</select></form>",
        );
        assert_eq!(list.body, "s=one&s=three");
    }

    #[test]
    fn what_was_typed_is_what_is_sent() {
        // The seam this whole feature turns on: the `value` attribute is the
        // field's default and what the reader typed is the control's contents,
        // and it is the contents that go to the server (#110).
        let mut doc = dom::parse("<form><input name=\"q\" value=\"Ada\"></form>");
        let form = doc.find_element("form").expect("a form");
        let field = doc
            .descendants(form)
            .into_iter()
            .find(|&node| control_of(&doc, node).is_some())
            .expect("a field");
        assert_eq!(submission(&doc, form, None).body, "q=Ada");
        doc.set_value(field, "Byron & co");
        assert_eq!(submission(&doc, form, None).body, "q=Byron+%26+co");
    }

    #[test]
    fn a_value_in_another_language_is_sent_as_its_bytes() {
        let mut doc = dom::parse("<form><input name=\"q\" value=\"\"></form>");
        let form = doc.find_element("form").expect("a form");
        let field = doc
            .descendants(form)
            .into_iter()
            .find(|&node| control_of(&doc, node).is_some())
            .expect("a field");
        doc.set_value(field, "日本");
        assert_eq!(submission(&doc, form, None).body, "q=%E6%97%A5%E6%9C%AC");
    }

    #[test]
    fn a_method_this_browser_has_never_heard_of_is_a_get() {
        // HTML's rule, and the safe direction: the default is the method that
        // cannot change anything on the far side.
        assert_eq!(sent("<form method=\"put\"></form>").method, Method::Get);
        assert_eq!(sent("<form method=\"POST\"></form>").method, Method::Post);
        assert_eq!(sent("<form></form>").method, Method::Get);
    }

    #[test]
    fn a_control_belongs_to_the_form_it_is_inside() {
        // Two forms on one page is the era's login-and-search layout, and
        // sending one form's contents to the other's destination would be a
        // page leaking its own fields.
        let doc = dom::parse(
            "<body>\
             <form action=\"/login\"><input name=\"user\" value=\"ada\"></form>\
             <form action=\"/search\"><input name=\"q\" value=\"tables\"></form>\
             </body>",
        );
        let forms: Vec<NodeId> = doc
            .descendants(doc.root())
            .into_iter()
            .filter(|&node| {
                doc.element(node)
                    .is_some_and(|it| it.local_name() == "form")
            })
            .collect();
        assert_eq!(submission(&doc, forms[0], None).body, "user=ada");
        assert_eq!(submission(&doc, forms[1], None).body, "q=tables");
    }

    #[test]
    fn a_field_finds_the_form_it_is_in() {
        let doc = dom::parse("<form action=\"/go\"><p><input name=\"q\"></p></form>");
        let field = doc
            .descendants(doc.root())
            .into_iter()
            .find(|&node| control_of(&doc, node).is_some())
            .expect("a field");
        let form = form_of(&doc, field).expect("a form");
        assert_eq!(
            doc.element(form).and_then(|it| it.attr("action")),
            Some("/go")
        );
        // And a field with no form around it belongs to none, rather than to
        // the first one on the page.
        let loose = dom::parse("<body><input name=\"q\"></body>");
        let node = loose
            .descendants(loose.root())
            .into_iter()
            .find(|&node| control_of(&loose, node).is_some())
            .expect("a field");
        assert_eq!(form_of(&loose, node), None);
    }

    /// Applies a press, the way the render path does, so a test can press
    /// twice and see the second press act on the result of the first.
    fn apply(doc: &mut Document, node: NodeId) -> bool {
        let changes = press(doc, node);
        let changed = !changes.is_empty();
        for (id, on) in changes {
            doc.set_chosen(id, on);
        }
        changed
    }

    fn controls(doc: &Document) -> Vec<NodeId> {
        doc.descendants(doc.root())
            .into_iter()
            .filter(|&id| control_of(doc, id).is_some())
            .collect()
    }

    #[test]
    fn a_checkbox_ticks_and_unticks() {
        let (mut doc, node) = node_of(r#"<input type="checkbox">"#, "input");
        assert!(!doc.is_on(node));
        assert!(apply(&mut doc, node));
        assert!(doc.is_on(node), "pressing an empty box ticks it");
        assert!(apply(&mut doc, node));
        assert!(!doc.is_on(node), "pressing it again unticks it");
    }

    #[test]
    fn a_box_the_markup_ticked_can_be_unticked() {
        // The case this exists for: a form with a pre-ticked "send me email"
        // box could until now only be sent with the box still ticked.
        let (mut doc, node) = node_of(r#"<input type="checkbox" checked>"#, "input");
        assert!(doc.is_on(node));
        apply(&mut doc, node);
        assert!(!doc.is_on(node));
    }

    #[test]
    fn unticking_a_box_takes_it_out_of_what_is_sent() {
        let mut doc = dom::parse(
            r#"<form><input type="checkbox" name="post" checked>
               <input type="text" name="q" value="x"></form>"#,
        );
        let form = doc.find_element("form").expect("the form");
        let box_ = controls(&doc)[0];
        assert_eq!(submission(&doc, form, None).body, "post=on&q=x");
        apply(&mut doc, box_);
        assert_eq!(
            submission(&doc, form, None).body,
            "q=x",
            "an unticked box is not a successful control",
        );
    }

    #[test]
    fn choosing_a_radio_clears_the_rest_of_its_group() {
        let mut doc = dom::parse(
            r#"<form><input type="radio" name="size" value="s" checked>
               <input type="radio" name="size" value="m">
               <input type="radio" name="size" value="l"></form>"#,
        );
        let form = doc.find_element("form").expect("the form");
        let radios = controls(&doc);
        apply(&mut doc, radios[2]);
        assert!(doc.is_on(radios[2]));
        assert!(!doc.is_on(radios[0]), "the markup's answer was cleared");
        assert!(!doc.is_on(radios[1]));
        assert_eq!(submission(&doc, form, None).body, "size=l");
    }

    #[test]
    fn a_radio_leaves_another_groups_answer_alone() {
        let mut doc = dom::parse(
            r#"<form><input type="radio" name="size" value="s" checked>
               <input type="radio" name="hot" value="y" checked></form>"#,
        );
        let form = doc.find_element("form").expect("the form");
        let radios = controls(&doc);
        apply(&mut doc, radios[0]);
        assert_eq!(submission(&doc, form, None).body, "size=s&hot=y");
    }

    #[test]
    fn pressing_a_chosen_radio_does_not_unchoose_it() {
        // HTML has no way to say "none of these" once one is chosen.
        let (mut doc, node) = node_of(r#"<input type="radio" name="a" checked>"#, "input");
        assert!(
            !apply(&mut doc, node),
            "nothing changed, so nothing repaints"
        );
        assert!(doc.is_on(node));
    }

    #[test]
    fn choosing_an_option_is_what_the_dropdown_shows_and_sends() {
        let mut doc = dom::parse(
            r#"<form><select name="where">
               <option value="uk">United Kingdom</option>
               <option value="fr" selected>France</option>
               </select></form>"#,
        );
        let form = doc.find_element("form").expect("the form");
        let select = doc.find_element("select").expect("the select");
        assert_eq!(submission(&doc, form, None).body, "where=fr");
        let uk = options_of(&doc, select)[0];
        apply(&mut doc, uk);
        assert_eq!(submission(&doc, form, None).body, "where=uk");
        assert_eq!(
            label_of(&doc, select, Control::Select).as_deref(),
            Some("United Kingdom"),
            "what it sends and what it shows have to be the same option",
        );
    }

    #[test]
    fn a_list_box_picks_a_set_rather_than_one() {
        let mut doc = dom::parse(
            r#"<form><select name="where" multiple>
               <option value="uk">UK</option>
               <option value="fr">France</option>
               </select></form>"#,
        );
        let form = doc.find_element("form").expect("the form");
        let select = doc.find_element("select").expect("the select");
        let options = options_of(&doc, select);
        apply(&mut doc, options[0]);
        apply(&mut doc, options[1]);
        assert_eq!(submission(&doc, form, None).body, "where=uk&where=fr");
        apply(&mut doc, options[0]);
        assert_eq!(submission(&doc, form, None).body, "where=fr");
    }

    #[test]
    fn pressing_a_field_or_a_button_changes_nothing() {
        // They are pressed for other reasons, and both already have one.
        for markup in [r#"<input type="text">"#, "<button>Go</button>"] {
            let doc = dom::parse(markup);
            let node = controls(&doc)[0];
            assert!(press(&doc, node).is_empty(), "{markup}");
        }
    }
}
