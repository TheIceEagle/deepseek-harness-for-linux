//! Native text rendering only: Markdown never becomes HTML or Pango markup.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
    heading: Option<u8>,
}

#[derive(Debug, Eq, PartialEq)]
struct Span {
    text: String,
    style: Style,
}

struct LinkLabel {
    url: String,
    label: String,
}

#[derive(Default)]
struct Output {
    spans: Vec<Span>,
    links: Vec<LinkLabel>,
}

impl Output {
    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        for link in &mut self.links {
            link.label.push_str(text);
        }
        if let Some(last) = self.spans.last_mut().filter(|last| last.style == style) {
            last.text.push_str(text);
        } else {
            self.spans.push(Span {
                text: text.to_owned(),
                style,
            });
        }
    }

    fn newline(&mut self, count: usize) {
        if self.spans.is_empty() {
            return;
        }
        let trailing = self
            .spans
            .iter()
            .rev()
            .flat_map(|span| span.text.chars().rev())
            .take_while(|character| *character == '\n')
            .count();
        if trailing < count {
            self.push(&"\n".repeat(count - trailing), Style::default());
        }
    }

    fn finish(mut self) -> Vec<Span> {
        // Paragraph spacing is presentation. A code block's own terminal
        // newline, indentation and other whitespace are retained unchanged.
        while let Some(last) = self.spans.last_mut().filter(|span| !span.style.code) {
            let length = last.text.trim_end_matches('\n').len();
            last.text.truncate(length);
            if !last.text.is_empty() {
                break;
            }
            self.spans.pop();
        }
        self.spans
    }
}

fn parse(text: &str) -> Vec<Span> {
    let mut output = Output::default();
    let mut style = Style::default();
    let mut bold_depth = 0usize;
    let mut italic_depth = 0usize;
    let mut lists: Vec<Option<u64>> = Vec::new();
    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                output.newline(2);
                style.heading = Some(level as u8);
            }
            Event::End(TagEnd::Heading(_)) => {
                style.heading = None;
                output.newline(2);
            }
            Event::Start(Tag::Strong) => {
                bold_depth += 1;
                style.bold = true;
            }
            Event::End(TagEnd::Strong) => {
                bold_depth = bold_depth.saturating_sub(1);
                style.bold = bold_depth > 0;
            }
            Event::Start(Tag::Emphasis) => {
                italic_depth += 1;
                style.italic = true;
            }
            Event::End(TagEnd::Emphasis) => {
                italic_depth = italic_depth.saturating_sub(1);
                style.italic = italic_depth > 0;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                output.newline(2);
                style.code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                style.code = false;
                output.newline(2);
            }
            Event::Start(Tag::List(start)) => {
                output.newline(if lists.is_empty() { 2 } else { 1 });
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                output.newline(if lists.is_empty() { 2 } else { 1 });
            }
            Event::Start(Tag::Item) => {
                output.newline(1);
                output.push(&"  ".repeat(lists.len().saturating_sub(1)), style);
                let marker = match lists.last_mut() {
                    Some(Some(next)) => {
                        let marker = format!("{next}. ");
                        *next = next.saturating_add(1);
                        marker
                    }
                    _ => "• ".into(),
                };
                output.push(&marker, style);
            }
            Event::End(TagEnd::Item) => output.newline(1),
            Event::End(TagEnd::Paragraph) => {
                output.newline(if lists.is_empty() { 2 } else { 1 });
            }
            Event::End(TagEnd::HtmlBlock) => output.newline(2),
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                output.links.push(LinkLabel {
                    url: dest_url.to_string(),
                    label: String::new(),
                });
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                if let Some(link) = output.links.pop() {
                    if !link.url.is_empty() && link.url != link.label {
                        output.push(&format!(" ({})", link.url), style);
                    }
                }
            }
            Event::Text(value) | Event::Html(value) | Event::InlineHtml(value) => {
                output.push(&value, style);
            }
            Event::Code(value) => output.push(
                &value,
                Style {
                    code: true,
                    ..style
                },
            ),
            Event::SoftBreak => output.push(" ", style),
            Event::HardBreak => output.push("\n", style),
            Event::Rule => {
                output.newline(2);
                output.push("—", Style::default());
                output.newline(2);
            }
            Event::InlineMath(value) | Event::DisplayMath(value) => output.push(&value, style),
            Event::FootnoteReference(value) => output.push(&format!("[{value}]"), style),
            Event::TaskListMarker(checked) => {
                output.push(if checked { "[x] " } else { "[ ] " }, style);
            }
            Event::Start(_) | Event::End(_) => {}
        }
    }
    output.finish()
}

fn insert_attribute(
    attributes: &gtk::pango::AttrList,
    attribute: impl Into<gtk::pango::Attribute>,
    start: u32,
    end: u32,
) {
    let mut attribute = attribute.into();
    attribute.set_start_index(start);
    attribute.set_end_index(end);
    attributes.insert(attribute);
}

fn label_content(text: &str) -> (String, gtk::pango::AttrList) {
    let mut plain = String::new();
    let attributes = gtk::pango::AttrList::new();
    for span in parse(text) {
        let start = plain.len();
        plain.push_str(&span.text);
        // Pango indexes UTF-8 bytes, not Unicode characters. Every span starts
        // and ends at a Rust string boundary, including emoji and CJK text.
        let (Ok(start), Ok(end)) = (u32::try_from(start), u32::try_from(plain.len())) else {
            continue;
        };
        if span.style.bold || span.style.heading.is_some() {
            insert_attribute(
                &attributes,
                gtk::pango::AttrInt::new_weight(gtk::pango::Weight::Bold),
                start,
                end,
            );
        }
        if span.style.italic {
            insert_attribute(
                &attributes,
                gtk::pango::AttrInt::new_style(gtk::pango::Style::Italic),
                start,
                end,
            );
        }
        if span.style.code {
            insert_attribute(
                &attributes,
                gtk::pango::AttrString::new_family("monospace"),
                start,
                end,
            );
        }
        if let Some(level) = span.style.heading {
            let scale = [1.4, 1.25, 1.15, 1.1, 1.05, 1.0][usize::from(level - 1)];
            insert_attribute(
                &attributes,
                gtk::pango::AttrFloat::new_scale(scale),
                start,
                end,
            );
        }
    }
    (plain, attributes)
}

/// Replaces display text with native font attributes. The caller retains the
/// original message separately; untrusted text never becomes Pango markup.
pub fn render_label(label: &gtk::Label, text: &str) {
    let (plain, attributes) = label_content(text);
    label.set_use_markup(false);
    label.set_use_underline(false);
    label.set_text(&plain);
    label.set_attributes(Some(&attributes));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|span| span.text.as_str()).collect()
    }

    #[test]
    fn headings_and_nested_emphasis_keep_semantic_native_styles() {
        let spans = parse("# Heading\n\nSome **bold and *italic*** with `code`.\n");
        assert_eq!(plain(&spans), "Heading\n\nSome bold and italic with code.");
        assert!(spans
            .iter()
            .any(|span| span.text == "Heading" && span.style.heading == Some(1)));
        assert!(spans
            .iter()
            .any(|span| span.text == "italic" && span.style.bold && span.style.italic));
        assert!(spans
            .iter()
            .any(|span| span.text == "code" && span.style.code));
    }

    #[test]
    fn ordered_and_nested_lists_remain_readable_and_copyable() {
        let spans = parse("3. First\n4. Second\n   - Nested\n\nAfter the list.");
        assert_eq!(
            plain(&spans),
            "3. First\n4. Second\n  • Nested\n\nAfter the list."
        );
    }

    #[test]
    fn fenced_code_keeps_indentation_blank_lines_and_literal_markup() {
        let source = "```rust\nfn main() {\n\tprintln!(\"<b>hello</b>\");\n\n}\n```";
        let spans = parse(source);
        assert_eq!(
            plain(&spans),
            "fn main() {\n\tprintln!(\"<b>hello</b>\");\n\n}\n"
        );
        assert!(spans.iter().all(|span| span.style.code));
    }

    #[test]
    fn html_and_pango_like_input_are_preserved_as_literal_text() {
        let source = "<script>alert('x')</script>\n\n<span foreground=\"red\">literal</span>";
        assert_eq!(plain(&parse(source)), source);
        assert_eq!(
            plain(&parse("`<b>code</b>` &lt;img src=x&gt;")),
            "<b>code</b> <img src=x>"
        );
    }

    #[test]
    fn links_include_urls_without_making_them_executable() {
        assert_eq!(
            plain(&parse(
                "[Docs](https://example.test/docs) and <https://example.test/>"
            )),
            "Docs (https://example.test/docs) and https://example.test/"
        );
        assert_eq!(
            plain(&parse("[Run](javascript:alert%281%29)")),
            "Run (javascript:alert%281%29)"
        );
    }

    #[test]
    fn paragraphs_unicode_and_hard_breaks_survive_rendering() {
        assert_eq!(
            plain(&parse("Café 世界 🌊\nsoft line\n\nNext  \nline")),
            "Café 世界 🌊 soft line\n\nNext\nline"
        );
        assert!(parse("").is_empty());
    }

    #[test]
    fn font_attributes_target_complete_utf8_spans_after_markdown_is_removed() {
        let (plain, attributes) = label_content("🌊 before **café 世界** and `λ`.");
        assert_eq!(plain, "🌊 before café 世界 and λ.");
        let attributes = attributes.attributes();
        assert_eq!(attributes.len(), 2);
        for attribute in attributes {
            let start = attribute.start_index() as usize;
            let end = attribute.end_index() as usize;
            assert!(plain.is_char_boundary(start) && plain.is_char_boundary(end));
            assert!(start < end && end <= plain.len());
            match attribute.type_() {
                gtk::pango::AttrType::Weight => assert_eq!(&plain[start..end], "café 世界"),
                gtk::pango::AttrType::Family => assert_eq!(&plain[start..end], "λ"),
                other => panic!("Unexpected font attribute: {other:?}"),
            }
        }
    }
}
