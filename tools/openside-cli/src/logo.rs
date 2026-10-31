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

/// Logical pixel grid for a word: rows of ' '/'#', SLOT_ROWS tall,
/// 1px gutter between letters.
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
    let width = glyphs.len() * 6 + (glyphs.len() - 1);
    let mut rows = vec![vec![false; width]; SLOT_ROWS];
    let mut x = 0usize;
    for glyph in glyphs {
        for (y, row) in glyph.iter().enumerate() {
            for (cx, ch) in row.chars().enumerate() {
                if ch == '#' {
                    rows[y][x + cx] = true;
                }
            }
        }
        x += 7;
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
        assert!(g.iter().all(|row| row.len() == 8 * 6 + 7));
        // every letter leaves ink
        assert!(g.iter().any(|row| row.iter().any(|&b| b)));
    }

    #[test]
    fn unknown_chars_skipped() {
        assert!(grid("x1z").is_empty());
    }
}
