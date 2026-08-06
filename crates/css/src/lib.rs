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
        let mut input = ParserInput::new(source);
        let mut parser = Parser::new(&mut input);
        let mut rule_parser = TopLevel::default();
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

/// Whether a media query list applies to this browser.
///
/// CSS 2.1 has media *types*, not media features: `screen`, `print`, `all`,
/// and a handful of others. A query carrying features — `screen and
/// (min-width: 600px)` — is CSS 3, and is treated as not applying rather than
/// as applying: a rule written for one viewport size, applied unconditionally,
/// misrenders the page more badly than not applying it at all. Pages that
/// depend on such rules are re-rendered as documents anyway (ADR-0009).
pub fn media_applies(query: &str) -> bool {
    // An empty query list is `all`, which is why `@media { … }` works.
    if query.trim().is_empty() {
        return true;
    }
    query.split(',').any(|entry| {
        let entry = entry.trim().to_ascii_lowercase();
        matches!(entry.as_str(), "all" | "screen")
    })
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
#[derive(Default)]
struct TopLevel {
    imports: Vec<String>,
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
                Ok(AtRule::Media(media_applies(input.slice_from(start))))
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
                if url.is_empty() || !media_applies(input.slice_from(start)) {
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
    fn a_feature_query_does_not_apply() {
        // CSS 3, and applying a rule written for one viewport size to every
        // size misrenders the page worse than dropping it.
        assert!(!media_applies("screen and (min-width: 600px)"));
        assert!(!media_applies("(max-width: 400px)"));
        let sheet = Stylesheet::parse("@media screen and (min-width: 9px) { p { color: lime } }");
        assert!(colors(&sheet).is_empty());
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
