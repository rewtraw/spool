//! How Soulseek puts things on the wire: little-endian numbers, strings and byte runs each
//! with their length before them, and messages framed by their length. Messages to and from
//! the server and between peers carry a four-byte code; those that open a connection and
//! those of the search network, a one-byte code.

use std::io::{self, Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt};

/// A message being written.
#[derive(Default)]
pub struct Out(pub Vec<u8>);

impl Out {
    pub fn new() -> Out {
        Out(Vec::new())
    }
    pub fn u8(mut self, v: u8) -> Out {
        self.0.push(v);
        self
    }
    pub fn u32(mut self, v: u32) -> Out {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i32(mut self, v: i32) -> Out {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(mut self, v: u64) -> Out {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn bool(self, v: bool) -> Out {
        self.u8(v as u8)
    }
    pub fn str(self, s: &str) -> Out {
        self.bytes(s.as_bytes())
    }
    pub fn bytes(mut self, b: &[u8]) -> Out {
        self.0.extend_from_slice(&(b.len() as u32).to_le_bytes());
        self.0.extend_from_slice(b);
        self
    }

    /// Framed with a four-byte code: a message to the server or to a peer.
    pub fn msg(self, code: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.0.len() + 8);
        v.extend_from_slice(&(self.0.len() as u32 + 4).to_le_bytes());
        v.extend_from_slice(&code.to_le_bytes());
        v.extend_from_slice(&self.0);
        v
    }

    /// Framed with a one-byte code: opening a connection, or the search network.
    pub fn msg8(self, code: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.0.len() + 5);
        v.extend_from_slice(&(self.0.len() as u32 + 1).to_le_bytes());
        v.push(code);
        v.extend_from_slice(&self.0);
        v
    }

    /// Compressed whole, as search results and lists of files are sent.
    pub fn zlib(self) -> Out {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = e.write_all(&self.0);
        Out(e.finish().unwrap_or_default())
    }
}

/// A message being read.
pub struct In<'a> {
    b: &'a [u8],
}

fn short() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "a message was cut short")
}

impl<'a> In<'a> {
    pub fn new(b: &'a [u8]) -> In<'a> {
        In { b }
    }
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.b.len() < n {
            return Err(short());
        }
        let (a, b) = self.b.split_at(n);
        self.b = b;
        Ok(a)
    }
    pub fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().expect("two bytes")))
    }
    pub fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("four bytes")))
    }
    pub fn i32(&mut self) -> io::Result<i32> {
        Ok(self.u32()? as i32)
    }
    pub fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().expect("eight bytes")))
    }
    pub fn bool(&mut self) -> io::Result<bool> {
        Ok(self.u8()? != 0)
    }
    pub fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    /// A string as UTF-8, or, from the old clients that send Latin-1, as that.
    pub fn str(&mut self) -> io::Result<String> {
        let b = self.bytes()?;
        Ok(match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => b.iter().map(|&c| c as char).collect(),
        })
    }
    /// A string only if it is UTF-8: a path that is not could not be asked for again as it was given.
    pub fn utf8(&mut self) -> io::Result<Option<String>> {
        let b = self.bytes()?;
        Ok(std::str::from_utf8(b).ok().map(str::to_string))
    }
    pub fn is_empty(&self) -> bool {
        self.b.is_empty()
    }
    pub fn rest(&self) -> &'a [u8] {
        self.b
    }
    /// How many of something a message says follow, held to what it could hold, so a
    /// garbled count asks for no more memory than the message's own size.
    pub fn count(&mut self, each_at_least: usize) -> io::Result<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(each_at_least.max(1)) > self.b.len() {
            return Err(short());
        }
        Ok(n)
    }
}

/// The most a compressed message may grow to once read.
const INFLATED_MOST: u64 = 64 << 20;

pub fn inflate(b: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(b).take(INFLATED_MOST + 1).read_to_end(&mut out)?;
    if out.len() as u64 > INFLATED_MOST {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a compressed message was too big"));
    }
    Ok(out)
}

/// Read one framed message: its code and what follows, no bigger than `most`.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, most: usize) -> io::Result<Vec<u8>> {
    let n = r.read_u32_le().await? as usize;
    if n > most {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a message of {n} bytes was more than the {most} allowed")));
    }
    let mut b = vec![0; n];
    r.read_exact(&mut b).await?;
    Ok(b)
}

/// The four-byte code at the head of a message, and the rest.
pub fn code32(b: &[u8]) -> io::Result<(u32, &[u8])> {
    let mut i = In::new(b);
    let c = i.u32()?;
    Ok((c, i.rest()))
}

/// The one-byte code at the head of a message, and the rest.
pub fn code8(b: &[u8]) -> io::Result<(u8, &[u8])> {
    let mut i = In::new(b);
    let c = i.u8()?;
    Ok((c, i.rest()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_is_framed_as_the_protocol_shows_it() {
        // The example login from the protocol's documentation.
        let m = Out::new().str("username").str("password").u32(177).str("d51c9a7e9353746a6020f9602d452929").u32(1).msg(1);
        assert_eq!(&m[..12], &[0x48, 0, 0, 0, 1, 0, 0, 0, 8, 0, 0, 0]);
        assert_eq!(m.len(), 0x48 + 4);
    }

    #[test]
    fn what_is_written_reads_back() {
        let m = Out::new().u8(7).u32(42).u64(1 << 40).bool(true).str("Café").msg(9);
        let mut i = In::new(&m[4..]);
        assert_eq!(i.u32().unwrap(), 9);
        assert_eq!((i.u8().unwrap(), i.u32().unwrap(), i.u64().unwrap(), i.bool().unwrap()), (7, 42, 1 << 40, true));
        assert_eq!(i.str().unwrap(), "Café");
        assert!(i.is_empty());
        assert!(i.u8().is_err(), "nothing more to read");
    }

    #[test]
    fn latin1_is_read_but_not_taken_as_a_path() {
        let m = Out::new().bytes(&[b'C', b'a', b'f', 0xe9]);
        assert_eq!(In::new(&m.0).str().unwrap(), "Café");
        assert_eq!(In::new(&m.0).utf8().unwrap(), None);
    }

    #[test]
    fn a_garbled_count_is_refused() {
        let m = Out::new().u32(1_000_000).u8(1);
        assert!(In::new(&m.0).count(10).is_err());
    }

    #[test]
    fn compressed_messages_come_back() {
        let z = Out::new().str("hello").zlib();
        let back = inflate(&z.0).unwrap();
        assert_eq!(In::new(&back).str().unwrap(), "hello");
    }
}
