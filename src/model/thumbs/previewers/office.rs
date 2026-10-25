//! Office documents (Word, Excel, PowerPoint, LibreOffice). They're zip
//! files, and most carry a preview picture: LibreOffice always writes
//! `Thumbnails/thumbnail.png`, Microsoft Office `docProps/thumbnail.*` when
//! asked to. A Word document without one gets its first lines of text on a
//! page (see `page`); the others get the icon.

use super::super::{FileInfo, Previewer, Rendered};
use std::io::Read;

const EXTENSIONS: &[&str] = &["docx", "xlsx", "pptx", "odt", "ods", "odp", "odg", "ott", "ots", "otp", "docm", "xlsm", "pptm"];
const MAX_FILE: u64 = 512 << 20;
/// Larger parts are corrupt or hostile: don't inflate them.
const MAX_PART: u64 = 64 << 20;
const PREVIEWS: &[&str] = &["Thumbnails/thumbnail.png", "docProps/thumbnail.jpeg", "docProps/thumbnail.jpg", "docProps/thumbnail.png"];

pub struct Office;

impl Previewer for Office {
    fn name(&self) -> &'static str {
        "office"
    }

    fn accepts(&self, f: &FileInfo) -> bool {
        EXTENSIONS.contains(&f.ext.as_str())
    }

    fn render(&self, f: &FileInfo, max: u32) -> Result<Rendered, String> {
        if f.size > MAX_FILE {
            return Err("too large to preview".into());
        }
        let file = std::fs::File::open(&f.path).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for name in PREVIEWS {
            if let Some(bytes) = part(&mut zip, name) {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    return Ok(Rendered::Still(super::shrink(img.into_rgba8(), max)));
                }
            }
        }
        let xml = part(&mut zip, "word/document.xml").ok_or("no preview in this document")?;
        let text = word_text(&String::from_utf8_lossy(&xml), 3000);
        super::page::render(None, &text, max).map(Rendered::Still)
    }
}

fn part<R: std::io::Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let f = zip.by_name(name).ok()?;
    if f.size() > MAX_PART {
        return None;
    }
    let mut out = Vec::with_capacity(f.size() as usize);
    f.take(MAX_PART).read_to_end(&mut out).ok()?;
    Some(out)
}

/// The text of a Word document's body: runs (`<w:t>`), with paragraphs and
/// line breaks as newlines, up to `limit` characters.
pub fn word_text(xml: &str, limit: usize) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        let Some(j) = rest[i..].find('>') else { break };
        let tag = &rest[i + 1..i + j];
        rest = &rest[i + j + 1..];
        let name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        match name {
            "w:t" if !tag.ends_with('/') => {
                let end = rest.find("</w:t>").unwrap_or(rest.len());
                out.push_str(&super::page::unescape(&rest[..end]));
                rest = &rest[end..];
            }
            "w:tab" => out.push(' '),
            "w:br" => out.push('\n'),
            "" if tag == "/w:p" => out.push('\n'),
            _ => {}
        }
        if out.len() >= limit {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_text_follows_paragraphs() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> &amp; welcome</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t><w:tab/><w:t>line&#233;</w:t></w:r></w:p><w:p/></w:body></w:document>"#;
        assert_eq!(word_text(xml, 1000), "Hello & welcome\nSecond lineé\n");
    }

    #[test]
    fn docx_without_thumbnail_gets_a_text_page() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("fractale-docx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("letter.docx");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        z.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(br#"<w:document><w:body><w:p><w:r><w:t>Dear reader, this is a test letter with some words.</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
        z.finish().unwrap();
        let info = FileInfo::new(p.clone(), std::fs::metadata(&p).unwrap().len(), 0);
        let img = Office.render(&info, 256).unwrap();
        let img = img.still();
        assert_eq!(img.height(), 256);
        let dark = img.pixels().filter(|p| p.0[0] < 200).count();
        assert!(dark > 50, "text drawn: {dark} dark pixels");
        std::fs::write(&p, b"not a zip").unwrap();
        assert!(Office.render(&info, 256).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
