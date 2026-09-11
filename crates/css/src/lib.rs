//! CSS 2.1 parsing, selector matching, cascade, and computed style (ADR-0004).
//!
//! Tokenisation comes from `cssparser` because it is specified precisely enough
//! that implementations disagreeing was itself the bug (ADR-0007). Everything
//! above the tokens — selectors, the cascade, computed values — is ours.

pub mod cascade;
pub mod selector;
pub mod style;
pub mod ua;
pub mod value;

use cssparser::{
    AtRuleParser, DeclarationParser, ParseError, Parser, ParserInput, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser,
};

pub use selector::{Selector, Specificity};
pub use style::ComputedStyle;
pub use value::{Color, Length, Raw};

/// One `name: value` pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    /// Lowercased property name.
    pub name: String,
    /// The value's component values.
    pub value: Vec<Raw>,
    /// Whether `!important` was present.
    pub important: bool,
}

/// A selector list and the declarations it applies.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Selectors that trigger this rule.
    pub selectors: Vec<Selector>,
    /// Declarations in source order.
    pub declarations: Vec<Declaration>,
}

/// A parsed stylesheet.
#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    /// Rules in source order, which the cascade uses to break specificity ties.
    pub rules: Vec<Rule>,
    /// URLs named by `@import`, in source order.
    ///
    /// Fetching them is the caller's job: this crate has no network and should
    /// not acquire one. An imported sheet's rules come *before* the importing
    /// sheet's, which is what the caller has to preserve.
    pub imports: Vec<String>,
}

impl Stylesheet {
    /// Parses a stylesheet.
    ///
    /// Never fails. CSS error handling is defined as "discard what you cannot
    /// parse and carry on", which is exactly what a browser must do with two
    /// decades of accumulated authoring mistakes.
    pub fn parse(source: &str) -> Self {
        Self::parse_at(source, ASSUMED_VIEWPORT_WIDTH)
    }

    /// Parses a stylesheet for a viewport `width` pixels across.
    ///
    /// The width is needed at parse time and not later because `@media`
    /// decides which rules exist at all: a block whose query does not apply
    /// contributes nothing, so there is no rule left to ask about afterwards.
    pub fn parse_at(source: &str, width: f32) -> Self {
        let mut input = ParserInput::new(source);
        let mut parser = Parser::new(&mut input);
        let mut rule_parser = TopLevel {
            viewport_width: width,
            ..TopLevel::default()
        };
        // `flatten` discards the Err arm, which is the specified recovery: a
        // rule that fails to parse is dropped and the sheet continues.
        let rules: Vec<Rule> = StyleSheetParser::new(&mut parser, &mut rule_parser)
            .flatten()
            .flatten()
            .collect();
        Self {
            rules,
            imports: rule_parser.imports,
        }
    }
}

/// Parses the contents of a `style` attribute.
///
/// An inline `style` has no selector: it is a bare declaration block that
/// applies to one element, and it outranks every author rule regardless of
/// specificity. Extremely common in the era's markup, and without it a page's
/// colours and spacing simply do not appear.
pub fn parse_style_attribute(source: &str) -> Vec<Declaration> {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    let mut declaration_parser = DeclarationBlock;
    let body = RuleBodyParser::new(&mut parser, &mut declaration_parser);
    // `flatten` drops declarations that fail to parse, which is the specified
    // recovery — one bad property must not discard the rest of the attribute.
    body.flatten().collect()
}

/// The viewport width assumed where none is supplied.
///
/// Only reached by callers that parse a stylesheet without a page to render
/// it into — tests, mostly. A real render passes its own width.
pub const ASSUMED_VIEWPORT_WIDTH: f32 = 1000.0;

/// Whether a media query list applies, at an assumed viewport width.
pub fn media_applies(query: &str) -> bool {
    media_applies_at(query, ASSUMED_VIEWPORT_WIDTH)
}

/// Whether a media query list applies to a viewport `width` pixels across.
///
/// CSS 2.1 has media *types* and no features, and this used to answer `false`
/// to any query carrying one, on the grounds that a rule written for one
/// viewport size is worse applied unconditionally than not applied at all.
/// That reasoning was right about the danger and wrong about the remedy:
/// answering `false` to `(min-width: 640px)` is not declining to guess, it is
/// guessing *no* — and on a page whose desktop layout lives behind exactly
/// that query, it throws the desktop layout away.
///
/// Wikipedia is the case in point. Its infobox is floated right by a rule
/// inside `@media all and (min-width: 640px)`, and without it the article's
/// whole two-column shape collapses. The width is a number this browser
/// knows, so the query gets a real answer.
///
/// Width features only. `orientation`, `resolution`, `color` and the rest are
/// still answered `false`, because a wrong answer there is a guess rather than
/// a measurement — and `print` still does not apply, because this is a screen.
pub fn media_applies_at(query: &str, width: f32) -> bool {
    // An empty query list is `all`, which is why `@media { … }` works.
    if query.trim().is_empty() {
        return true;
    }
    query
        .split(',')
        .any(|entry| query_applies(entry.trim(), width))
}

/// Whether one comma-separated query in a list applies.
fn query_applies(query: &str, width: f32) -> bool {
    let query = query.trim().to_ascii_lowercase();
    let (negated, query) = match query.strip_prefix("not ") {
        Some(rest) => (true, rest.trim()),
        // `only` exists to hide a query from parsers too old to know the
        // syntax. This one is not, so it reads through it.
        None => (false, query.strip_prefix("only ").unwrap_or(&query).trim()),
    };
    let mut parts = query
        .split(" and ")
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let matched = parts.clone().next().is_some()
        && parts.all(|part| {
            if part.starts_with('(') {
                feature_applies(part.trim_matches(['(', ')']).trim(), width)
            } else {
                matches!(part, "all" | "screen")
            }
        });
    matched != negated
}

/// Whether one `(feature: value)` term holds.
fn feature_applies(term: &str, width: f32) -> bool {
    let Some((name, value)) = term.split_once(':') else {
        // A bare `(feature)` asks whether it exists and is non-zero. Only the
        // width ones can be answered, and a viewport always has a width.
        return matches!(term.trim(), "width" | "device-width");
    };
    let name = name.trim();
    let Some(pixels) = length_in_pixels(value.trim()) else {
        return false;
    };
    match name {
        "min-width" | "min-device-width" => width >= pixels,
        "max-width" | "max-device-width" => width <= pixels,
        "width" | "device-width" => (width - pixels).abs() < f32::EPSILON,
        // Answering anything else would be a guess rather than a measurement.
        _ => false,
    }
}

/// A media feature's length value, in pixels.
///
/// `em` is relative to the *initial* font size in a media query — the page's
/// own font size cannot apply, since the query decides which rules make it.
fn length_in_pixels(value: &str) -> Option<f32> {
    let value = value.trim();
    for (unit, scale) in [
        ("px", 1.0),
        ("em", style::DEFAULT_FONT_SIZE),
        ("rem", style::DEFAULT_FONT_SIZE),
    ] {
        if let Some(number) = value.strip_suffix(unit) {
            return number.trim().parse::<f32>().ok().map(|n| n * scale);
        }
    }
    // A bare `0` is legal and needs no unit.
    value.parse::<f32>().ok().filter(|n| *n == 0.0)
}

/// What an at-rule turned out to be.
enum AtRule {
    /// `@media`, with whether its query applies.
    Media(bool),
    /// `@import`, with the URL it names.
    Import(String),
    /// Something we do not implement. Skipping is the specified recovery.
    Unhandled,
}

/// Parses top-level rules, including the two at-rules that matter.
struct TopLevel {
    imports: Vec<String>,
    /// The viewport the sheet is being parsed for, which `@media` needs.
    viewport_width: f32,
}

impl Default for TopLevel {
    fn default() -> Self {
        Self {
            imports: Vec::new(),
            viewport_width: ASSUMED_VIEWPORT_WIDTH,
        }
    }
}

impl<'i> QualifiedRuleParser<'i> for TopLevel {
    type Prelude = Vec<Selector>;
    type QualifiedRule = Vec<Rule>;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, ()>> {
        // Selectors are re-parsed from source text rather than from tokens:
        // combinators and compounds are easier to read off the original string,
        // and the CSS 2.1 selector grammar is small enough not to need more.
        let start = input.position();
        while input.next().is_ok() {}
        let text = input.slice_from(start);
        Ok(selector::parse_selector_list(text))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Vec<Rule>, ParseError<'i, ()>> {
        if prelude.is_empty() {
            return Err(input.new_custom_error(()));
        }
        Ok(vec![Rule {
            selectors: prelude,
            declarations: read_declarations(input),
        }])
    }
}

impl<'i> AtRuleParser<'i> for TopLevel {
    type Prelude = AtRule;
    type AtRule = Vec<Rule>;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: cssparser::CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, ()>> {
        match name.as_ref().to_ascii_lowercase().as_str() {
            "media" => {
                let start = input.position();
                while input.next().is_ok() {}
                Ok(AtRule::Media(media_applies_at(
                    input.slice_from(start),
                    self.viewport_width,
                )))
            }
            "import" => {
                // `@import url(x.css)` and `@import "x.css"` are both ordinary,
                // and either may be followed by a media query list.
                let url = match input.next()?.clone() {
                    cssparser::Token::UnquotedUrl(url) => url.as_ref().to_owned(),
                    cssparser::Token::QuotedString(url) => url.as_ref().to_owned(),
                    cssparser::Token::Function(name) if name.as_ref() == "url" => input
                        .parse_nested_block(|inner| {
                            Ok::<_, ParseError<'i, ()>>(match inner.next() {
                                Ok(cssparser::Token::QuotedString(url)) => url.as_ref().to_owned(),
                                Ok(cssparser::Token::UnquotedUrl(url)) => url.as_ref().to_owned(),
                                _ => String::new(),
                            })
                        })?,
                    _ => return Ok(AtRule::Unhandled),
                };
                let start = input.position();
                while input.next().is_ok() {}
                if url.is_empty() || !media_applies_at(input.slice_from(start), self.viewport_width)
                {
                    return Ok(AtRule::Unhandled);
                }
                Ok(AtRule::Import(url))
            }
            _ => Ok(AtRule::Unhandled),
        }
    }

    /// `@import` has no block; this is where it is recorded.
    fn rule_without_block(
        &mut self,
        prelude: Self::Prelude,
        _: &ParserState,
    ) -> Result<Self::AtRule, ()> {
        match prelude {
            AtRule::Import(url) => {
                self.imports.push(url);
                Ok(Vec::new())
            }
            // An at-rule that needs a block and has none is malformed.
            _ => Err(()),
        }
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, ParseError<'i, ()>> {
        let AtRule::Media(applies) = prelude else {
            return Err(input.new_custom_error(()));
        };
        // The block is consumed either way — leaving it unparsed would make the
        // rest of the sheet look like garbage to the tokenizer.
        let mut nested = Nested;
        let rules: Vec<Rule> = RuleBodyParser::new(input, &mut nested).flatten().collect();
        Ok(if applies { rules } else { Vec::new() })
    }
}

/// Parses the qualified rules inside an `@media` block.
///
/// Nested at-rules and declarations are not CSS 2.1 inside `@media`, so this
/// only handles the one thing that belongs there.
struct Nested;

impl<'i> QualifiedRuleParser<'i> for Nested {
    type Prelude = Vec<Selector>;
    type QualifiedRule = Rule;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, ()>> {
        let start = input.position();
        while input.next().is_ok() {}
        Ok(selector::parse_selector_list(input.slice_from(start)))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Rule, ParseError<'i, ()>> {
        if prelude.is_empty() {
            return Err(input.new_custom_error(()));
        }
        Ok(Rule {
            selectors: prelude,
            declarations: read_declarations(input),
        })
    }
}

impl<'i> AtRuleParser<'i> for Nested {
    type Prelude = ();
    type AtRule = Rule;
    type Error = ();
}

/// Required by the body parser, and never reached: `parse_declarations` is
/// false, so a stray declaration inside `@media` is skipped rather than
/// offered here.
impl<'i> DeclarationParser<'i> for Nested {
    type Declaration = Rule;
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, Rule, ()> for Nested {
    fn parse_declarations(&self) -> bool {
        false
    }

    fn parse_qualified(&self) -> bool {
        true
    }
}

/// Reads a `{ … }` body as declarations.
fn read_declarations(input: &mut Parser<'_, '_>) -> Vec<Declaration> {
    let mut declaration_parser = DeclarationBlock;
    RuleBodyParser::new(input, &mut declaration_parser)
        .flatten()
        .collect()
}

/// Parses the declarations inside `{ … }`.
struct DeclarationBlock;

impl<'i> DeclarationParser<'i> for DeclarationBlock {
    type Declaration = Declaration;
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: cssparser::CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _: &ParserState,
    ) -> Result<Declaration, ParseError<'i, ()>> {
        let mut value = value::read_components(input);
        // `!important` arrives as a Delim('!') we do not model, followed by the
        // keyword; detect it by looking at the tail.
        let important = matches!(value.last(), Some(Raw::Ident(word)) if word == "important");
        if important {
            value.pop();
            if matches!(value.last(), Some(Raw::Other)) {
                value.pop();
            }
        }
        Ok(Declaration {
            name: name.as_ref().to_ascii_lowercase(),
            value,
            important,
        })
    }
}

impl<'i> AtRuleParser<'i> for DeclarationBlock {
    type Prelude = ();
    type AtRule = Declaration;
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for DeclarationBlock {
    type Prelude = ();
    type QualifiedRule = Declaration;
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, Declaration, ()> for DeclarationBlock {
    fn parse_declarations(&self) -> bool {
        true
    }

    // Nested rules are not CSS 2.1.
    fn parse_qualified(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rules_and_declarations() {
        let sheet = Stylesheet::parse("p { color: red; margin: 0 auto } .x, #y { display: none }");
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].declarations.len(), 2);
        assert_eq!(sheet.rules[0].declarations[0].name, "color");
        assert_eq!(sheet.rules[1].selectors.len(), 2);
    }

    #[test]
    fn recovers_from_broken_css() {
        // A malformed rule must not take the following rule down with it.
        let sheet =
            Stylesheet::parse("p { color: } @media print { a { b: c } } div { color: red }");
        let last = sheet.rules.last().expect("a rule survived");
        assert_eq!(last.declarations[0].name, "color");
    }

    #[test]
    fn detects_important() {
        let sheet = Stylesheet::parse("p { color: red !important; margin: 0 }");
        assert!(sheet.rules[0].declarations[0].important);
        assert!(!sheet.rules[0].declarations[1].important);
    }

    #[test]
    fn parses_colors() {
        let sheet = Stylesheet::parse(
            "a { color: #f00 } b { color: #00ff00 } c { color: rgb(0,0,255) } \
             d { color: rgba(0,0,0,0.5) } e { color: teal }",
        );
        let color =
            |i: usize| value::parse_color(&sheet.rules[i].declarations[0].value[0]).unwrap();
        assert_eq!(color(0), Color::rgb(255, 0, 0));
        assert_eq!(color(1), Color::rgb(0, 255, 0));
        assert_eq!(color(2), Color::rgb(0, 0, 255));
        assert_eq!(color(3).a, 128);
        assert_eq!(color(4), Color::rgb(0, 128, 128));
    }

    #[test]
    fn parses_lengths_including_absolute_units() {
        let sheet = Stylesheet::parse("a { width: 10px } b { width: 12pt } c { width: 0 }");
        let length = |i: usize| value::parse_length(&sheet.rules[i].declarations[0].value[0]);
        assert_eq!(length(0), Some(Length::Px(10.0)));
        assert_eq!(length(1), Some(Length::Px(16.0)), "12pt is 16px at 96dpi");
        assert_eq!(
            length(2),
            Some(Length::Px(0.0)),
            "unitless zero is a length"
        );
    }
}

#[cfg(test)]
mod at_rule_tests {
    use super::*;

    fn colors(sheet: &Stylesheet) -> Vec<&str> {
        sheet
            .rules
            .iter()
            .flat_map(|rule| &rule.declarations)
            .filter(|d| d.name == "color")
            .filter_map(|d| match d.value.first() {
                Some(Raw::Ident(name)) => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn media_screen_rules_are_applied() {
        // Skipping the whole block, which is what happened before, loses the
        // styling of any page that wrapped its rules in `@media screen`.
        let sheet = Stylesheet::parse(
            "p { color: red } @media screen { p { color: lime } } div { color: blue }",
        );
        assert_eq!(colors(&sheet), vec!["red", "lime", "blue"]);
    }

    #[test]
    fn media_all_and_an_empty_query_apply() {
        assert_eq!(
            colors(&Stylesheet::parse("@media all { p { color: lime } }")),
            vec!["lime"]
        );
        assert_eq!(
            colors(&Stylesheet::parse("@media { p { color: lime } }")),
            vec!["lime"]
        );
    }

    #[test]
    fn media_print_rules_are_not() {
        let sheet = Stylesheet::parse("@media print { p { color: lime } } p { color: red }");
        assert_eq!(colors(&sheet), vec!["red"]);
    }

    #[test]
    fn a_comma_list_applies_if_any_type_matches() {
        assert!(media_applies("print, screen"));
        assert!(media_applies("screen, projection"));
        assert!(!media_applies("print, tty"));
    }

    #[test]
    fn a_width_query_is_answered_against_the_viewport() {
        // This used to answer `false` to every feature query. Answering
        // `false` to `(min-width: 640px)` at 1000px is not declining to
        // guess — it is guessing wrong, and it threw away the desktop half
        // of every responsive stylesheet.
        assert!(media_applies_at("screen and (min-width: 640px)", 1000.0));
        assert!(!media_applies_at("screen and (min-width: 640px)", 320.0));
        assert!(media_applies_at("(max-width: 400px)", 320.0));
        assert!(!media_applies_at("(max-width: 400px)", 1000.0));
        assert!(media_applies_at("all and (min-width: 0)", 1000.0));
    }

    #[test]
    fn every_term_of_a_query_has_to_hold() {
        assert!(media_applies_at(
            "screen and (min-width: 500px) and (max-width: 1200px)",
            1000.0
        ));
        assert!(!media_applies_at(
            "screen and (min-width: 500px) and (max-width: 800px)",
            1000.0
        ));
        // The type still has to match, whatever the width says.
        assert!(!media_applies_at("print and (min-width: 100px)", 1000.0));
    }

    #[test]
    fn a_feature_that_cannot_be_measured_still_does_not_apply() {
        // A wrong answer here would be a guess rather than a measurement,
        // which is the distinction the width features cross and these do not.
        for query in [
            "screen and (orientation: landscape)",
            "screen and (min-resolution: 2dppx)",
            "screen and (color)",
            "screen and (min-width: 40banana)",
            // The one that matters most: a perfectly good length under a
            // name this browser cannot answer. The others fail while
            // parsing the value and never reach the question.
            "screen and (min-height: 100px)",
            "screen and (max-height: 100px)",
        ] {
            assert!(!media_applies_at(query, 1000.0), "{query}");
        }
    }

    #[test]
    fn not_and_only_are_read() {
        assert!(!media_applies_at("not screen", 1000.0));
        assert!(media_applies_at("not print", 1000.0));
        assert!(!media_applies_at(
            "not screen and (min-width: 640px)",
            1000.0
        ));
        // `only` exists to hide a query from parsers too old to know the
        // syntax; this one is not one of those.
        assert!(media_applies_at(
            "only screen and (min-width: 640px)",
            1000.0
        ));
    }

    #[test]
    fn em_in_a_query_is_the_initial_font_size() {
        // The page's own font size cannot apply: the query decides which
        // rules exist, so it is answered before any of them are.
        assert!(media_applies_at("(min-width: 40em)", 1000.0));
        assert!(!media_applies_at("(min-width: 40em)", 320.0));
    }

    #[test]
    fn a_media_block_contributes_its_rules_when_the_width_matches() {
        let sheet = Stylesheet::parse_at(
            "@media screen and (min-width: 9px) { p { color: lime } }",
            100.0,
        );
        assert_eq!(colors(&sheet), vec!["lime"]);
        let narrow = Stylesheet::parse_at(
            "@media screen and (min-width: 900px) { p { color: lime } }",
            100.0,
        );
        assert!(colors(&narrow).is_empty());
    }

    #[test]
    fn a_skipped_media_block_does_not_derail_the_rest_of_the_sheet() {
        // The block still has to be consumed, or the tokenizer treats the
        // remainder of the sheet as garbage.
        let sheet = Stylesheet::parse(
            "@media print { p { color: lime } .x { color: teal } } div { color: red }",
        );
        assert_eq!(colors(&sheet), vec!["red"]);
    }

    #[test]
    fn imports_are_recorded_in_both_forms() {
        for source in [
            "@import url(site.css);",
            "@import url(\"site.css\");",
            "@import 'site.css';",
            "@import \"site.css\" screen;",
        ] {
            let sheet = Stylesheet::parse(source);
            assert_eq!(sheet.imports, vec!["site.css".to_owned()], "for {source}");
        }
    }

    #[test]
    fn an_import_for_another_medium_is_not_recorded() {
        // No point fetching a print stylesheet to then not apply it.
        assert!(
            Stylesheet::parse("@import url(print.css) print;")
                .imports
                .is_empty()
        );
    }

    #[test]
    fn an_import_does_not_swallow_the_rules_after_it() {
        let sheet = Stylesheet::parse("@import url(a.css); p { color: red }");
        assert_eq!(sheet.imports, vec!["a.css".to_owned()]);
        assert_eq!(colors(&sheet), vec!["red"]);
    }

    #[test]
    fn an_unknown_at_rule_is_still_skipped() {
        let sheet = Stylesheet::parse("@font-face { src: url(x.ttf) } p { color: red }");
        assert_eq!(colors(&sheet), vec!["red"]);
        assert!(sheet.imports.is_empty());
    }
}
