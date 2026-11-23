//! Marked text blocks. Only the lines between a relaxkit begin and end marker are ever changed.

/// Inserts a block at the end of `text`, or replaces the body of the existing block `id`. A text with no final newline
/// stays that way: the block starts on a new line and ends without a newline, so removing it restores the text.
pub fn block_upsert(text: &str, id: &str, body: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    match find_block(&lines, id) {
        Some((start, stop)) => {
            let block = render_block(id, body, ends_bare(&lines, stop));
            format!(
                "{}{}{}",
                lines[..start].concat(),
                block,
                lines[stop + 1..].concat()
            )
        }
        None if text.is_empty() => render_block(id, body, false),
        None if text.ends_with('\n') => format!("{text}{}", render_block(id, body, false)),
        None => format!("{text}\n{}", render_block(id, body, true)),
    }
}

/// Removes the block `id`, including both markers. Text outside it is untouched, and so is the line break that
/// `block_upsert` added before a block set on text without a final newline.
pub fn block_remove(text: &str, id: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some((start, stop)) = find_block(&lines, id) else {
        return text.to_string();
    };
    let mut before = lines[..start].concat();
    if ends_bare(&lines, stop) && before.ends_with('\n') {
        before.pop();
    }
    format!("{}{}", before, lines[stop + 1..].concat())
}

/// Whether the block's end marker is the last line of the text and has no line break after it.
fn ends_bare(lines: &[&str], stop: usize) -> bool {
    stop + 1 == lines.len() && !lines[stop].ends_with('\n')
}

/// The body of block `id` without its trailing newline, or `None` when there is no block.
pub fn block_get(text: &str, id: &str) -> Option<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let (start, stop) = find_block(&lines, id)?;
    let body: Vec<&str> = lines[start + 1..stop]
        .iter()
        .map(|line| line.trim_end_matches(['\n', '\r']))
        .collect();
    Some(body.join("\n"))
}

fn begin_marker(id: &str) -> String {
    format!("<!-- relaxkit:begin {id} -->")
}

fn end_marker(id: &str) -> String {
    format!("<!-- relaxkit:end {id} -->")
}

/// Line indexes of block `id`: the nearest begin marker before each end marker.
/// An unclosed begin marker is skipped, so user text after it is never taken into a block.
fn find_block(lines: &[&str], id: &str) -> Option<(usize, usize)> {
    let begin = begin_marker(id);
    let end = end_marker(id);
    let mut start = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == begin {
            start = Some(index);
        } else if let (true, Some(first)) = (trimmed == end, start) {
            return Some((first, index));
        }
    }
    None
}

/// A full block, markers included. It ends in a newline unless `bare` asks for none.
fn render_block(id: &str, body: &str, bare: bool) -> String {
    let mut out = format!("{}\n", begin_marker(id));
    for line in body.lines() {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&end_marker(id));
    if !bare {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_upsert_appends_then_replaces_only_the_body() {
        let base = "# My notes\nuser line\n";
        let appended = block_upsert(base, "core", "one");
        assert_eq!(
            appended,
            "# My notes\nuser line\n<!-- relaxkit:begin core -->\none\n<!-- relaxkit:end core -->\n"
        );
        let replaced = block_upsert(&appended, "core", "two\nthree");
        assert_eq!(
            replaced,
            "# My notes\nuser line\n<!-- relaxkit:begin core -->\ntwo\nthree\n<!-- relaxkit:end core -->\n"
        );
    }

    #[test]
    fn block_upsert_leaves_text_after_the_block_alone() {
        let text = "before\n<!-- relaxkit:begin core -->\nold\n<!-- relaxkit:end core -->\nafter\n";
        let out = block_upsert(text, "core", "new");
        assert_eq!(
            out,
            "before\n<!-- relaxkit:begin core -->\nnew\n<!-- relaxkit:end core -->\nafter\n"
        );
    }

    #[test]
    fn orphan_begin_marker_never_swallows_user_text() {
        let orphan = "<!-- relaxkit:begin core -->\nuser stuff\n";
        let first = block_upsert(orphan, "core", "one");
        assert!(first.contains("user stuff"), "user text lost:\n{first}");
        let second = block_upsert(&first, "core", "two");
        assert!(second.contains("user stuff"), "user text lost:\n{second}");
        assert_eq!(block_get(&second, "core"), Some("two".to_string()));
    }

    #[test]
    fn block_upsert_is_idempotent() {
        let once = block_upsert("notes\n", "core", "same body");
        assert_eq!(block_upsert(&once, "core", "same body"), once);
    }

    #[test]
    fn block_remove_restores_original() {
        let original = "# notes\n\nlast line\n";
        let with_block = block_upsert(original, "core", "body");
        assert_eq!(block_remove(&with_block, "core"), original);
        assert_eq!(block_remove(original, "core"), original);
    }

    #[test]
    fn a_block_set_on_text_without_a_final_newline_is_removed_exactly() {
        let original = "# owner notes";
        let with_block = block_upsert(original, "core", "body");
        assert_eq!(block_remove(&with_block, "core"), original);
    }

    #[test]
    fn replacing_the_body_keeps_a_block_without_a_final_newline() {
        let original = "# owner notes";
        let once = block_upsert(original, "core", "one");
        let replaced = block_upsert(&once, "core", "two");
        assert_eq!(block_get(&replaced, "core"), Some("two".to_string()));
        assert_eq!(block_remove(&replaced, "core"), original);
    }

    #[test]
    fn block_get_returns_the_body() {
        let text = block_upsert("x\n", "core", "two\nthree");
        assert_eq!(block_get(&text, "core"), Some("two\nthree".to_string()));
        assert_eq!(block_get(&text, "other"), None);
    }

    #[test]
    fn blocks_with_different_ids_do_not_collide() {
        let text = block_upsert(&block_upsert("", "a", "1"), "b", "2");
        assert_eq!(block_get(&text, "a"), Some("1".to_string()));
        assert_eq!(block_get(&text, "b"), Some("2".to_string()));
    }
}
