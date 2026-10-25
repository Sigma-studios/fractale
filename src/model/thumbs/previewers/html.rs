//! HTML pages: their title and text on a page (see `page`). Nothing is
//! fetched or run: rendering a page for real would need a browser engine,
//! and loading a downloaded page's remote resources just to draw a tile
//! would tell its author you looked.

use super::super::{FileInfo, Previewer, Rendered};

const MAX_READ: u64 = 2 << 20;

pub struct Html;

impl Previewer for Html {
    fn name(&self) -> &'static str {
        "html"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        f.ext == "html" || f.ext == "htm" || f.ext == "xhtml"
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(&f.path).map_err(|e| e.to_string())?.take(MAX_READ).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        let (title, text) = html_text(&String::from_utf8_lossy(&bytes), 3000);
        if title.is_none() && text.trim().is_empty() {
            return Err("no text".into());
        }
        super::page::render(title.as_deref(), &text, max).map(Rendered::Still)
    }
}

/// The page's title and visible text: no scripts, styles or comments;
/// block elements start new lines.
pub fn html_text(html: &str, limit: usize) -> (Option<String>, String) {
    const BLOCKS: &[&str] = &["p", "div", "br", "li", "h1", "h2", "h3", "h4", "h5", "h6", "tr", "section", "article", "header", "footer", "blockquote", "pre", "hr", "table", "ul", "ol"];
    let mut title = None;
    let mut out = String::new();
    let mut rest = html;
    while !rest.is_empty() && out.len() < limit {
        let Some(i) = rest.find('<') else {
            out.push_str(&super::page::unescape(rest));
            break;
        };
        out.push_str(&super::page::unescape(&rest[..i]));
        rest = &rest[i..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        let Some(end) = rest.find('>') else { break };
        let closing = rest[1..].starts_with('/');
        let tag = rest[1..end].trim_start_matches('/');
        let name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_ascii_lowercase();
        rest = &rest[end + 1..];
        if closing {
            if BLOCKS.contains(&name.as_str()) {
                out.push('\n');
            }
            continue;
        }
        match name.as_str() {
            // Skip their contents entirely.
            "script" | "style" | "noscript" | "template" | "svg" => {
                let close = format!("</{name}");
                rest = find_ci(rest, &close).map_or("", |e| &rest[e..]);
            }
            "title" => {
                let e = find_ci(rest, "</title").unwrap_or(rest.len());
                title = Some(super::page::unescape(rest[..e].trim()));
                rest = &rest[e..];
            }
            n if BLOCKS.contains(&n) => out.push('\n'),
            _ => {}
        }
    }
    // Collapse runs of spaces within lines, like a browser.
    let text = out.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    (title, text)
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.to_ascii_lowercase().find(&needle.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_without_scripts_or_styles() {
        let html = r#"<!doctype html><html><head><title>My &amp; page</title><style>p{color:red}</style><script>alert("x<y")</script></head>
<body><!-- hidden --><h1>Hello</h1><p>First   paragraph<br>next line</p><div>caf&eacute; &#8212; ok</div></body></html>"#;
        let (title, text) = html_text(html, 1000);
        assert_eq!(title.as_deref(), Some("My & page"));
        assert_eq!(text, "Hello\nFirst paragraph\nnext line\ncafé — ok");
    }
}
