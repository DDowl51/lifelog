//! `vectors` 的纯逻辑：把文档切成块、向量和字节之间的转换、相似度。

/// 把一篇文档切成不超过 `max_chars` 个字的块：按段落（空行）切，短的段落合在一起，
/// 太长的段落按句号、换行再切，还太长就硬切。标题放在第一块前面。
pub fn chunks(title: &str, body: &str, max_chars: usize) -> Vec<String> {
    let max = max_chars.max(20);
    let mut pieces: Vec<String> = Vec::new();
    for paragraph in body.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if paragraph.chars().count() <= max {
            pieces.push(paragraph.to_owned());
            continue;
        }
        let mut current = String::new();
        for sentence in paragraph.split_inclusive(['。', '！', '？', '\n', '.']) {
            if current.chars().count() + sentence.chars().count() > max && !current.is_empty() {
                pieces.push(std::mem::take(&mut current).trim().to_owned());
            }
            current.push_str(sentence);
            while current.chars().count() > max {
                let head: String = current.chars().take(max).collect();
                let rest: String = current.chars().skip(max).collect();
                pieces.push(head.trim().to_owned());
                current = rest;
            }
        }
        if !current.trim().is_empty() {
            pieces.push(current.trim().to_owned());
        }
    }
    // 短的合起来，少算几次向量
    let mut out: Vec<String> = Vec::new();
    for piece in pieces {
        match out.last_mut() {
            Some(last) if last.chars().count() + piece.chars().count() < max => {
                last.push('\n');
                last.push_str(&piece);
            }
            _ => out.push(piece),
        }
    }
    let title = title.trim();
    if !title.is_empty() {
        match out.first_mut() {
            Some(first) => *first = format!("{title}\n{first}"),
            None => out.push(title.to_owned()),
        }
    }
    out
}

/// 向量存成小端序的 f32 字节。
pub fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

/// 余弦相似度。向量已经归一化时就是点积；这里还是除一下，防着没归一化的。
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum();
    let norm = |v: &[f32]| v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    let (na, nb) = (norm(a), norm(b));
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// 每篇文档只留最相近的一块，按相似度降序，去掉低于 `min_score` 的，最多 `limit` 篇。
/// `scored` 是（文档键，块原文，相似度）。
pub fn best_per_document(
    scored: Vec<(String, String, f64)>,
    min_score: f64,
    limit: usize,
) -> Vec<(String, String, f64)> {
    let mut best: Vec<(String, String, f64)> = Vec::new();
    for (key, chunk, score) in scored {
        if score < min_score {
            continue;
        }
        match best.iter_mut().find(|(k, _, _)| *k == key) {
            Some(entry) if entry.2 < score => *entry = (key, chunk, score),
            Some(_) => {}
            None => best.push((key, chunk, score)),
        }
    }
    best.sort_by(|a, b| b.2.total_cmp(&a.2));
    best.truncate(limit);
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_keeps_paragraphs_and_respects_the_limit() {
        let body = "## 概览\n短的一段。\n\n## 按项目\n第二段也短。\n\n".to_owned()
            + &"很长的一句话。".repeat(20);
        let parts = chunks("2026-11-12 周四", &body, 50);
        assert!(
            parts[0].starts_with("2026-11-12 周四\n## 概览"),
            "{parts:?}"
        );
        assert!(parts[0].contains("第二段也短"), "短的合在一起：{parts:?}");
        assert!(
            parts.iter().all(|p| p.chars().count() <= 50 + 20),
            "{parts:?}"
        );
        assert!(parts.len() >= 3, "长的被切开：{parts:?}");
        assert_eq!(chunks("只有标题", "", 50), ["只有标题"]);
        assert!(chunks("", "  ", 50).is_empty());
        let hard = chunks("", &"字".repeat(130), 50);
        assert_eq!(
            hard.iter().map(|c| c.chars().count()).collect::<Vec<_>>(),
            [50, 50, 30],
            "没有标点就硬切"
        );
    }

    #[test]
    fn bytes_round_trip_and_similarity() {
        let v = vec![0.6f32, -0.8, 0.0];
        assert_eq!(from_bytes(&to_bytes(&v)), v);
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-9);
        assert!((cosine(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-9);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0, "维度不同（换了模型）不比");
    }

    #[test]
    fn best_chunk_per_document() {
        let s = |k: &str, c: &str, x: f64| (k.to_owned(), c.to_owned(), x);
        let out = best_per_document(
            vec![
                s("a", "a1", 0.7),
                s("b", "b1", 0.9),
                s("a", "a2", 0.95),
                s("c", "c1", 0.2),
            ],
            0.5,
            10,
        );
        assert_eq!(out, [s("a", "a2", 0.95), s("b", "b1", 0.9)]);
        assert_eq!(best_per_document(out, 0.0, 1).len(), 1);
    }
}
