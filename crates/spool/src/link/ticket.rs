//! The ticket one file is played on.
//!
//! A player is sent to the machine that holds a file with a ticket in hand. The ticket
//! names one file, by the share it is in and its place there, and says when it runs out. It
//! is signed with a key made from the one the two Spools share, so only the Spool that was
//! given that key can make one, and the machine holding the file can check it without
//! asking anybody.
//!
//! A ticket can be used again until it runs out, on purpose: a player asks for a film in
//! parts and asks again every time someone skips. It lets the file be read, and nothing
//! else.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ring::{hkdf, hmac};
use serde::{Deserialize, Serialize};

/// What is signed along with the ticket, so that a signature made for anything else with
/// the same key is never taken for one.
const PURPOSE: &[u8] = b"spool/play/ticket/v1";
const VERSION: &str = "1";
/// How long a ticket lasts when the one who makes it does not say: long enough for a film
/// that is paused for the evening and taken up again.
pub const LIFE: u64 = 12 * 3600;
/// No ticket is honoured for longer than this, whatever it says.
const LONGEST: u64 = 24 * 3600;
/// How far the two machines' clocks may differ.
const SKEW: u64 = 60;
const LARGEST: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub share: String,
    /// The file's place inside the share.
    pub rel: String,
    /// When it was made and when it runs out, in seconds.
    pub made: u64,
    pub until: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    s: String,
    r: String,
    i: u64,
    e: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not a ticket, not one of ours, or changed since it was signed.
    Bad,
    Expired,
}

/// A place inside a share: plain names, one inside the next, and nothing that climbs out.
pub fn plain(rel: &str) -> bool {
    rel.len() <= 2048 && !rel.contains('\0') && rel.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// Makes and checks tickets with one link's key.
pub struct Signer(hmac::Key);

impl Signer {
    /// The key tickets are signed with is made from the link's key and is of no use for
    /// anything else, so that the link's own key never signs anything a player carries.
    pub fn new(secret: &str) -> Signer {
        let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, ring::digest::digest(&ring::digest::SHA256, b"spool/link/key/v1").as_ref());
        let (prk, info) = (salt.extract(secret.as_bytes()), [PURPOSE]);
        Signer(hmac::Key::from(prk.expand(&info, hmac::HMAC_SHA256).expect("a key of this length can always be made")))
    }

    fn signed(body: &str) -> Vec<u8> {
        [PURPOSE, b"\0", body.as_bytes()].concat()
    }

    pub fn sign(&self, t: &Ticket) -> String {
        let payload = serde_json::to_vec(&Payload { s: t.share.clone(), r: t.rel.clone(), i: t.made, e: t.until }).expect("a ticket can always be written");
        let body = format!("{VERSION}.{}", B64.encode(payload));
        let mac = hmac::sign(&self.0, &Self::signed(&body));
        format!("{body}.{}", B64.encode(mac.as_ref()))
    }

    /// A ticket for one file, good from now for the usual time.
    pub fn issue(&self, share: &str, rel: &str, now: u64) -> String {
        self.sign(&Ticket { share: share.to_string(), rel: rel.to_string(), made: now, until: now + LIFE })
    }

    pub fn verify(&self, token: &str, now: u64) -> Result<Ticket, Refused> {
        if token.len() > LARGEST {
            return Err(Refused::Bad);
        }
        let (body, mac) = token.rsplit_once('.').ok_or(Refused::Bad)?;
        let mac = B64.decode(mac).map_err(|_| Refused::Bad)?;
        // The signature is checked over what was sent, before any of it is read.
        hmac::verify(&self.0, &Self::signed(body), &mac).map_err(|_| Refused::Bad)?;
        let payload = body.strip_prefix(VERSION).and_then(|b| b.strip_prefix('.')).ok_or(Refused::Bad)?;
        let p: Payload = serde_json::from_slice(&B64.decode(payload).map_err(|_| Refused::Bad)?).map_err(|_| Refused::Bad)?;
        if !plain(&p.r) || p.s.is_empty() || p.s.len() > 64 || p.e <= p.i || p.e - p.i > LONGEST || p.i > now + SKEW {
            return Err(Refused::Bad);
        }
        if now > p.e + SKEW {
            return Err(Refused::Expired);
        }
        Ok(Ticket { share: p.s, rel: p.r, made: p.i, until: p.e })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn ticket() -> Ticket {
        Ticket { share: "a1b2c3d4e5f6".into(), rel: "Alpha (2001)/Alpha (2001).mkv".into(), made: NOW, until: NOW + LIFE }
    }

    #[test]
    fn a_ticket_is_read_back_as_it_was_made() {
        let s = Signer::new("a key shared by two Spools");
        let token = s.sign(&ticket());
        assert_eq!(s.verify(&token, NOW), Ok(ticket()));
        assert_eq!(s.verify(&token, NOW + LIFE), Ok(ticket()), "it is good until it runs out");
        assert_eq!(s.verify(&s.issue("a1b2c3d4e5f6", "Alpha (2001)/Alpha (2001).mkv", NOW), NOW), Ok(ticket()));
        assert!(token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)), "it goes in an address as it is: {token}");
    }

    #[test]
    fn a_ticket_is_the_same_for_the_same_key_and_file() {
        // Both ends must go on making the same thing of the same key, release after release.
        let token = Signer::new("a key shared by two Spools").sign(&ticket());
        assert_eq!(token, "1.eyJzIjoiYTFiMmMzZDRlNWY2IiwiciI6IkFscGhhICgyMDAxKS9BbHBoYSAoMjAwMSkubWt2IiwiaSI6MTgwMDAwMDAwMCwiZSI6MTgwMDA0MzIwMH0.XAjPasVcHvXWFnr0FaLqjEZ1B16CAu9p3HDGmhSSv_I");
    }

    #[test]
    fn a_ticket_runs_out_and_clocks_may_differ_a_little() {
        let s = Signer::new("k");
        let token = s.sign(&ticket());
        assert_eq!(s.verify(&token, NOW + LIFE + SKEW), Ok(ticket()));
        assert_eq!(s.verify(&token, NOW + LIFE + SKEW + 1), Err(Refused::Expired));
        assert_eq!(s.verify(&token, NOW - SKEW), Ok(ticket()));
        assert_eq!(s.verify(&token, NOW - SKEW - 1), Err(Refused::Bad), "one made in the future is not believed");
    }

    #[test]
    fn nothing_but_a_ticket_of_ours_is_taken() {
        let s = Signer::new("the right key");
        let token = s.sign(&ticket());
        assert_eq!(Signer::new("another key").verify(&token, NOW), Err(Refused::Bad));
        // Any one character changed, anywhere.
        for i in 0..token.len() {
            let mut bytes = token.clone().into_bytes();
            bytes[i] = if bytes[i] == b'A' { b'B' } else { b'A' };
            let changed = String::from_utf8(bytes).unwrap();
            assert_eq!(s.verify(&changed, NOW), Err(Refused::Bad), "changed at {i}");
        }
        let long = "A".repeat(LARGEST + 1);
        for junk in ["", ".", "..", "1..", "1.e30.", "2.e30.AAAA", long.as_str(), "not a ticket at all"] {
            assert_eq!(s.verify(junk, NOW), Err(Refused::Bad), "{junk:.40}");
        }
    }

    #[test]
    fn a_signed_ticket_that_says_too_much_is_still_refused() {
        let s = Signer::new("k");
        let bad = |t: Ticket| s.verify(&s.sign(&t), NOW);
        for rel in ["../outside.mkv", "/etc/passwd", "a/../../b", "./a", "", "a\0b"] {
            assert_eq!(bad(Ticket { rel: rel.into(), ..ticket() }), Err(Refused::Bad), "{rel:?}");
        }
        assert_eq!(bad(Ticket { share: String::new(), ..ticket() }), Err(Refused::Bad));
        assert_eq!(bad(Ticket { until: NOW, ..ticket() }), Err(Refused::Bad), "it must last some time");
        assert_eq!(bad(Ticket { until: NOW + LONGEST + 1, ..ticket() }), Err(Refused::Bad), "and not for ever");
        // A field this release does not know means a ticket it cannot answer for.
        let body = format!("1.{}", B64.encode(br#"{"s":"a","r":"b","i":1800000000,"e":1800000100,"m":"PUT"}"#));
        let token = format!("{body}.{}", B64.encode(hmac::sign(&s.0, &Signer::signed(&body)).as_ref()));
        assert_eq!(s.verify(&token, NOW), Err(Refused::Bad));
    }

    #[test]
    fn places_inside_a_share_are_plain() {
        assert!(plain("Alpha (2001)/Season 01/a.mkv"));
        assert!(plain("a..b/c"));
        for rel in ["", "/a", "..", "a/..", "a/./b", "a//b", "a/", "a\0"] {
            assert!(!plain(rel), "{rel:?}");
        }
    }
}
