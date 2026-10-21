//! Sentences as points in a space where those that mean much the same sit close together,
//! so that "wake up" can find "don't fall asleep".
//!
//! The model is a small one, all-MiniLM-L6-v2, fetched once from Hugging Face at a fixed
//! revision and checked against the checksums here before it is used. It runs on a Mac's
//! graphics chip where there is one, and on the processor otherwise. Its words are split as
//! its own tokenizer splits them, which is written out here rather than brought in.

use anyhow::{anyhow, bail, Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How many numbers each sentence becomes.
pub const DIMENSIONS: usize = 384;
/// The model, by a name that changes whenever it does: points from one are no use with another.
pub const MODEL: &str = "all-MiniLM-L6-v2@1110a243";
const REVISION: &str = "1110a243fdf4706b3f48f1d95db1a4f5529b4d41";
/// Where it is fetched from, unless told otherwise.
pub const SOURCE: &str = "https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve";
const FILES: [(&str, &str); 3] = [
    ("config.json", "953f9c0d463486b10a6871cc2fd59f223b2c70184f49815e7efbcab5d8908b41"),
    ("vocab.txt", "07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3"),
    ("model.safetensors", "53aa51172d142c89d9012cce15ae4d6cc0ca6895895114379cacb4fab128d9db"),
];
/// The most word pieces it reads of one sentence, as it was trained.
const LONGEST: usize = 128;
const BATCH: usize = 64;
const CLS: u32 = 101;
const SEP: u32 = 102;
const UNK: u32 = 100;

/// Where the model is kept under Spool's data folder.
pub fn model_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join(MODEL.replace('@', "-"))
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Fetch the model unless it is here already. `source` is [`SOURCE`], or for tests a folder
/// holding the model's files.
pub async fn fetch(http: &reqwest::Client, dir: &Path, source: &str) -> Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    for (name, sum) in FILES {
        let path = dir.join(name);
        if path.is_file() {
            continue;
        }
        let bytes = if source.starts_with('/') {
            tokio::fs::read(Path::new(source).join(name)).await.with_context(|| format!("reading the model's {name}"))?.into()
        } else {
            let url = format!("{}/{REVISION}/{name}", source.trim_end_matches('/'));
            http.get(&url).send().await.and_then(|r| r.error_for_status()).map_err(|e| anyhow!("fetching the model: {}", e.without_url()))?.bytes().await?
        };
        if sha256(&bytes) != sum {
            bail!("the model's {name} is not what it should be");
        }
        let part = dir.join(format!("{name}.part"));
        tokio::fs::write(&part, &bytes).await?;
        tokio::fs::rename(&part, &path).await?;
    }
    Ok(())
}

pub struct Embedder {
    model: BertModel,
    vocab: HashMap<String, u32>,
    device: Device,
}

impl Embedder {
    /// Read the model from where [`fetch`] put it. Slow enough to belong off the async threads.
    pub fn load(dir: &Path) -> Result<Embedder> {
        let config: Config = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        let vocab = std::fs::read_to_string(dir.join("vocab.txt"))?.lines().enumerate().map(|(i, l)| (l.to_string(), i as u32)).collect();
        let weights = std::fs::read(dir.join("model.safetensors"))?;
        let load = |device: &Device| -> Result<BertModel> {
            let vb = VarBuilder::from_buffered_safetensors(weights.clone(), DType::F32, device)?;
            Ok(BertModel::load(vb, &config)?)
        };
        // The graphics chip where it works, the processor where it does not.
        #[cfg(target_os = "macos")]
        if let Ok(device) = Device::new_metal(0) {
            if let Ok(model) = load(&device) {
                let e = Embedder { model, vocab, device };
                if e.embed(&["a test"]).is_ok() {
                    return Ok(e);
                }
                return Embedder::on_cpu(load, e.vocab);
            }
        }
        Embedder::on_cpu(load, vocab)
    }

    fn on_cpu(load: impl Fn(&Device) -> Result<BertModel>, vocab: HashMap<String, u32>) -> Result<Embedder> {
        Ok(Embedder { model: load(&Device::Cpu).context("reading the model")?, vocab, device: Device::Cpu })
    }

    /// Each sentence as a point of [`DIMENSIONS`] numbers, a unit's length from the middle, so
    /// that how alike two are is the sum of their numbers multiplied together.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut order: Vec<(usize, Vec<u32>)> = texts.iter().map(|t| tokenize(&self.vocab, t)).enumerate().collect();
        // Sentences of a length together, so little of each batch is padding.
        order.sort_by_key(|(_, ids)| ids.len());
        let mut out = vec![vec![]; texts.len()];
        for batch in order.chunks(BATCH) {
            let n = batch.iter().map(|(_, ids)| ids.len()).max().unwrap_or(2);
            let (mut ids, mut mask) = (Vec::with_capacity(batch.len() * n), Vec::with_capacity(batch.len() * n));
            for (_, t) in batch {
                ids.extend(t.iter().copied().chain(std::iter::repeat_n(0, n - t.len())));
                mask.extend(std::iter::repeat_n(1u32, t.len()).chain(std::iter::repeat_n(0, n - t.len())));
            }
            let ids = Tensor::from_vec(ids, (batch.len(), n), &self.device)?;
            let mask = Tensor::from_vec(mask, (batch.len(), n), &self.device)?;
            let words = self.model.forward(&ids, &ids.zeros_like()?, Some(&mask))?;
            // The mean of the words, padding left out, brought to a unit's length.
            let mask = mask.to_dtype(DType::F32)?.unsqueeze(2)?;
            let mean = words.broadcast_mul(&mask)?.sum(1)?.broadcast_div(&mask.sum(1)?)?;
            let unit = mean.broadcast_div(&mean.sqr()?.sum_keepdim(1)?.sqrt()?)?;
            for ((i, _), v) in batch.iter().zip(unit.to_vec2::<f32>()?) {
                out[*i] = v;
            }
        }
        Ok(out)
    }
}

fn is_punctuation(c: char) -> bool {
    let u = c as u32;
    c.is_ascii_punctuation() || (0x2000..=0x206F).contains(&u) || (0x3000..=0x303F).contains(&u) || (0xFF00..=0xFF0F).contains(&u) || matches!(c, '¡' | '¿' | '«' | '»' | '·' | '§' | '¶')
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0x2A700..=0x2CEAF | 0xF900..=0xFAFF | 0x2F800..=0x2FA1F)
}

/// A sentence as the model's word pieces, as BERT's own uncased tokenizer splits it: by
/// spaces and punctuation, lower case and without accents, then each word into the longest
/// pieces its vocabulary has.
pub fn tokenize(vocab: &HashMap<String, u32>, text: &str) -> Vec<u32> {
    use unicode_normalization::UnicodeNormalization;
    let mut words: Vec<String> = vec![];
    let mut word = String::new();
    for c in text.chars() {
        if c == '\0' || c == '\u{fffd}' || (c.is_control() && !c.is_whitespace()) {
            continue;
        }
        if c.is_whitespace() || is_cjk(c) || is_punctuation(c) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if !c.is_whitespace() {
                words.push(c.to_string());
            }
            continue;
        }
        word.push(c);
    }
    if !word.is_empty() {
        words.push(word);
    }
    let mut ids = vec![CLS];
    for word in words {
        // Lower case and without accents, which can bring out more punctuation to split at.
        let plain: String = word.to_lowercase().nfd().filter(|c| !unicode_normalization::char::is_combining_mark(*c)).collect();
        let mut parts: Vec<String> = vec![];
        let mut part = String::new();
        for c in plain.chars() {
            if is_punctuation(c) {
                if !part.is_empty() {
                    parts.push(std::mem::take(&mut part));
                }
                parts.push(c.to_string());
            } else {
                part.push(c);
            }
        }
        if !part.is_empty() {
            parts.push(part);
        }
        for part in parts {
            ids.extend(pieces(vocab, &part).unwrap_or_else(|| vec![UNK]));
        }
        if ids.len() >= LONGEST - 1 {
            break;
        }
    }
    ids.truncate(LONGEST - 1);
    ids.push(SEP);
    ids
}

/// A word as the longest pieces the vocabulary has, or nothing when it cannot be made of them.
fn pieces(vocab: &HashMap<String, u32>, word: &str) -> Option<Vec<u32>> {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() > 100 {
        return None;
    }
    let (mut start, mut out) = (0, vec![]);
    while start < chars.len() {
        let mut end = chars.len();
        let id = loop {
            if end == start {
                return None;
            }
            let piece: String = chars[start..end].iter().collect();
            let piece = if start > 0 { format!("##{piece}") } else { piece };
            if let Some(&id) = vocab.get(&piece) {
                break id;
            }
            end -= 1;
        };
        out.push(id);
        start = end;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> HashMap<String, u32> {
        // A slice of the model's own vocabulary, by its real numbers.
        [("[PAD]", 0), ("[UNK]", 100), ("[CLS]", 101), ("[SEP]", 102), ("!", 999), (",", 1010), (".", 1012), ("'", 1005), ("t", 1056), ("don", 2123), ("up", 2039), ("wake", 5256), ("cafe", 7668), ("neo", 9253), ("##s", 2015), ("proxy", 24540), ("awake", 8300), ("##ned", 7228)]
            .into_iter()
            .map(|(w, i)| (w.to_string(), i))
            .collect()
    }

    #[test]
    fn words_are_split_as_the_models_own_tokenizer_splits_them() {
        let v = vocab();
        assert_eq!(tokenize(&v, "Wake up, Neo."), vec![101, 5256, 2039, 1010, 9253, 1012, 102]);
        assert_eq!(tokenize(&v, "<don't>"), vec![101, 100, 2123, 1005, 1056, 100, 102]);
        // Accents go and capitals come down; a word is made of the longest pieces there are.
        assert_eq!(tokenize(&v, "CAFÉ  Proxy awakened!"), vec![101, 7668, 24540, 8300, 7228, 999, 102]);
        assert_eq!(tokenize(&v, "\u{1F642}"), vec![101, 100, 102]);
        assert_eq!(tokenize(&v, &"up ".repeat(500)).len(), LONGEST);
    }
}
