//! Marked text blocks. Only the lines between a relaxkit begin and end marker are ever changed.

/// Inserts a block at the end of `text`, or replaces the body of the existing block `id`.
pub fn block_upsert(text: &str, id: &str, body: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let block = render_block(id, body);
    match find_block(&lines, id) {
        Some((start, stop)) => format!(
            "{}{}{}",
            lines[..start].concat(),
            block,
            lines[stop + 1..].concat()
        ),
        None => {
            let mut out = text.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&block);
            out
        }
    }
}

/// Removes the block `id`, including both markers. Text outside it is untouched.
pub fn block_remove(text: &str, id: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    match find_block(&lines, id) {
        Some((start, stop)) => format!("{}{}", lines[..start].concat(), lines[stop + 1..].concat()),
        None => text.to_string(),
    }
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

/// A full block, markers included, ending in a newline.
fn render_block(id: &str, body: &str) -> String {
    let mut out = format!("{}\n", begin_marker(id));
    for line in body.lines() {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&end_marker(id));
    out.push('\n');
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
