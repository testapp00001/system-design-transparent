use std::collections::HashMap;

use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd, html};

use crate::util::escape_html;

pub struct Rendered {
    pub html: String,
    pub toc_html: String,
    /// The visible text with Markdown syntax removed, for search indexing.
    pub text: String,
    pub word_count: usize,
}

struct TocEntry {
    level: HeadingLevel,
    id: String,
    text: String,
}

/// Renders Markdown to HTML, giving every heading a stable `id` (so sections
/// can be linked to) and building a table of contents from h2/h3 headings.
pub fn render(markdown: &str) -> Rendered {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_SMART_PUNCTUATION
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_GFM;

    let events: Vec<Event> = Parser::new_ext(markdown, options).collect();
    let mut out: Vec<Event> = Vec::with_capacity(events.len() + 16);
    let mut toc = Vec::new();
    let mut used_ids: HashMap<String, usize> = HashMap::new();
    let mut text = String::with_capacity(markdown.len());

    for (i, event) in events.iter().enumerate() {
        match event {
            Event::Text(t) | Event::Code(t) => text.push_str(t),
            // Block boundaries become spaces; inline ones (bold, links) must not
            // split words.
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::CodeBlock
                | TagEnd::TableCell
                | TagEnd::BlockQuote(_)
                | TagEnd::FootnoteDefinition,
            )
            | Event::SoftBreak
            | Event::HardBreak => text.push(' '),
            _ => {}
        }
        match event {
            Event::Start(Tag::Heading { level, id, classes, attrs }) => {
                let text = heading_text(&events[i + 1..]);
                let base = id.as_ref().map(|s| s.to_string()).unwrap_or_else(|| slugify(&text));
                let unique = unique_id(base, &mut used_ids);
                if matches!(level, HeadingLevel::H2 | HeadingLevel::H3) {
                    toc.push(TocEntry { level: *level, id: unique.clone(), text });
                }
                let anchor = format!(r##"<a class="anchor" href="#{unique}" aria-hidden="true">#</a>"##);
                out.push(Event::Start(Tag::Heading {
                    level: *level,
                    id: Some(CowStr::from(unique)),
                    classes: classes.clone(),
                    attrs: attrs.clone(),
                }));
                out.push(Event::InlineHtml(CowStr::from(anchor)));
            }
            other => out.push(other.clone()),
        }
    }

    let mut body = String::with_capacity(markdown.len() * 3 / 2);
    html::push_html(&mut body, out.into_iter());
    // Wide tables scroll horizontally on phones instead of breaking the layout.
    let body = body.replace("<table>", r#"<div class="table-wrap"><table>"#).replace("</table>", "</table></div>");

    Rendered {
        html: body,
        toc_html: render_toc(&toc),
        word_count: text.split_whitespace().count(),
        text: text.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

fn heading_text(events: &[Event]) -> String {
    let mut text = String::new();
    for e in events {
        match e {
            Event::End(TagEnd::Heading(_)) => break,
            Event::Text(t) | Event::Code(t) => text.push_str(t),
            _ => {}
        }
    }
    text
}

fn unique_id(base: String, used: &mut HashMap<String, usize>) -> String {
    let base = if base.is_empty() { "section".to_string() } else { base };
    let n = used.entry(base.clone()).or_insert(0);
    *n += 1;
    if *n == 1 { base } else { format!("{base}-{n}") }
}

/// "How B-Trees work (and why)" -> "how-b-trees-work-and-why"
pub fn slugify(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    let mut dash = false;
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
            dash = false;
        } else if !dash && !slug.is_empty() {
            slug.push('-');
            dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn render_toc(entries: &[TocEntry]) -> String {
    if entries.len() < 2 {
        return String::new();
    }
    let mut s = String::from("<ol>");
    let mut in_sub = false;
    for (i, e) in entries.iter().enumerate() {
        let link = format!(r##"<a href="#{}">{}</a>"##, e.id, escape_html(&e.text));
        match e.level {
            HeadingLevel::H3 if i > 0 => {
                if !in_sub {
                    s.push_str("<ol>");
                    in_sub = true;
                }
                s.push_str(&format!("<li>{link}</li>"));
            }
            _ => {
                if in_sub {
                    s.push_str("</ol>");
                    in_sub = false;
                }
                if i > 0 {
                    s.push_str("</li>");
                }
                s.push_str(&format!("<li>{link}"));
            }
        }
    }
    if in_sub {
        s.push_str("</ol>");
    }
    s.push_str("</li></ol>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("How B-Trees work (and why)"), "how-b-trees-work-and-why");
        assert_eq!(slugify("  TCP vs. UDP!  "), "tcp-vs-udp");
        assert_eq!(slugify("???"), "");
    }

    #[test]
    fn headings_get_unique_ids_and_toc() {
        let r = render("## Intro\ntext\n### Details\n## Intro\n");
        assert!(r.html.contains(r#"<h2 id="intro">"#), "{}", r.html);
        assert!(r.html.contains(r#"<h2 id="intro-2">"#), "{}", r.html);
        assert!(r.html.contains(r#"<h3 id="details">"#), "{}", r.html);
        assert!(r.toc_html.contains(r##"href="#intro-2""##));
        assert!(r.toc_html.starts_with("<ol>"));
    }

    #[test]
    fn toc_escapes_heading_text() {
        let r = render("## A <b> tag\n## `Vec<T>`\n");
        assert!(r.toc_html.contains("Vec&lt;T&gt;"), "{}", r.toc_html);
    }

    #[test]
    fn plain_text_strips_markup() {
        let r = render("# Title\n\nSome **bold** and `code` with [a link](https://x.y).\n");
        assert_eq!(r.text, "Title Some bold and code with a link.");
    }

    #[test]
    fn tables_are_wrapped() {
        let r = render("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert!(r.html.contains(r#"<div class="table-wrap"><table>"#));
    }

    #[test]
    fn toc_html_is_balanced() {
        let r = render("## A\n### A1\n### A2\n## B\n## C\n### C1\n");
        assert_eq!(r.toc_html.matches("<ol>").count(), r.toc_html.matches("</ol>").count());
        assert_eq!(r.toc_html.matches("<li>").count(), r.toc_html.matches("</li>").count());
    }
}
