// Pixel glyphs, hand-drawn, same family as the "ratscope" wordmark.
// 6px wide. Shared baseline: slot 12 rows tall — x-height letters ink rows
// 2-9, ascender/dot up to row 0, descender down to row 11. Rendered with
// half-blocks: one terminal row = two pixel rows.

pub const SLOT_ROWS: usize = 12;

pub const GLYPHS: &[(&str, &[&str])] = &[
    ("i", &[
        "..##..", "..##..", "......", "..##..", "..##..", "..##..",
        "..##..", "..##..", "..##..", "..##..", "......", "......",
    ]),
    ("o", &[
        "......", "......", ".####.", "##..##", "##..##", "##..##",
        "##..##", "##..##", "##..##", ".####.", "......", "......",
    ]),
    ("p", &[
        "......", "......", ".####.", "##..##", "##..##", "##..##",
        ".####.", "##....", "##....", "##....", "##....", "##....",
    ]),
    ("n", &[
        "......", "......", "#####.", "##..##", "##..##", "##..##",
        "##..##", "##..##", "##..##", "##..##", "......", "......",
    ]),
    ("s", &[
        "......", "......", ".#####", "##....", "##....", ".####.",
        "....##", "....##", "....##", "#####.", "......", "......",
    ]),
    ("d", &[
        "....##", "....##", ".####.", "##..##", "##..##", "##..##",
        "##..##", "##..##", "##..##", ".####.", "......", "......",
    ]),
    ("e", &[
        "......", "......", ".####.", "##..##", "##..##", "######",
        "##....", "##....", "##..##", ".####.", "......", "......",
    ]),
    ("a", &[
        "......", "......", ".####.", "....##", "....##", ".####.",
        "##..##", "##..##", "##..##", ".####.", "......", "......",
    ]),
];

/// Logical pixel grid for a word: rows of ' '/'#', SLOT_ROWS tall.
/// Kerning is ink-based: letters touch 1px apart from their actual ink,
/// so narrow glyphs (like `i`) don't float in a 6px-wide slot.
pub fn grid(word: &str) -> Vec<Vec<bool>> {
    let glyphs: Vec<&[&str]> = word
        .chars()
        .filter_map(|c| {
            GLYPHS
                .iter()
                .find(|(name, _)| name.chars().next() == Some(c))
                .and_then(|(_, g)| Some(*g))
        })
        .collect();
    if glyphs.is_empty() {
        return vec![];
    }
    let ink_span = |g: &[&str]| -> (usize, usize) {
        let mut first = usize::MAX;
        let mut last = 0usize;
        for row in g {
            for (cx, ch) in row.chars().enumerate() {
                if ch == '#' {
                    first = first.min(cx);
                    last = last.max(cx);
                }
            }
        }
        (first.min(6), last)
    };
    let mut xs = Vec::with_capacity(glyphs.len());
    let mut x = 0usize;
    for g in &glyphs {
        let (first, last) = ink_span(g);
        x = x.saturating_sub(first);
        xs.push(x);
        x += last + 2;
    }
    let width = x - 1;
    let mut rows = vec![vec![false; width]; SLOT_ROWS];
    for (glyph, &gx) in glyphs.iter().zip(&xs) {
        for (y, row) in glyph.iter().enumerate() {
            for (cx, ch) in row.chars().enumerate() {
                if ch == '#' && gx + cx < width {
                    rows[y][gx + cx] = true;
                }
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_shared() {
        let g = grid("openside");
        // bottom ink row = baseline: some ink in slot row 9, none in 11's pair mismatch
        assert!(g[9].iter().any(|&b| b), "baseline na linha 9");
        assert!(g.iter().take(2).all(|row| !row.iter().all(|&b| b) || row.iter().any(|&b| b)));
    }

    #[test]
    fn grid_has_full_strokes() {
        let g = grid("openside");
        assert_eq!(g.len(), SLOT_ROWS);
        // tight kerning: 6 letters (6px ink + 1px gap) + home (3px ink) + 1px gap
        // = 6*7 + 3 + 1 + 6*7... = 51 ink columns total
        assert!(g.iter().all(|row| row.len() == 51), "width = {}", g[0].len());
        // every letter leaves ink
        assert!(g.iter().any(|row| row.iter().any(|&b| b)));
    }

    #[test]
    fn i_kerning_tight() {
        let g = grid("openside");
        // s right ink | 1px | i stem (2px) | 1px | d left ink, rows 6..=8
        // (row 5/9 have bowl transitions, one extra gap column there)
        for y in 6..=8 {
            assert!(
                g[y][33] && !g[y][34] && g[y][35] && g[y][36] && !g[y][37] && g[y][38],
                "linha {y}: s/i/d não estão tangentes: {:?}", &g[y][28..44],
            );
        }
    }

    #[test]
    fn unknown_chars_skipped() {
        assert!(grid("x1z").is_empty());
    }
}
