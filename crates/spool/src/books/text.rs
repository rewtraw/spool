//! Book titles and author names put into one form, so that the same book written two ways
//! is found as one, and how alike two of them are.

/// Words too common to tell books apart.
pub const STOP: &[&str] = &["the", "a", "an", "of", "and", "in", "on", "to", "for", "de", "la", "le", "el"];

/// Lower-case ASCII words, accents and punctuation gone.
pub fn words(s: &str) -> Vec<String> {
    deunicode::deunicode(s).to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// A title without its subtitle, series mark or edition note: "Dune: Deluxe Edition" and
/// "Dune (Dune Chronicles #1)" are both "dune".
pub fn core_title(t: &str) -> String {
    let cut = t.find([':', '(', '[']).or_else(|| t.find(" - ")).or_else(|| t.find(" / ")).unwrap_or(t.len());
    let core = words(&t[..cut]);
    if core.is_empty() { words(t) } else { core }.join(" ")
}

/// An author in one form: "Le Guin, Ursula K.", "Ursula K. Le Guin" and "Ursula K. Le Guin
/// [Le Guin, Ursula K.]" are all the sorted words of the name, initials dropped. Only the
/// first of several authors counts.
pub fn author_key(a: &str) -> String {
    let mut plain = String::new();
    let mut depth = 0;
    for c in a.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = (depth - 1).max(0),
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let first = plain.split([';', '&']).next().unwrap_or("");
    let first = first.split(" and ").next().unwrap_or(first);
    // "Last, First" is one author; "A, B, C" is several, of which the first counts.
    let first = if first.matches(',').count() == 1 { first.to_string() } else { first.split(',').next().unwrap_or("").to_string() };
    let mut w: Vec<String> = words(&first).into_iter().filter(|w| w.len() > 1).collect();
    w.sort();
    w.dedup();
    w.join(" ")
}

/// The surname, as far as it can be told: the last word of "First Last", the first of
/// "Last, First".
pub fn surname(a: &str) -> String {
    let first = a.split([';', '&', '[', '(']).next().unwrap_or("");
    match first.split_once(',') {
        Some((last, _)) => words(last).last().cloned().unwrap_or_default(),
        None => words(first).last().cloned().unwrap_or_default(),
    }
}

/// Words that put a number after them: "Volume Eight", "Book II".
const NUMBERED: [&str; 6] = ["vol", "volume", "book", "part", "tome", "no"];
const SPELLED: [&str; 20] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen", "twenty",
];
const ROMAN: [&str; 20] = ["i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x", "xi", "xii", "xiii", "xiv", "xv", "xvi", "xvii", "xviii", "xix", "xx"];

/// A word's number: figures anywhere, and a number written out or in Roman numerals after a
/// word such as "volume", so "Volume Eight" is 8 but "The One Thing" has none.
fn number(w: &str, after: Option<&str>) -> Option<u32> {
    if let Ok(n) = w.parse::<u32>() {
        return Some(n);
    }
    if !after.is_some_and(|a| NUMBERED.contains(&a)) {
        return None;
    }
    SPELLED.iter().chain(ROMAN.iter()).position(|s| *s == w).map(|i| i as u32 % 20 + 1)
}

/// The numbers in a title, such as a volume or a part: "Goblin Slayer, Vol. 4" has 4, and
/// "The Zohar: Pritzker Edition, Volume Eight" has 8.
pub fn numbers(t: &str) -> Vec<u32> {
    let ws = words(t);
    (0..ws.len()).filter_map(|i| number(&ws[i], i.checked_sub(1).map(|j| ws[j].as_str()))).filter(|n| *n < 1000).collect()
}

/// How well the words of `a` are found in `b`: each word against its best match by
/// Jaro-Winkler, weighted by its length.
pub fn cover(a: &str, b: &str) -> f64 {
    let aw: Vec<&str> = a.split(' ').filter(|w| !w.is_empty()).collect();
    let bw: Vec<&str> = b.split(' ').filter(|w| !w.is_empty()).collect();
    if aw.is_empty() || bw.is_empty() {
        return 0.0;
    }
    let (mut got, mut all) = (0.0, 0.0);
    for w in &aw {
        let best = bw.iter().map(|x| strsim::jaro_winkler(w, x)).fold(0.0, f64::max);
        got += best * w.len() as f64;
        all += w.len() as f64;
    }
    got / all
}

/// Words a subtitle may have without naming another book: "Dune: A Novel" and "Dune:
/// Deluxe Edition" are Dune.
const PLAIN_SUBTITLE: &[&str] = &["a", "an", "the", "novel", "edition", "deluxe", "anniversary", "illustrated", "unabridged", "classic", "classics", "revised", "expanded", "annotated", "complete", "special", "collectors", "th", "st", "nd", "rd", "of", "and", "book", "story", "stories", "memoir"];

/// What a title says after its core, less the words that say nothing: "insurrection" for
/// "Viridian Gate Online: Insurrection", nothing for "Dune: Deluxe Edition".
fn subtitle(t: &str) -> String {
    let core = core_title(t);
    let all = words(t);
    let skip = core.split(' ').filter(|w| !w.is_empty()).count();
    // Numbers are weighed apart, by `numbers`, and so are left out, with the words that
    // number them.
    let rest: Vec<String> = all.into_iter().skip(skip).collect();
    (0..rest.len())
        .filter(|&i| {
            let w = rest[i].as_str();
            let numbers_next = rest.get(i + 1).is_some_and(|n| number(n, Some(w)).is_some());
            !PLAIN_SUBTITLE.contains(&w) && !numbers_next && number(w, i.checked_sub(1).map(|j| rest[j].as_str())).is_none()
        })
        .map(|i| rest[i].clone())
        .collect::<Vec<_>>()
        .join(" ")
}

/// How alike two titles are, both ways round, 0 to 1. Numbers that disagree, such as
/// volume 4 and volume 11, make them different books, and so do subtitles that do, such as
/// "Viridian Gate Online: Insurrection" and "Viridian Gate Online: Vindication".
pub fn title_alike(a: &str, b: &str) -> f64 {
    let (ca, cb) = (core_title(a), core_title(b));
    let mut s = cover(&ca, &cb) * 0.6 + cover(&cb, &ca) * 0.4;
    // A file's title sometimes leads with its author, "McDermott, Andy - The Hunt for
    // Atlantis", or keeps a subtitle the other lacks: the words wanted, found in it whole.
    s = s.max(cover(&ca, &words(b).join(" ")) * 0.95);
    let (na, nb) = (numbers(a), numbers(b));
    if !na.is_empty() && !nb.is_empty() && na.iter().all(|n| !nb.contains(n)) {
        s -= 0.35;
    }
    // The same volume settles it, though one names the volume's contents and the other not.
    let same_volume = !na.is_empty() && na == nb;
    let (sa, sb) = (subtitle(a), subtitle(b));
    if !same_volume && !sa.is_empty() && !sb.is_empty() && cover(&sa, &sb).min(cover(&sb, &sa)) < 0.75 {
        s -= 0.35;
    }
    s.max(0.0)
}

/// How alike two authors are, 0 to 1.
pub fn author_alike(a: &str, b: &str) -> f64 {
    let (ka, kb) = (author_key(a), author_key(b));
    if ka.is_empty() || kb.is_empty() {
        return 0.0;
    }
    cover(&ka, &kb).max(cover(&kb, &ka) * 0.9)
}

/// An ISBN as thirteen digits, from ten or thirteen with or without dashes; None for
/// anything else.
pub fn isbn13(s: &str) -> Option<String> {
    let d: String = s.chars().filter(|c| c.is_ascii_digit() || *c == 'X' || *c == 'x').collect();
    match d.len() {
        13 if d.chars().all(|c| c.is_ascii_digit()) => Some(d),
        10 => {
            let body = format!("978{}", &d[..9]);
            let sum: u32 = body.chars().enumerate().map(|(i, c)| c.to_digit(10).unwrap_or(0) * if i % 2 == 0 { 1 } else { 3 }).sum();
            Some(format!("{body}{}", (10 - sum % 10) % 10))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_put_in_one_form() {
        assert_eq!(core_title("Dune: Deluxe Edition"), "dune");
        assert_eq!(core_title("Soldiers Live (The Chronicles of the Black Company, #9)"), "soldiers live");
        assert_eq!(author_key("Le Guin, Ursula K."), author_key("Ursula K. Le Guin"));
        assert_eq!(author_key("Agatha Christie [Christie, Agatha]"), author_key("Christie, Agatha"));
        assert_eq!(author_key("N.H. Paxton & James Hunter"), "paxton");
        assert_eq!(surname("Le Guin, Ursula K."), "guin");
        assert_eq!(surname("Kumo Kagyu"), "kagyu");
        assert_eq!(isbn13("0-441-47812-3").as_deref(), Some("9780441478125"));
        assert_eq!(isbn13("978-0-441-47812-5").as_deref(), Some("9780441478125"));
    }

    #[test]
    fn another_volume_is_another_book() {
        assert!(title_alike("Goblin Slayer, Vol. 4", "Goblin Slayer, Vol. 4") > 0.95);
        assert!(title_alike("Goblin Slayer, Vol. 4", "Goblin Slayer, Vol. 11") < 0.7);
        assert!(title_alike("The Left Hand of Darkness", "Left Hand of Darknes") > 0.9);
        assert!(title_alike("The Hunt for Atlantis", "McDermott, Andy - The Hunt for Atlantis") > 0.9);
        assert!(title_alike("Viridian Gate Online: Insurrection", "Viridian Gate Online: Vindication") < 0.7);
        assert!(title_alike("Viridian Gate Online: Insurrection", "Viridian Gate Online") > 0.9);
        assert!(title_alike("Dune: A Novel", "Dune: Deluxe Edition") > 0.9);
    }

    #[test]
    fn a_volume_written_out_is_a_number() {
        assert_eq!(numbers("The Zohar: Pritzker Edition, Volume Nine"), vec![9]);
        assert_eq!(numbers("Hermetica, Book II"), vec![2]);
        assert!(numbers("The One Thing").is_empty());
        let nine = "The Zohar: Pritzker Edition, Volume Nine";
        let right = title_alike(nine, "The Zohar (Zohar: The Pritzker Editions) Vol 9: Numbers-Deuteronomy");
        let wrong = title_alike(nine, "The Zohar (The Zohar: Pritzker Edition) Vol 11: Midrash ha-Ne'lam and Zoharic Compositions");
        assert!(right > 0.85 && wrong < 0.6, "{right} {wrong}");
        assert!(title_alike("The Zohar: Pritzker Edition, Volume Seven", "The Zohar (The Zohar: Pritzker Edition) Vol 7: Leviticus") > 0.85);
    }
}
