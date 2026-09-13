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
//! # What is not here
//!
//! Nothing can be typed into, clicked, or submitted: this is how a form
//! *looks*, not a form that works. That is a deliberate stopping point rather
//! than an oversight — a control that draws correctly makes the page read
//! correctly, and interaction is a separate piece of work with a separate risk.
//!
//! Radio buttons are drawn square, because the rasteriser has rectangles and no
//! rounded primitive. A round one wants either a circle in the display list or
//! a bitmap, and both are more than this change is for.

use css::style::ComputedStyle;
use dom::{Document, NodeId};

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
    /// A radio button, drawn square — see the module note.
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
        Control::TextArea | Control::Button if element.local_name() != "input" => {
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
            let len = element.attr("value").map(|v| v.chars().count())?;
            (len > 0).then(|| "\u{2022}".repeat(len))
        }
        Control::Text => element
            .attr("value")
            .map(str::to_owned)
            .filter(|value| !value.is_empty()),
        Control::TextArea => {
            let text = descendant_text(doc, node);
            (!text.trim().is_empty()).then_some(text)
        }
    }
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
    let line = style.line_height.max(font_size);
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

/// Every `option` under a `select`, in document order.
fn options_of(doc: &Document, node: NodeId) -> Vec<NodeId> {
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
        .find(|&&id| {
            doc.element(id)
                .is_some_and(|element| element.attr("selected").is_some())
        })
        .or(options.first())
        .copied()
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
            line_height: 12.0,
            ..ComputedStyle::default()
        };
        let large = ComputedStyle {
            font_size: 20.0,
            line_height: 24.0,
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
}
