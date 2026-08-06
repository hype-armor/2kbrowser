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
        let mut rule_parser = TopLevel;
        // `flatten` discards the Err arm, which is the specified recovery: a
        // rule that fails to parse is dropped and the sheet continues.
        let rules = StyleSheetParser::new(&mut parser, &mut rule_parser)
            .flatten()
            .collect();
        Self { rules }
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

/// Parses top-level qualified rules. At-rules are skipped: `@media` and
/// `@import` are M2 work, and skipping is the specified recovery.
struct TopLevel;

impl<'i> QualifiedRuleParser<'i> for TopLevel {
    type Prelude = Vec<Selector>;
    type QualifiedRule = Rule;
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
    ) -> Result<Rule, ParseError<'i, ()>> {
        if prelude.is_empty() {
            return Err(input.new_custom_error(()));
        }
        let mut declarations = Vec::new();
        let mut declaration_parser = DeclarationBlock;
        let mut body = RuleBodyParser::new(input, &mut declaration_parser);
        while let Some(Ok(declaration)) = body.next() {
            declarations.push(declaration);
        }
        Ok(Rule {
            selectors: prelude,
            declarations,
        })
    }
}

impl<'i> AtRuleParser<'i> for TopLevel {
    type Prelude = ();
    type AtRule = Rule;
    type Error = ();
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
