//! A bounded XML tree for manifests and subtitles. DTD declarations are refused outright
//! (so no entity expansion), nesting depth and element count are capped, and names are
//! reduced to their local part so namespace prefixes don't matter.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::error::ParseError;
use crate::limits::ParseLimits;

#[derive(Debug, Default, Clone)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub children: Vec<Node>,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn text_trimmed(&self) -> &str {
        self.text.trim()
    }
}

/// Parses a document and returns its root element.
pub fn parse(bytes: &[u8], lim: &ParseLimits) -> Result<Node, ParseError> {
    let mut reader = Reader::from_reader(bytes);
    let mut buf = Vec::new();
    // Element 0 is a synthetic document node; the real root is its only child.
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut elements = 0usize;
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| ParseError::invalid(format!("the XML is broken ({e})")))?;
        match event {
            Event::Start(e) => {
                open(&mut stack, &e, lim, &mut elements)?;
            }
            Event::Empty(e) => {
                open(&mut stack, &e, lim, &mut elements)?;
                close(&mut stack)?;
            }
            Event::End(_) => close(&mut stack)?,
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    push_text(&mut top.text, &t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                // Entity references come as their own events. No DTD can define others.
                let ch = if r.is_char_ref() {
                    r.resolve_char_ref().map_err(|e| {
                        ParseError::invalid(format!("a character reference is broken ({e})"))
                    })?
                } else {
                    match &*r {
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "amp" => Some('&'),
                        "apos" => Some('\''),
                        "quot" => Some('"'),
                        _ => {
                            return Err(ParseError::invalid(
                                "the XML uses an entity that isn't defined",
                            ));
                        }
                    }
                };
                if let (Some(c), Some(top)) = (ch, stack.last_mut()) {
                    let mut buf = [0u8; 4];
                    push_text(&mut top.text, c.encode_utf8(&mut buf));
                }
            }
            Event::CData(t) => {
                let s = t.into_inner().into_owned();
                if let Some(top) = stack.last_mut() {
                    push_text(&mut top.text, &s);
                }
            }
            Event::DocType(_) => return Err(ParseError::Doctype),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if stack.len() != 1 {
        return Err(ParseError::invalid("an element isn't closed"));
    }
    let mut doc = stack.pop().unwrap_or_default();
    if doc.children.is_empty() {
        return Err(ParseError::invalid("the document is empty"));
    }
    Ok(doc.children.swap_remove(0))
}

fn open(
    stack: &mut Vec<Node>,
    e: &BytesStart<'_>,
    lim: &ParseLimits,
    elements: &mut usize,
) -> Result<(), ParseError> {
    *elements += 1;
    if *elements > lim.max_xml_elements {
        return Err(ParseError::TooMany {
            what: "XML elements",
            limit: lim.max_xml_elements,
        });
    }
    if stack.len() > lim.max_xml_depth {
        return Err(ParseError::TooMany {
            what: "levels of XML nesting",
            limit: lim.max_xml_depth,
        });
    }
    let name = e.local_name().into_inner().to_string();
    let mut attrs = Vec::new();
    for a in e.attributes() {
        let a =
            a.map_err(|err| ParseError::invalid(format!("an XML attribute is broken ({err})")))?;
        let key = a.key.local_name().into_inner().to_string();
        let value = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| ParseError::invalid(format!("an XML attribute is broken ({err})")))?
            .into_owned();
        attrs.push((key, value));
    }
    stack.push(Node {
        name,
        attrs,
        text: String::new(),
        children: Vec::new(),
    });
    Ok(())
}

fn close(stack: &mut Vec<Node>) -> Result<(), ParseError> {
    if stack.len() < 2 {
        return Err(ParseError::invalid("an XML element is closed twice"));
    }
    let node = stack.pop().unwrap_or_default();
    if let Some(parent) = stack.last_mut() {
        // The text of a child lands in the parent's text where the child sat, so mixed
        // content (`a <b>b</b> c`) reads in document order.
        push_text(&mut parent.text, &node.text);
        if node.name == "br" {
            push_text(&mut parent.text, "\n");
        }
        parent.children.push(node);
    }
    Ok(())
}

/// Longest text kept per element. Nested text is copied into each ancestor, so this keeps a
/// hostile document from multiplying its size by its depth.
const MAX_TEXT: usize = 64 * 1024;

fn push_text(text: &mut String, more: &str) {
    let room = MAX_TEXT.saturating_sub(text.len());
    if room == 0 {
        return;
    }
    let mut take = room.min(more.len());
    while !more.is_char_boundary(take) {
        take -= 1;
    }
    text.push_str(&more[..take]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_with_prefixes_and_entities() {
        let doc = br#"<?xml version="1.0"?>
            <tt:tt xmlns:tt="x"><tt:body><tt:p begin="1s">a &amp; b<br/>c</tt:p></tt:body></tt:tt>"#;
        let root = parse(doc, &ParseLimits::default()).expect("parses");
        assert_eq!(root.name, "tt");
        let p = root.child("body").and_then(|b| b.child("p")).expect("p");
        assert_eq!(p.attr("begin"), Some("1s"));
        assert_eq!(p.text, "a & b\nc");
        assert!(p.child("br").is_some());
    }

    #[test]
    fn mixed_content_keeps_order() {
        let root =
            parse(b"<p>a <span>b</span> c<br/>d</p>", &ParseLimits::default()).expect("parses");
        assert_eq!(root.text, "a b c\nd");
    }

    #[test]
    fn doctype_is_refused() {
        let doc = b"<!DOCTYPE x [<!ENTITY a \"b\">]><x>&a;</x>";
        assert_eq!(
            parse(doc, &ParseLimits::default()).err(),
            Some(ParseError::Doctype)
        );
    }

    #[test]
    fn depth_and_elements_are_capped() {
        let deep = format!("{}{}", "<a>".repeat(70), "</a>".repeat(70));
        let lim = ParseLimits::default();
        assert!(matches!(
            parse(deep.as_bytes(), &lim),
            Err(ParseError::TooMany { .. })
        ));
        let wide = format!("<r>{}</r>", "<i/>".repeat(50));
        let small = ParseLimits {
            max_xml_elements: 10,
            ..lim
        };
        assert!(matches!(
            parse(wide.as_bytes(), &small),
            Err(ParseError::TooMany { .. })
        ));
    }

    #[test]
    fn broken_xml_is_an_error_not_a_panic() {
        for bad in ["", "<a>", "<a></b>", "<a attr=></a>", "text only"] {
            assert!(
                parse(bad.as_bytes(), &ParseLimits::default()).is_err(),
                "{bad}"
            );
        }
    }
}
