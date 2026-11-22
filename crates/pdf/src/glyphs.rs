use super::paint::append_path;
use super::{FaceRef, GlyphId, HashMap, Path, PathBuilder};

/// A shaped glyph id as krilla takes it. Shaping gives 32-bit ids (a format-12 cmap can map to
/// any), but krilla's font subsetting takes 16-bit ones and panics on a larger id (#3674): one
/// that doesn't fit is written as .notdef (glyph 0), keeping its place. Its own character doesn't
/// extract, since krilla maps no text to .notdef.
pub(crate) fn glyph_id(g: u32) -> GlyphId {
    GlyphId::new(u16::try_from(g).map_or(0, u32::from))
}

/// The outlines of `glyphs` (glyph id, pen position) at `size`, as one path in page coordinates.
pub(crate) fn glyph_outlines(face: &FaceRef, size: f32, glyphs: &[(u32, f32, f32)]) -> Option<Path> {
    let db = wordcraft_fonts::FontDb::global();
    let scale = f64::from(size) / face.upem.max(1.0);
    let mut pb = PathBuilder::new();
    for &(g, x, y) in glyphs {
        // Outlines are in font units, y-down: scale to the size, move to the pen position.
        let mut outline = (*db.outline(face, g)).clone();
        outline.apply_affine(kurbo::Affine::translate((f64::from(x), f64::from(y))) * kurbo::Affine::scale(scale));
        append_path(&mut pb, &outline);
    }
    pb.finish()
}

/// Whether a run's glyphs already run left to right, as PDF text extractors (PDFium in Chrome and
/// Edge, pdf.js, poppler) take them to: they read a run's glyphs as written left to right and turn
/// right-to-left words back into reading order themselves. Layout gives right-to-left text in
/// reading order.
pub(crate) fn left_to_right(glyphs: &[(u32, f32, f32)]) -> bool {
    glyphs.windows(2).all(|w| matches!(w, [a, b] if a.1 <= b.1))
}

/// Per-glyph text ranges for clusters of several glyphs (a letter and its vowel marks) when each
/// glyph is one of the cluster's characters (through the font's character map, one glyph at most
/// left to take the character left over), so a viewer
/// reversing a right-to-left word puts every mark back after its letter. Other clusters, such
/// as a ligature drawn as two glyphs, keep their shared range.
pub(crate) fn split_clusters(
    text: &str,
    glyphs: &[(u32, f32, f32)],
    ranges: &[std::ops::Range<usize>],
    rev: &HashMap<u32, char>,
) -> Vec<std::ops::Range<usize>> {
    let mut out = ranges.to_vec();
    let mut i = 0;
    while i < ranges.len() {
        let Some(r) = ranges.get(i) else { break };
        let n = ranges.get(i..).map_or(1, |rest| rest.iter().take_while(|x| *x == r).count()).max(1);
        let chars: Vec<(usize, char)> = text.get(r.clone()).map(|t| t.char_indices().map(|(k, c)| (r.start + k, c)).collect()).unwrap_or_default();
        if n > 1 && chars.len() == n {
            // Glyphs the character map knows take their characters; one left over (a letter's
            // contextual form, which the map doesn't list) takes the one character left.
            let mut used = vec![false; n];
            let mut of: Vec<Option<usize>> = (i..i + n)
                .map(|g| {
                    let c = rev.get(&glyphs.get(g)?.0)?;
                    let k = chars.iter().enumerate().position(|(k, (_, ch))| ch == c && !used.get(k).copied().unwrap_or(true))?;
                    *used.get_mut(k)? = true;
                    Some(k)
                })
                .collect();
            let free: Vec<usize> = (0..n).filter(|k| !used.get(*k).copied().unwrap_or(true)).collect();
            if let ([k], [slot]) = (free.as_slice(), of.iter_mut().filter(|o| o.is_none()).collect::<Vec<_>>().as_mut_slice()) {
                **slot = Some(*k);
            }
            let parts: Option<Vec<std::ops::Range<usize>>> = of
                .iter()
                .map(|k| {
                    let (a, ch) = chars.get((*k)?)?;
                    Some(*a..a + ch.len_utf8())
                })
                .collect();
            if let (Some(parts), Some(dst)) = (parts, out.get_mut(i..i + n)) {
                dst.clone_from_slice(&parts);
            }
        }
        i += n;
    }
    out
}

/// Indices of a run's glyphs in visual order, left to right: its clusters (consecutive glyphs
/// sharing a range in `clusters`, such as a letter and its marks, or a lam-alef ligature) sorted
/// by their leftmost glyph and kept together. Inside a cluster, glyphs go in reverse order of
/// their own text (`parts`) when it is right to left, so a viewer reversing the word reads the
/// letter before its marks.
pub(crate) fn visual_order(
    text: &str,
    glyphs: &[(u32, f32, f32)],
    clusters: &[std::ops::Range<usize>],
    parts: &[std::ops::Range<usize>],
) -> Vec<usize> {
    let mut groups: Vec<(f32, Vec<usize>)> = Vec::new();
    for (i, (_, x, _)) in glyphs.iter().enumerate() {
        match groups.last_mut() {
            Some((left, idx)) if i > 0 && clusters.get(i).is_some() && clusters.get(i) == clusters.get(i - 1) => {
                *left = left.min(*x);
                idx.push(i);
            }
            _ => groups.push((*x, vec![i])),
        }
    }
    // Clusters at the same x go in text order, reversed when right to left: some fonts draw
    // lam-alef as a lam and a zero-width alef at one x, and a right-to-left run whose ranges
    // don't decrease loses its reading order (#2310). Stable for glyphs sharing their text.
    let tie = |idx: &[usize]| -> i64 {
        let Some(r) = idx.first().and_then(|&g| clusters.get(g)) else { return 0 };
        let start = i64::try_from(r.start).unwrap_or(i64::MAX);
        if text.get(r.clone()).is_some_and(|t| t.chars().any(wordcraft_fonts::is_rtl)) { -start } else { start }
    };
    groups.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| tie(&a.1).cmp(&tie(&b.1))));
    groups
        .into_iter()
        .flat_map(|(_, mut idx)| {
            let rtl = idx.first().and_then(|&g| text.get(clusters.get(g)?.clone())).is_some_and(|t| t.chars().any(wordcraft_fonts::is_rtl));
            if rtl {
                idx.sort_by_key(|&g| std::cmp::Reverse(parts.get(g).map_or(0, |r| r.start)));
            }
            idx
        })
        .collect()
}

/// The y most of a run's glyphs share: its baseline, which raised or lowered marks leave.
pub(crate) fn baseline(glyphs: &[(u32, f32, f32)]) -> Option<f32> {
    let &(_, _, first) = glyphs.first()?;
    if glyphs.iter().all(|g| g.2.to_bits() == first.to_bits()) {
        return Some(first);
    }
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for (_, _, y) in glyphs {
        *counts.entry(y.to_bits()).or_insert(0) += 1;
    }
    // Ties go to the lower y bits, so the result doesn't depend on hash order.
    counts.into_iter().max_by_key(|&(bits, n)| (n, std::cmp::Reverse(bits))).map(|(bits, _)| f32::from_bits(bits))
}

/// Where each glyph's text ends: the cluster (byte offset) of the first later glyph whose cluster
/// is past the glyph's own, else `len`. One backward pass keeping the later clusters that can
/// still be an answer, so a long run of glyphs sharing a cluster (combining marks) stays linear.
pub(crate) fn cluster_ends(clusters: &[usize], len: usize) -> Vec<usize> {
    let mut ends = vec![len; clusters.len()];
    // Later clusters, nearest last, each greater than every one nearer.
    let mut later: Vec<usize> = Vec::new();
    for (end, &c) in ends.iter_mut().zip(clusters).rev() {
        while later.last().is_some_and(|&n| n <= c) {
            later.pop();
        }
        *end = later.last().copied().unwrap_or(len);
        later.push(c);
    }
    ends
}

/// For a left-to-right run with shaped clusters (a consonant with a vowel sign or a mark, as in
/// Devanagari, Bengali, Tamil …): each word's glyphs share the word's whole text range, so krilla
/// writes the word as one ActualText span. Text extractors place a cluster's text by its glyphs,
/// and a cluster ending in a mark drawn back over its consonant («के») looks to them like it ends
/// early: they read the gap to the next cluster as a space («एप्लिके शन»). As one span the word
/// reads whole. Runs without such clusters keep their ranges (and their per-glyph text), and so
/// do words with no character of such a script: a Latin «å» drawn as «a» plus a ring mark (as
/// Calibri and Cambria do) must not give the «a» glyph the whole word as its text, or krilla's
/// ToUnicode map sends every later «a» to that word.
pub(crate) fn word_ranges(
    text: &str,
    ranges: &[std::ops::Range<usize>],
    glyphs: &[(u32, f32, f32)],
    is_mark: impl Fn(u32) -> bool,
) -> Vec<std::ops::Range<usize>> {
    let shaped = glyphs.iter().any(|g| is_mark(g.0)) || ranges.windows(2).any(|w| matches!(w, [a, b] if a == b));
    if !shaped {
        return ranges.to_vec();
    }
    // Word boundaries: the byte ranges between whitespace.
    let words: Vec<std::ops::Range<usize>> = {
        let mut out = Vec::new();
        let mut start = None;
        for (i, c) in text.char_indices() {
            match (c.is_whitespace(), start) {
                (true, Some(s)) => {
                    out.push(s..i);
                    start = None;
                }
                (false, None) => start = Some(i),
                _ => {}
            }
        }
        if let Some(s) = start {
            out.push(s..text.len());
        }
        out
    };
    let complex = |w: &std::ops::Range<usize>| text.get(w.clone()).is_some_and(|t| t.chars().any(wordcraft_fonts::needs_layout_tables));
    let word_of = |r: &std::ops::Range<usize>| words.iter().find(|w| w.start <= r.start && r.end <= w.end && complex(w)).cloned();
    let mut out: Vec<std::ops::Range<usize>> = ranges.iter().map(|r| word_of(r).unwrap_or_else(|| r.clone())).collect();
    // A word's glyphs must follow each other to share a span; when they don't (a word broken by
    // something placed in between), keep the original ranges.
    for (i, r) in out.clone().iter().enumerate() {
        if let Some(j) = out.iter().rposition(|x| x == r) {
            if out.get(i..=j).is_some_and(|between| between.iter().any(|x| x != r)) {
                return ranges.to_vec();
            }
        }
    }
    out.shrink_to_fit();
    out
}

/// Clusters of one letter drawn as a base glyph and zero-width marks (Cambria and Calibri draw
/// «å» as «a» plus a ring) become the font's own glyph for that letter, at the base glyph's
/// position, when the font has one (`glyph_of`). The base glyph's ToUnicode entry is the plain
/// letter, so the cluster's «å» could only go in ActualText, which many extractors and applicant
/// tracking systems ignore («nå» read as «na»). Letters of scripts that need shaping
/// (`needs_layout_tables`) keep their clusters. `None` when nothing changes.
pub(crate) fn precompose(
    glyphs: &[(u32, f32, f32)],
    text: &str,
    ranges: &[std::ops::Range<usize>],
    is_mark: impl Fn(u32) -> bool,
    glyph_of: impl Fn(char) -> Option<u32>,
) -> Option<(Vec<(u32, f32, f32)>, Vec<std::ops::Range<usize>>)> {
    let mut out_g = Vec::with_capacity(glyphs.len());
    let mut out_r = Vec::with_capacity(ranges.len());
    let mut changed = false;
    let mut i = 0;
    while let (Some(&g), Some(r)) = (glyphs.get(i), ranges.get(i)) {
        // The cluster: this glyph and the marks after it with the same text.
        let mut j = i + 1;
        while glyphs.get(j).is_some_and(|m| is_mark(m.0)) && ranges.get(j) == Some(r) {
            j += 1;
        }
        let mut chars = text.get(r.clone()).unwrap_or("").chars();
        let letter = match (chars.next(), chars.next()) {
            (Some(c), None) if j > i + 1 && !is_mark(g.0) && !wordcraft_fonts::needs_layout_tables(c) => Some(c),
            _ => None,
        };
        match letter.and_then(&glyph_of).filter(|&pre| !is_mark(pre)) {
            Some(pre) => {
                out_g.push((pre, g.1, g.2));
                out_r.push(r.clone());
                changed = true;
            }
            None => {
                out_g.extend_from_slice(glyphs.get(i..j).unwrap_or_default());
                out_r.extend_from_slice(ranges.get(i..j).unwrap_or_default());
            }
        }
        i = j;
    }
    (changed && out_r.len() == out_g.len()).then_some((out_g, out_r))
}

/// Right-to-left clusters drawn as one glyph that takes room plus marks of no text of their own
/// (Noto Sans Arabic draws the dots of «ن», «ي», «ة» as separate glyphs): the glyph that takes room
/// keeps the cluster's text, and the marks get none. A cluster that keeps a shared range is written
/// as an ActualText span, which PDF readers place oddly in right-to-left text: the word reads in
/// pieces («مهن دسة»). Clusters with more than one glyph that takes room keep their range.
///
/// A letter with harakat drawn as glyphs of their own («قً») keeps them in its text. pdftotext
/// reads that span reversed («فريًقا»); Chrome-like viewers read it right. Text without harakat,
/// as in most CVs and documents, is not affected.
pub(crate) fn bare_marks(
    ranges: &[std::ops::Range<usize>],
    glyphs: &[(u32, f32, f32)],
    is_mark: impl Fn(u32) -> bool,
) -> Vec<std::ops::Range<usize>> {
    let mut out = ranges.to_vec();
    let mut i = 0;
    while i < ranges.len() {
        let Some(r) = ranges.get(i) else { break };
        let n = ranges.get(i..).map_or(1, |rest| rest.iter().take_while(|x| *x == r).count()).max(1);
        if n > 1 {
            let solid: Vec<usize> = (i..i + n).filter(|&g| glyphs.get(g).is_some_and(|gl| !is_mark(gl.0))).collect();
            if let [base] = solid.as_slice() {
                for g in i..i + n {
                    if g != *base {
                        if let Some(slot) = out.get_mut(g) {
                            *slot = r.end..r.end;
                        }
                    }
                }
            }
        }
        i += n;
    }
    out
}
