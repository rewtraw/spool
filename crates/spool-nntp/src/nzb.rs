//! NZB parsing.

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub number: u32,
    pub bytes: u64,
    pub message_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NzbFile {
    pub subject: String,
    pub poster: String,
    pub date: i64,
    pub groups: Vec<String>,
    pub segments: Vec<Segment>,
}

impl NzbFile {
    /// Encoded size of all segments. The decoded file is a few percent smaller.
    pub fn bytes(&self) -> u64 {
        self.segments.iter().map(|s| s.bytes).sum()
    }

    /// Best guess at the file name from the subject, before any article has been fetched.
    pub fn subject_filename(&self) -> Option<String> {
        let s = &self.subject;
        if let (Some(a), Some(b)) = (s.find('"'), s.rfind('"')) {
            if b > a + 1 {
                return Some(s[a + 1..b].to_string());
            }
        }
        // "name.ext (1/23)" or "name.ext yEnc (1/23)"
        let cut = s.find(" yEnc").or_else(|| s.rfind(" (")).unwrap_or(s.len());
        let candidate = s[..cut].trim();
        let last = candidate.rsplit(' ').next().unwrap_or("");
        if last.contains('.') {
            Some(last.to_string())
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Nzb {
    pub title: Option<String>,
    pub password: Option<String>,
    pub files: Vec<NzbFile>,
}

impl Nzb {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes()).sum()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NzbError {
    #[error("NZB is not valid XML: {0}")]
    Xml(String),
    #[error("NZB contains no files")]
    Empty,
}

fn attr(e: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == name).and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

pub fn parse(data: &[u8]) -> Result<Nzb, NzbError> {
    let text = String::from_utf8_lossy(data);
    let mut reader = Reader::from_str(&text);
    let mut nzb = Nzb::default();
    let mut file: Option<NzbFile> = None;
    let mut segment: Option<Segment> = None;
    let mut meta_type: Option<String> = None;
    let mut in_group = false;
    let mut text_buf = String::new();

    loop {
        match reader.read_event() {
            Err(e) => return Err(NzbError::Xml(e.to_string())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                text_buf.clear();
                match e.local_name().as_ref() {
                    b"file" => {
                        file = Some(NzbFile {
                            subject: attr(&e, b"subject").unwrap_or_default(),
                            poster: attr(&e, b"poster").unwrap_or_default(),
                            date: attr(&e, b"date").and_then(|d| d.parse().ok()).unwrap_or(0),
                            groups: vec![],
                            segments: vec![],
                        })
                    }
                    b"segment" => {
                        segment = Some(Segment {
                            number: attr(&e, b"number").and_then(|d| d.parse().ok()).unwrap_or(0),
                            bytes: attr(&e, b"bytes").and_then(|d| d.parse().ok()).unwrap_or(0),
                            message_id: String::new(),
                        })
                    }
                    b"group" => in_group = true,
                    b"meta" => meta_type = attr(&e, b"type"),
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if let Ok(s) = t.decode() {
                    text_buf.push_str(&quick_xml::escape::unescape(&s).map(|c| c.into_owned()).unwrap_or_else(|_| s.into_owned()));
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if let Ok(name) = r.decode() {
                    let resolved = match name.as_ref() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "",
                    };
                    text_buf.push_str(resolved);
                }
            }
            Ok(Event::CData(t)) => text_buf.push_str(&String::from_utf8_lossy(&t)),
            Ok(Event::End(e)) => {
                match e.local_name().as_ref() {
                    b"segment" => {
                        if let (Some(mut s), Some(f)) = (segment.take(), file.as_mut()) {
                            s.message_id = text_buf.trim().trim_matches(['<', '>']).to_string();
                            if !s.message_id.is_empty() {
                                f.segments.push(s);
                            }
                        }
                    }
                    b"group" => {
                        if let (true, Some(f)) = (in_group, file.as_mut()) {
                            f.groups.push(text_buf.trim().to_string());
                        }
                        in_group = false;
                    }
                    b"file" => {
                        if let Some(mut f) = file.take() {
                            f.segments.sort_by_key(|s| s.number);
                            f.segments.dedup_by_key(|s| s.number);
                            if !f.segments.is_empty() {
                                nzb.files.push(f);
                            }
                        }
                    }
                    b"meta" => {
                        match meta_type.take().as_deref() {
                            Some("password") => nzb.password = Some(text_buf.trim().to_string()),
                            Some("title") | Some("name") => nzb.title = Some(text_buf.trim().to_string()),
                            _ => {}
                        }
                    }
                    _ => {}
                }
                text_buf.clear();
            }
            _ => {}
        }
    }
    if nzb.files.is_empty() {
        return Err(NzbError::Empty);
    }
    Ok(nzb)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
 <head><meta type="password">s3cret &amp; more</meta><meta type="title">My Release</meta></head>
 <file poster="a@b.c (poster)" date="1700000000" subject="[1/2] - &quot;movie.part01.rar&quot; yEnc (1/2)">
  <groups><group>alt.binaries.test</group></groups>
  <segments>
   <segment bytes="500" number="2">part2@example</segment>
   <segment bytes="700" number="1">&lt;part1@example&gt;</segment>
  </segments>
 </file>
 <file poster="p" date="1" subject="movie.nfo (1/1)"><groups><group>g</group></groups><segments><segment bytes="10" number="1">n@x</segment></segments></file>
</nzb>"#;

    #[test]
    fn parses_files_segments_and_meta() {
        let nzb = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(nzb.password.as_deref(), Some("s3cret & more"));
        assert_eq!(nzb.title.as_deref(), Some("My Release"));
        assert_eq!(nzb.files.len(), 2);
        let f = &nzb.files[0];
        assert_eq!(f.subject_filename().as_deref(), Some("movie.part01.rar"));
        assert_eq!(f.segments.iter().map(|s| s.number).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(f.segments[0].message_id, "part1@example");
        assert_eq!(f.bytes(), 1200);
        assert_eq!(nzb.files[1].subject_filename().as_deref(), Some("movie.nfo"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(parse(b"<nzb></nzb>"), Err(NzbError::Empty)));
        assert!(parse(b"not xml at all <<<").is_err());
    }
}
