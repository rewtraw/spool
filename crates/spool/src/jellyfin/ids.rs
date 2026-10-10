//! Item ids as players see them. Jellyfin's are 32 hex digits; these are Spool's own numbers
//! written in that form, so nothing has to be stored to turn one back into the other.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Id {
    Root,
    Movies,
    Shows,
    User,
    Movie(i64),
    Series(i64),
    /// Title id and season number.
    Season(i64, u32),
    Episode(i64),
    File(i64),
    /// The metadata source (1 TMDB, 2 TVmaze) and its id for the person.
    Person(u8, u64),
    Collection(u32),
    Genre(u64),
    Studio(u64),
}

impl Id {
    fn parts(self) -> (u8, u64, u64) {
        match self {
            Id::Root => (0x01, 0, 1),
            Id::Movies => (0x02, 0, 1),
            Id::Shows => (0x03, 0, 1),
            Id::User => (0x04, 0, 1),
            Id::Movie(id) => (0x10, 0, id as u64),
            Id::Series(id) => (0x11, 0, id as u64),
            Id::Season(id, n) => (0x12, n as u64, id as u64),
            Id::Episode(id) => (0x13, 0, id as u64),
            Id::File(id) => (0x14, 0, id as u64),
            Id::Person(source, id) => (0x20, source as u64, id),
            Id::Collection(id) => (0x21, 0, id as u64),
            Id::Genre(h) => (0x22, 0, h),
            Id::Studio(h) => (0x23, 0, h),
        }
    }

    pub fn parse(text: &str) -> Option<Id> {
        let hex: String = text.chars().filter(|c| *c != '-').collect();
        if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let tag = u8::from_str_radix(&hex[..2], 16).ok()?;
        let a = u64::from_str_radix(&hex[2..16], 16).ok()?;
        let b = u64::from_str_radix(&hex[16..], 16).ok()?;
        Some(match tag {
            0x01 => Id::Root,
            0x02 => Id::Movies,
            0x03 => Id::Shows,
            0x04 => Id::User,
            0x10 => Id::Movie(b as i64),
            0x11 => Id::Series(b as i64),
            0x12 => Id::Season(b as i64, a as u32),
            0x13 => Id::Episode(b as i64),
            0x14 => Id::File(b as i64),
            0x20 => Id::Person(a as u8, b),
            0x21 => Id::Collection(b as u32),
            0x22 => Id::Genre(b),
            0x23 => Id::Studio(b),
            _ => return None,
        })
    }

    /// The id for a genre or a studio, which have names and no numbers.
    pub fn named(name: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in name.trim().to_lowercase().bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (tag, a, b) = self.parts();
        write!(f, "{tag:02x}{a:014x}{b:016x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_survive_the_round_trip() {
        for id in [Id::Root, Id::Movies, Id::Shows, Id::User, Id::Movie(42), Id::Series(7), Id::Season(7, 3), Id::Episode(9001), Id::File(12), Id::Person(2, 55_123), Id::Collection(10), Id::Genre(Id::named("Drama")), Id::Studio(Id::named("A24"))] {
            let text = id.to_string();
            assert_eq!(text.len(), 32);
            assert_eq!(Id::parse(&text), Some(id));
        }
        // Players send them with dashes and in capitals too.
        let dashed = "10000000-0000-0000-0000-00000000002A";
        assert_eq!(Id::parse(dashed), Some(Id::Movie(42)));
        assert_eq!(Id::parse("not an id"), None);
        assert_eq!(Id::named(" drama "), Id::named("Drama"));
    }
}
