//! Macro-block clustering for full-mode translation (R4).
//! Pure over OCR lines, so the heuristics are unit-testable here:
//! lines sort by (y,x); a new block starts on a big vertical gap or lost
//! horizontal overlap; empty-text lines are natural boundaries. Inside a
//! block, hard line breaks emit '\n', soft wraps join (nothing between
//! adjacent CJK, one space otherwise).
use crate::ocr::OcrLine;

/// One translatable paragraph: union bounds, paint font size, and the joined
/// source text (hard breaks restored as '\n', soft wraps joined inline).
pub struct Block {
    pub rect: (i32, i32, i32, i32),
    pub font_px: u32,
    pub text: String,
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' | '\u{FF66}'..='\u{FF9F}'
        | '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}'
        | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{2FFFF}')
}

/// Bullet / numbered-list marker at a line start (• - 1. 1、 ① …).
fn is_list_start(t: &str) -> bool {
    let t = t.trim_start();
    let mut chars = t.chars();
    match chars.next() {
        Some(c) if matches!(c, '•' | '‣' | '·' | '-' | '–' | '—' | '*' | '>' | '#') => true,
        Some(c) if c.is_ascii_digit() => {
            let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
            t[digits.len()..].chars().next().map(|m| matches!(m, '.' | '、' | ')' | '）')).unwrap_or(false)
        }
        _ => false,
    }
}

pub fn cluster_blocks(lines: &[OcrLine]) -> Vec<Block> {
    struct Entry {
        idx: usize,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        empty: bool,
    }
    let mut entries: Vec<Entry> = lines
        .iter()
        .enumerate()
        .map(|(idx, l)| {
            let (x0, y0, x1, y1) = l.quad.axis_aligned_bounds();
            Entry {
                idx,
                x0: x0 as f32,
                y0: y0 as f32,
                x1: x1 as f32,
                y1: y1 as f32,
                empty: l.text.trim().is_empty(),
            }
        })
        .collect();
    if entries.iter().all(|e| e.empty) {
        return vec![];
    }
    // Median line height sets both the cluster gap and the newline gap.
    let mut hs: Vec<f32> = entries.iter().filter(|e| !e.empty).map(|e| (e.y1 - e.y0).max(1.0)).collect();
    hs.sort_by(|a, b| a.total_cmp(b));
    let med_h = hs[hs.len() / 2];
    entries.sort_by(|a, b| a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0)));

    // Cluster: barrier on empty lines; new block on big gap or no x-overlap.
    let mut groups: Vec<Vec<Entry>> = Vec::new();
    let mut barrier = false;
    for e in entries {
        if e.empty {
            barrier = true;
            continue;
        }
        let fresh = barrier
            || groups.last().map(|g: &Vec<Entry>| {
                let prev = &g[g.len() - 1];
                let gap = e.y0 - prev.y1;
                let bx0 = g.iter().map(|m| m.x0).fold(f32::INFINITY, f32::min);
                let bx1 = g.iter().map(|m| m.x1).fold(f32::NEG_INFINITY, f32::max);
                gap > 1.5 * med_h || e.x0 >= bx1 || e.x1 <= bx0
            }).unwrap_or(true);
        barrier = false;
        if fresh {
            groups.push(vec![e]);
        } else {
            groups.last_mut().unwrap().push(e);
        }
    }

    // Join each block: hard breaks restore '\n', soft wraps join inline.
    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        let left = g.iter().map(|m| m.x0).fold(f32::INFINITY, f32::min);
        let right = g.iter().map(|m| m.x1).fold(f32::NEG_INFINITY, f32::max);
        let mut text = String::new();
        for (k, e) in g.iter().enumerate() {
            let t = lines[e.idx].text.trim().to_string();
            if k > 0 {
                let a = &g[k - 1];
                let at = lines[a.idx].text.trim();
                let ah = ((a.y1 - a.y0).max(1.0)).min(10000.0);
                let bh = ((e.y1 - e.y0).max(1.0)).min(10000.0);
                let cw_a = (a.x1 - a.x0) / at.chars().count().max(1) as f32;
                let cw_b = (e.x1 - e.x0) / t.chars().count().max(1) as f32;
                let indent = e.x0 - left > 2.0 * cw_b.max(1.0);
                let big_gap = e.y0 - a.y1 > 0.7 * ah.min(bh);
                let ragged = right - a.x1 > 2.0 * cw_a.max(1.0)
                    && (e.x0 - left).abs() <= 2.0 * cw_b.max(1.0);
                let hard = indent || big_gap || is_list_start(&t) || ragged;
                if hard {
                    text.push('\n');
                } else if at.chars().last().map(is_cjk).unwrap_or(false)
                    || t.chars().next().map(is_cjk).unwrap_or(false)
                {
                    // Adjacent CJK: no separator.
                } else {
                    text.push(' ');
                }
            }
            text.push_str(&t);
        }
        let x0 = g.iter().map(|m| m.x0).fold(f32::INFINITY, f32::min) as i32;
        let y0 = g.iter().map(|m| m.y0).fold(f32::INFINITY, f32::min) as i32;
        let x1 = g.iter().map(|m| m.x1).fold(f32::NEG_INFINITY, f32::max) as i32;
        let y1 = g.iter().map(|m| m.y1).fold(f32::NEG_INFINITY, f32::max) as i32;
        let mut bhs: Vec<f32> = g.iter().map(|m| (m.y1 - m.y0).max(1.0)).collect();
        bhs.sort_by(|a, b| a.total_cmp(b));
        let font_px = (bhs[bhs.len() / 2] as u32).saturating_sub(4).max(12);
        out.push(Block {
            rect: (x0, y0, x1, y1),
            font_px,
            text,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Quad;

    fn line(x0: f32, y0: f32, x1: f32, y1: f32, text: &str) -> OcrLine {
        OcrLine {
            quad: Quad::from_xyxy(x0, y0, x1, y1),
            text: text.to_string(),
            score: 1.0,
            chars: vec![],
        }
    }

    #[test]
    fn paragraph_gap_splits_blocks() {
        let lines = vec![
            line(10.0, 10.0, 400.0, 34.0, "第一段第一行很长很长很长很长"),
            line(10.0, 38.0, 400.0, 62.0, "第一段第二行很长很长很长很长"),
            line(10.0, 130.0, 400.0, 154.0, "第二段只有一行内容"),
        ];
        let b = cluster_blocks(&lines);
        assert_eq!(b.len(), 2, "expected 2 blocks, got {}", b.len());
        assert_eq!(b[0].rect, (10, 10, 400, 62));
        assert_eq!(b[1].rect, (10, 130, 400, 154));
        // Soft wrap: adjacent CJK joins with no separator.
        assert_eq!(b[0].text, "第一段第一行很长很长很长很长第一段第二行很长很长很长很长");
    }

    #[test]
    fn columns_stay_apart() {
        let lines = vec![
            line(10.0, 10.0, 200.0, 34.0, "左栏内容行"),
            line(300.0, 10.0, 500.0, 34.0, "Right column line"),
        ];
        let b = cluster_blocks(&lines);
        assert_eq!(b.len(), 2, "columns must not merge");
    }

    #[test]
    fn indent_and_ragged_restore_newlines() {
        let lines = vec![
            line(10.0, 10.0, 400.0, 34.0, "This is a long wrapped paragraph line one"),
            line(10.0, 38.0, 250.0, 62.0, "short tail"),
            line(10.0, 66.0, 400.0, 90.0, "Next paragraph starts here fresh"),
            line(40.0, 94.0, 400.0, 118.0, "indented continuation line"),
        ];
        let b = cluster_blocks(&lines);
        assert_eq!(b.len(), 1);
        // ragged "short tail" + indented line both force '\n'; the two long
        // lines soft-join with a space.
        assert_eq!(
            b[0].text,
            "This is a long wrapped paragraph line one short tail\nNext paragraph starts here fresh\nindented continuation line"
        );
    }

    #[test]
    fn empty_line_is_a_boundary() {
        let lines = vec![
            line(10.0, 10.0, 400.0, 34.0, "上段"),
            line(10.0, 38.0, 400.0, 62.0, "   "),
            line(10.0, 66.0, 400.0, 90.0, "下段"),
        ];
        let b = cluster_blocks(&lines);
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn list_markers_break() {
        let lines = vec![
            line(10.0, 10.0, 400.0, 34.0, "购物清单如下所示内容"),
            line(10.0, 38.0, 400.0, 62.0, "• 苹果"),
            line(10.0, 66.0, 400.0, 90.0, "• 香蕉"),
        ];
        let b = cluster_blocks(&lines);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].text, "购物清单如下所示内容\n• 苹果\n• 香蕉");
    }
}
