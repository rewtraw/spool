//! Whether a file is an EPUB that can be read, and what it says it is. A file from Anna's
//! Archive is the file its MD5 names, but not always the book the catalogue says: the
//! check is that it opens, is not locked by DRM, has the text its contents list, and names
//! itself as the book wanted.

use anyhow::{bail, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::Read;
use std::path::Path;

/// What an EPUB says about itself.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Opened {
    pub title: String,
    pub authors: Vec<String>,
    pub language: String,
    pub isbns: Vec<String>,
    /// Bytes of text in the documents it is read through.
    pub text_bytes: u64,
}

/// Encryption an EPUB may have without being locked: the scrambling of embedded fonts.
const FONT_SCRAMBLES: &[&str] = &["http://www.idpf.org/2008/embedding", "http://ns.adobe.com/pdf/enc#RC"];

/// At most this much of its description and contents list is read, so that a file built to
/// blow up when unpacked cannot.
const MOST: u64 = 4 << 20;

/// More parts than any book has, or more unpacked than any book needs, is not a book.
const MOST_PARTS: usize = 20_000;
const MOST_UNPACKED: u64 = 2 << 30;
/// Each part it is read through is read whole, which checks it against its checksum, up to
/// these limits.
const MOST_CHAPTER: u64 = 64 << 20;
const MOST_TEXT: u64 = 400 << 20;

fn part<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Result<Option<String>> {
    let Ok(f) = zip.by_name(name) else { return Ok(None) };
    let mut s = String::new();
    f.take(MOST).read_to_string(&mut s).with_context(|| format!("{name} could not be read"))?;
    Ok(Some(s))
}

/// The text between tags and the attributes of each tag, by local name.
fn events(xml: &str, mut each: impl FnMut(&str, &[(String, String)], Option<&str>)) -> Result<()> {
    let mut r = Reader::from_str(xml);
    let mut open: Vec<(String, Vec<(String, String)>, String)> = vec![];
    loop {
        match r.read_event()? {
            Event::Eof => break,
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                let attrs = e.attributes().flatten().map(|a| (String::from_utf8_lossy(a.key.local_name().as_ref()).to_string(), a.unescape_value().map(|v| v.into_owned()).unwrap_or_default())).collect();
                open.push((name, attrs, String::new()));
            }
            Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                let attrs: Vec<(String, String)> = e.attributes().flatten().map(|a| (String::from_utf8_lossy(a.key.local_name().as_ref()).to_string(), a.unescape_value().map(|v| v.into_owned()).unwrap_or_default())).collect();
                each(&name, &attrs, None);
            }
            Event::Text(t) => {
                if let (Some(top), Ok(s)) = (open.last_mut(), t.decode()) {
                    top.2.push_str(&quick_xml::escape::unescape(&s).map(|c| c.into_owned()).unwrap_or_else(|_| s.into_owned()));
                }
            }
            Event::GeneralRef(g) => {
                if let (Some(top), Ok(name)) = (open.last_mut(), g.decode()) {
                    top.2.push_str(match name.as_ref() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "",
                    });
                }
            }
            Event::CData(t) => {
                if let Some(top) = open.last_mut() {
                    top.2.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::End(_) => {
                if let Some((name, attrs, text)) = open.pop() {
                    each(&name, &attrs, Some(text.trim()));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Open an EPUB and read what it says it is; an error says why it cannot be read here.
pub fn open(path: &Path) -> Result<Opened> {
    let f = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(f).context("it is not an EPUB: it does not open as one")?;
    if zip.len() > MOST_PARTS {
        bail!("it has {} parts, more than a book has", zip.len());
    }
    let mut unpacked = 0u64;
    for i in 0..zip.len() {
        unpacked += zip.by_index_raw(i).map(|f| f.size()).unwrap_or(0);
    }
    if unpacked > MOST_UNPACKED {
        bail!("it unpacks to {} GB, more than a book needs", unpacked >> 30);
    }
    if zip.by_name("META-INF/rights.xml").is_ok() {
        bail!("it is locked by Adobe DRM");
    }
    if let Some(enc) = part(&mut zip, "META-INF/encryption.xml")? {
        let mut locked = false;
        events(&enc, |name, attrs, _| {
            if name == "EncryptionMethod" && attr(attrs, "Algorithm").is_some_and(|a| !FONT_SCRAMBLES.contains(&a)) {
                locked = true;
            }
        })?;
        if locked {
            bail!("it is locked by DRM");
        }
    }
    let container = part(&mut zip, "META-INF/container.xml")?.context("it is not an EPUB: it has no META-INF/container.xml")?;
    let mut opf_path = None;
    events(&container, |name, attrs, _| {
        if name == "rootfile" && opf_path.is_none() {
            opf_path = attr(attrs, "full-path").map(String::from);
        }
    })?;
    let opf_path = opf_path.context("it is not an EPUB: its container names no package")?;
    let opf = part(&mut zip, &opf_path)?.with_context(|| format!("its package {opf_path} is missing"))?;
    let base = opf_path.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();

    let mut out = Opened::default();
    let mut manifest: Vec<(String, String)> = vec![];
    let mut spine: Vec<String> = vec![];
    events(&opf, |name, attrs, text| match (name, text) {
        ("title", Some(t)) if out.title.is_empty() && !t.is_empty() => out.title = t.to_string(),
        ("creator", Some(t)) if !t.is_empty() => out.authors.push(t.to_string()),
        ("language", Some(t)) if out.language.is_empty() => out.language = t.to_lowercase(),
        ("identifier", Some(t)) => {
            if let Some(i) = super::text::isbn13(t.trim_start_matches("urn:isbn:")).filter(|_| t.chars().filter(|c| c.is_ascii_digit()).count() >= 10) {
                out.isbns.push(i);
            }
        }
        ("item", _) => {
            if let (Some(id), Some(href)) = (attr(attrs, "id"), attr(attrs, "href")) {
                manifest.push((id.to_string(), href.to_string()));
            }
        }
        ("itemref", _) => {
            if let Some(id) = attr(attrs, "idref") {
                spine.push(id.to_string());
            }
        }
        _ => {}
    })?;
    if spine.is_empty() {
        bail!("it has nothing to read: its spine is empty");
    }
    let mut missing = 0;
    for id in &spine {
        let Some((_, href)) = manifest.iter().find(|(i, _)| i == id) else {
            missing += 1;
            continue;
        };
        let href = urlencoding::decode(href.split('#').next().unwrap_or(href)).map(|c| c.into_owned()).unwrap_or_else(|_| href.clone());
        let full = normalize(&format!("{base}{href}"));
        let Ok(f) = zip.by_name(&full) else {
            missing += 1;
            continue;
        };
        let size = f.size();
        if size > MOST_CHAPTER || out.text_bytes + size > MOST_TEXT {
            bail!("a part of it is too large to be text");
        }
        let read = std::io::copy(&mut f.take(MOST_CHAPTER), &mut std::io::sink()).with_context(|| format!("{full} is damaged"))?;
        if read != size {
            bail!("{full} is damaged: it holds {read} bytes of {size}");
        }
        out.text_bytes += read;
    }
    if missing * 4 > spine.len() {
        bail!("{missing} of the {} parts it is read through are missing", spine.len());
    }
    if out.text_bytes < 8_000 {
        bail!("it has almost no text ({} bytes)", out.text_bytes);
    }
    out.isbns.sort();
    out.isbns.dedup();
    Ok(out)
}

/// A path inside the zip with "." and ".." worked out.
fn normalize(p: &str) -> String {
    let mut parts: Vec<&str> = vec![];
    for s in p.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A small EPUB, for tests: `encryption` adds an encryption.xml using that algorithm.
    pub fn make(path: &Path, title: &str, author: &str, isbn: &str, words: usize, encryption: Option<&str>) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o: zip::write::SimpleFileOptions = Default::default();
        z.start_file("mimetype", o).unwrap();
        z.write_all(b"application/epub+zip").unwrap();
        z.start_file("META-INF/container.xml", o).unwrap();
        z.write_all(br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#).unwrap();
        if let Some(alg) = encryption {
            z.start_file("META-INF/encryption.xml", o).unwrap();
            z.write_all(format!(r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData><enc:EncryptionMethod Algorithm="{alg}"/></enc:EncryptedData></encryption>"#).as_bytes()).unwrap();
        }
        z.start_file("OEBPS/content.opf", o).unwrap();
        z.write_all(format!(r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{title}</dc:title><dc:creator>{author}</dc:creator><dc:language>en</dc:language><dc:identifier>urn:isbn:{isbn}</dc:identifier></metadata><manifest><item id="c1" href="Text/ch%201.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/></spine></package>"#).as_bytes()).unwrap();
        z.start_file("OEBPS/Text/ch 1.xhtml", o).unwrap();
        z.write_all(format!("<html><body><p>{}</p></body></html>", "word ".repeat(words)).as_bytes()).unwrap();
        z.finish().unwrap();
    }

    #[test]
    fn an_epub_says_what_it_is() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("b.epub");
        make(&p, "The Left Hand of Darkness &amp; More", "Ursula K. Le Guin", "978-0-441-47812-5", 5000, None);
        let o = open(&p).unwrap();
        assert_eq!(o.title, "The Left Hand of Darkness & More");
        assert_eq!(o.authors, vec!["Ursula K. Le Guin"]);
        assert_eq!(o.language, "en");
        assert_eq!(o.isbns, vec!["9780441478125"]);
        assert!(o.text_bytes > 20_000);
    }

    #[test]
    fn locked_empty_or_broken_files_are_turned_away() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("b.epub");
        make(&p, "T", "A", "", 5000, Some("http://www.idpf.org/2008/embedding"));
        assert!(open(&p).is_ok(), "scrambled fonts are not a lock");
        make(&p, "T", "A", "", 5000, Some("http://www.w3.org/2001/04/xmlenc#aes128-cbc"));
        assert!(open(&p).unwrap_err().to_string().contains("DRM"));
        make(&p, "T", "A", "", 10, None);
        assert!(open(&p).unwrap_err().to_string().contains("almost no text"));
        std::fs::write(&p, b"not a zip").unwrap();
        assert!(open(&p).is_err());
        // A chapter damaged inside the zip, where its contents list still names it.
        make(&p, "T", "A", "", 5000, None);
        let mut bytes = std::fs::read(&p).unwrap();
        let at = bytes.windows(6).position(|w| w == b"Text/c").unwrap();
        let data = bytes[at..].windows(4).position(|w| w == b"PK\x03\x04" || w == b"PK\x01\x02").unwrap() + at;
        for b in &mut bytes[data - 40..data - 20] {
            *b ^= 0x5a;
        }
        std::fs::write(&p, &bytes).unwrap();
        assert!(open(&p).is_err(), "a damaged chapter is caught");
    }
}
