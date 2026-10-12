//! Linking two Spools with a short code.
//!
//! One Spool gives a code: a helper prints it when it starts, and a Spool with a page of
//! its own shows it. Whoever types that code into the other Spool has shown that they
//! stand at both, and the two agree a key between themselves that nobody had to copy.
//!
//! A code is short enough to type, which makes it short enough to guess, so it is never
//! sent and nothing made from it alone is. The two run SPAKE2: each sends one message, and
//! only two that hold the same code come out with the same secret. Someone listening
//! learns nothing to test guesses against, and someone taking part gets one guess for each
//! try, of which the giver allows few. Each then proves to the other that it came out with
//! the secret before either takes the key into use.
//!
//! The asker is the one the code is typed into; the giver is the one that gave it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ring::{hkdf, hmac};
use spake2::{Ed25519Group, Identity, Password, Spake2};

/// What every key made here is bound to, so that nothing agreed for one purpose or by one
/// version of this is of use for another.
const PURPOSE: &[u8] = b"spool/link/pair/v1";
const ASKER: &[u8] = b"spool/link/pair/v1/asker";
const GIVER: &[u8] = b"spool/link/pair/v1/giver";
/// How many digits a code has.
pub const DIGITS: usize = 8;

#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    /// What the other side sent is not part of this at all.
    Garbled,
    /// The two did not come out with the same secret: the codes differ.
    WrongCode,
}

/// A code as it is compared: its digits, without the spaces or dashes it was written with.
pub fn tidy(code: &str) -> String {
    code.chars().filter(char::is_ascii_digit).collect()
}

/// A code as it is shown: in two halves.
pub fn shown(code: &str) -> String {
    let code = tidy(code);
    let half = code.len() / 2;
    format!("{} {}", &code[..half], &code[half..])
}

/// A new code, every one as likely as any other.
pub fn new_code() -> String {
    use ring::rand::SecureRandom;
    let random = ring::rand::SystemRandom::new();
    let mut code = String::new();
    while code.len() < DIGITS {
        let mut byte = [0u8; 1];
        random.fill(&mut byte).expect("the system gives random numbers");
        // 250 is the most of a byte that divides evenly into digits.
        if byte[0] < 250 {
            code.push(char::from(b'0' + byte[0] % 10));
        }
    }
    code
}

/// A name for one try, that nobody else could make up.
pub fn session() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 12];
    ring::rand::SystemRandom::new().fill(&mut bytes).expect("the system gives random numbers");
    B64.encode(bytes)
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

/// What the two hold once they have the same secret: the key for the link, and what each
/// says to prove it.
struct Made {
    key: String,
    asker_says: Vec<u8>,
    giver_says: Vec<u8>,
}

/// Everything is made from the secret and from both messages as they were sent, so a
/// message changed on the way gives keys that match nothing.
fn make(secret: &[u8], asked: &[u8], gave: &[u8]) -> Made {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, ring::digest::digest(&ring::digest::SHA256, PURPOSE).as_ref());
    let prk = salt.extract(secret);
    let said = [(asked.len() as u32).to_be_bytes().as_slice(), asked, gave].concat();
    let mut key = [0u8; 32];
    prk.expand(&[PURPOSE, b"/key", &said], Len(32)).and_then(|okm| okm.fill(&mut key)).expect("a key of this length can always be made");
    let proof = |who: &[u8]| {
        let k = hmac::Key::from(prk.expand(&[who, b"/proof", &said], hmac::HMAC_SHA256).expect("a key of this length can always be made"));
        hmac::sign(&k, &said).as_ref().to_vec()
    };
    Made { key: B64.encode(key), asker_says: proof(ASKER), giver_says: proof(GIVER) }
}

fn same(a: &[u8], b: &[u8]) -> bool {
    // In the same time whatever is sent.
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The asker's side, between sending its message and hearing the giver's.
pub struct Asking {
    state: Spake2<Ed25519Group>,
    asked: Vec<u8>,
}

impl Asking {
    /// Begin, with the code as it was typed. Returns what to send.
    pub fn start(code: &str) -> (Asking, String) {
        let (state, asked) = Spake2::<Ed25519Group>::start_a(&Password::new(tidy(code).as_bytes()), &Identity::new(ASKER), &Identity::new(GIVER));
        let sent = B64.encode(&asked);
        (Asking { state, asked }, sent)
    }

    /// Take the giver's message and its proof. Returns the key for the link, and the proof
    /// to send back; the key is of use only once the giver has taken that proof.
    pub fn finish(self, gave: &str, proof: &str) -> Result<(String, String), Failed> {
        let gave = B64.decode(gave).map_err(|_| Failed::Garbled)?;
        let proof = B64.decode(proof).map_err(|_| Failed::Garbled)?;
        let secret = self.state.finish(&gave).map_err(|_| Failed::Garbled)?;
        let made = make(&secret, &self.asked, &gave);
        if !same(&made.giver_says, &proof) {
            return Err(Failed::WrongCode);
        }
        Ok((made.key, B64.encode(made.asker_says)))
    }
}

/// The giver's side, between answering the asker and hearing its proof.
pub struct Giving {
    key: String,
    asker_says: Vec<u8>,
}

impl Giving {
    /// Answer an asker's message, with the code this side gave. Returns what to send: this
    /// side's message and its proof.
    pub fn answer(code: &str, asked: &str) -> Result<(Giving, String, String), Failed> {
        let asked = B64.decode(asked).map_err(|_| Failed::Garbled)?;
        let (state, gave) = Spake2::<Ed25519Group>::start_b(&Password::new(tidy(code).as_bytes()), &Identity::new(ASKER), &Identity::new(GIVER));
        let secret = state.finish(&asked).map_err(|_| Failed::Garbled)?;
        let made = make(&secret, &asked, &gave);
        Ok((Giving { key: made.key, asker_says: made.asker_says }, B64.encode(&gave), B64.encode(made.giver_says)))
    }

    /// Whether this is the asker's proof for this try.
    pub fn proved(&self, proof: &str) -> bool {
        B64.decode(proof).is_ok_and(|p| same(&self.asker_says, &p))
    }

    /// The key for the link. Of use only once the asker has proved itself.
    pub fn key(&self) -> &str {
        &self.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_that_hold_the_same_code_agree_a_key() {
        let (asking, asked) = Asking::start("1234 5678");
        let (giving, gave, giver_proof) = Giving::answer("12345678", &asked).unwrap();
        let (key, asker_proof) = asking.finish(&gave, &giver_proof).unwrap();
        assert!(giving.proved(&asker_proof));
        assert_eq!(giving.key(), key);
        assert_eq!(B64.decode(&key).unwrap().len(), 32);
        // Another try with the same code agrees another key.
        let (asking, asked) = Asking::start("12345678");
        let (_, gave, giver_proof) = Giving::answer("12345678", &asked).unwrap();
        assert_ne!(asking.finish(&gave, &giver_proof).unwrap().0, key);
    }

    #[test]
    fn a_wrong_code_agrees_nothing_and_proves_nothing() {
        let (asking, asked) = Asking::start("12345678");
        let (giving, gave, giver_proof) = Giving::answer("12345679", &asked).unwrap();
        assert_eq!(asking.finish(&gave, &giver_proof).err(), Some(Failed::WrongCode));
        // An asker that guessed wrong has no proof the giver will take, whatever it sends.
        let (other, asked) = Asking::start("00000000");
        let (_, gave, proof) = Giving::answer("00000000", &asked).unwrap();
        let (_, stolen) = other.finish(&gave, &proof).unwrap();
        assert!(!giving.proved(&stolen) && !giving.proved("") && !giving.proved("not base64!"));
    }

    #[test]
    fn a_message_changed_on_the_way_agrees_nothing() {
        let (asking, asked) = Asking::start("12345678");
        let (_, gave, giver_proof) = Giving::answer("12345678", &asked).unwrap();
        let mut changed = B64.decode(&gave).unwrap();
        let last = changed.len() - 1;
        changed[last] ^= 1;
        assert!(asking.finish(&B64.encode(changed), &giver_proof).is_err());
        assert_eq!(Giving::answer("12345678", "not base64!").err(), Some(Failed::Garbled));
        assert_eq!(Giving::answer("12345678", &B64.encode(b"too short")).err(), Some(Failed::Garbled));
    }

    #[test]
    fn a_code_is_digits_however_it_is_written() {
        let code = new_code();
        assert!(code.len() == DIGITS && code.chars().all(|c| c.is_ascii_digit()), "{code}");
        assert_ne!(new_code(), new_code());
        assert_eq!(tidy(" 1234-5678 "), "12345678");
        assert_eq!(shown("12345678"), "1234 5678");
    }
}
