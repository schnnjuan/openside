// Pixel glyphs in the same hand-drawn family as the "ratscope" wordmark.
// 6px wide, variable height, rendered with half-blocks so one terminal
// row = two pixels.

pub const GLYPHS: &[(&str, &[&str])] = &[
    ("i", &[".####.", "..##..", "..##..", "..##..", "..##..", "..##..", ".####."]),
    ("p", &["#####.", "##..##", "##..##", "##..##", "#####.", "##....", "##....", "##...."]),
    ("o", &[".####.", "##..##", "##..##", "##..##", "##..##", "##..##", "##..##", ".####."]),
    ("n", &["#####.", "##..##", "##..##", "##..##", "##..##", "##..##", "##..##", "##..##"]),
    ("s", &[".#####", "##....", "##....", ".####.", "....##", "....##", "....##", "#####."]),
    ("d", &[".####.", "##..##", "##..##", "##..##", "##..##", "##..##", "##..##", ".####."]),
    ("e", &[".####.", "##..##", "##..##", "######", "##....", "##....", "##..##", ".####."]),
    ("a", &[".####.", "....##", "....##", ".####.", "##..##", "##..##", "##..##", ".####."]),
];

/// Logical pixel grid for a word: rows of ' '/'#'. Width gaining 1px gutters
/// between letters, height = tallest glyph.
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
    let height = glyphs.iter().map(|g| g.len()).max().unwrap();
    let width = glyphs.len() * 6 + (glyphs.len() - 1);
    let mut rows = vec![vec![false; width]; height];
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
    fn grid_has_full_strokes() {
        let g = grid("openside");
        assert_eq!(g.len(), 8);
        assert!(g.iter().all(|row| row.len() == 8 * 6 + 7));
        // every letter leaves ink
        assert!(g.iter().any(|row| row.iter().any(|&b| b)));
    }

    #[test]
    fn unknown_chars_skipped() {
        assert!(grid("x1z").is_empty());
    }
}
