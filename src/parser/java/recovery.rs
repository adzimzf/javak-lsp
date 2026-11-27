//! Error recovery where tree-sitter's own gives up on a whole type: one
//! unterminated string or one malformed expression can leave a class
//! declaration as a single `ERROR` node, where JDT's parser keeps the class
//! and its members and loses only the broken expression.
//!
//! The text is repaired a piece at a time, every repair the same length as
//! what it replaces so that each position stays where it was, and parsed
//! again; a repair is kept while it leaves less of the file in errors. Only
//! a file that lost a type declaration is repaired: a file whose errors
//! tree-sitter keeps local is read as it is.

use crate::parser::Syn;

/// How many repairs one file gets.
const REPAIRS: usize = 8;

/// The tree of `text`, repaired where a type declaration was lost to an
/// error; `parse` reads a text into a tree.
pub fn recovered(text: &str, parse: impl Fn(&str) -> Option<Syn>) -> Option<Syn> {
    let original = parse(text)?;
    if !lost_a_type(&original) {
        return Some(original);
    }
    let mut best = original.clone();
    let mut repaired = text.to_owned();
    // Once the type is back, the errors left are repaired as well.
    for _ in 0..REPAIRS {
        let Some(next) = repair_once(&repaired, &best) else {
            break;
        };
        let Some(tree) = parse(&next) else {
            break;
        };
        if error_size(&tree) >= error_size(&best) {
            break;
        }
        repaired = next;
        best = tree;
    }
    // What the repairs blanked out is kept, as an error inside the node
    // that now holds its place: `m[]` of `new int[m[]]` is still a type
    // JDT's recovery names.
    for (start, end) in changed_spans(text, &repaired) {
        let kept: Vec<Syn> = original
            .walk()
            .into_iter()
            .filter(|node| node.kind == "ERROR")
            .flat_map(|error| error.children.iter())
            .filter(|child| start <= child.start && child.end <= end && child.kind != "ERROR")
            .cloned()
            .collect();
        if let (Some(first), Some(last)) = (kept.first(), kept.last()) {
            let graft = Syn {
                kind: "ERROR".to_owned(),
                field: None,
                text: String::new(),
                start: first.start,
                end: last.end,
                children: kept.clone(),
            };
            graft_into(&mut best, graft);
        }
    }
    Some(best)
}

/// The spans where `repaired` differs from `text`, the two the same
/// length line by line.
fn changed_spans(text: &str, repaired: &str) -> Vec<(crate::parser::Position, crate::parser::Position)> {
    let mut spans = Vec::new();
    for (line, (before, after)) in text.split('\n').zip(repaired.split('\n')).enumerate() {
        if before == after {
            continue;
        }
        let first = before.bytes().zip(after.bytes()).position(|(left, right)| left != right).unwrap_or(0);
        let last = before.bytes().zip(after.bytes()).rposition(|(left, right)| left != right).unwrap_or(first);
        spans.push((
            crate::parser::Position { line: line as u32, column: first as u32 },
            crate::parser::Position { line: line as u32, column: last as u32 + 1 },
        ));
    }
    spans
}

/// `graft` as a child of the innermost node around it, in order.
fn graft_into(node: &mut Syn, graft: Syn) {
    let inner = node
        .children
        .iter()
        .position(|child| child.start <= graft.start && graft.end <= child.end && !child.children.is_empty());
    match inner {
        Some(index) => graft_into(&mut node.children[index], graft),
        None => {
            let at = node.children.iter().position(|child| child.start >= graft.end).unwrap_or(node.children.len());
            node.children.insert(at, graft);
        }
    }
}

/// Whether an error swallowed a type declaration: an `ERROR` node holding
/// the keyword of one directly.
fn lost_a_type(tree: &Syn) -> bool {
    tree.walk().into_iter().any(|node| {
        node.kind == "ERROR"
            && node
                .children
                .iter()
                .any(|child| matches!(child.kind.as_str(), "class" | "interface" | "enum" | "record"))
    })
}

/// How much of the file errors hold: the lines of every `ERROR` node with
/// no `ERROR` around it.
fn error_size(tree: &Syn) -> usize {
    fn size(node: &Syn) -> usize {
        if node.kind == "ERROR" {
            return (node.end.line - node.start.line) as usize * 1000 + node.text.len().min(999);
        }
        node.children.iter().map(size).sum()
    }
    size(tree)
}

fn repair_once(text: &str, tree: &Syn) -> Option<String> {
    unterminated_string(text).or_else(|| broken_initializer(text, tree))
}

/// The first string literal that does not end on its line, closed right
/// after its opening quote: `"some str` reads `""       `.
fn unterminated_string(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at = text[at..].find('\n').map_or(bytes.len(), |end| at + end);
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at = text[at + 2..].find("*/").map_or(bytes.len(), |end| at + 2 + end + 2);
            }
            b'"' if text[at..].starts_with("\"\"\"") => {
                at = text[at + 3..].find("\"\"\"").map_or(bytes.len(), |end| at + 3 + end + 3);
            }
            quote @ (b'"' | b'\'') => {
                let mut end = at + 1;
                let mut closed = false;
                while end < bytes.len() && bytes[end] != b'\n' {
                    if bytes[end] == b'\\' {
                        end += 2;
                        continue;
                    }
                    if bytes[end] == quote {
                        closed = true;
                        break;
                    }
                    end += 1;
                }
                if !closed && quote == b'"' {
                    let end = end.min(bytes.len());
                    let rest = &text[at + 1..end];
                    let mut repaired = String::with_capacity(text.len());
                    repaired.push_str(&text[..at]);
                    if rest.is_empty() {
                        repaired.push(' ');
                    } else {
                        repaired.push_str("\"\"");
                        repaired.extend(std::iter::repeat_n(' ', rest.chars().count() - 1));
                    }
                    repaired.push_str(&text[end..]);
                    return Some(repaired);
                }
                at = end + 1;
            }
            _ => at += 1,
        }
    }
    None
}

/// The value of the declaration an error starts in, as `0`: `int[] a = new
/// int[m[]];` reads `int[] a = 0          ;`, the declaration kept.
fn broken_initializer(text: &str, tree: &Syn) -> Option<String> {
    // The first error with no other error inside it.
    let error = tree
        .walk()
        .into_iter()
        .filter(|node| node.kind == "ERROR" && !node.children.iter().any(|child| child.walk().iter().any(|below| below.kind == "ERROR")))
        .filter(|node| !node.children.iter().any(|child| matches!(child.kind.as_str(), "class" | "interface" | "enum" | "record")))
        .min_by_key(|node| node.start)?;
    let line_start = text.split_inclusive('\n').take(error.start.line as usize).map(str::len).sum::<usize>();
    let line = text[line_start..].split('\n').next()?;
    // Columns count UTF-16 units; this line's are taken as bytes, and a
    // line that is not ASCII is left alone.
    if !line.is_ascii() {
        return None;
    }
    let column = (error.start.column as usize).min(line.len());
    let equals = line[..=column.min(line.len().saturating_sub(1))]
        .char_indices()
        .filter(|(at, character)| {
            *character == '='
                && !matches!(line.as_bytes().get(at + 1), Some(b'='))
                && !matches!(line.as_bytes().get(at.wrapping_sub(1)), Some(b'=' | b'!' | b'<' | b'>'))
        })
        .map(|(at, _)| at)
        .next_back()?;
    let end = line.rfind(';').filter(|end| *end > equals)?;
    let value_start = equals + 1 + line[equals + 1..].len() - line[equals + 1..].trim_start().len();
    if value_start >= end {
        return None;
    }
    let mut repaired = String::with_capacity(text.len());
    repaired.push_str(&text[..line_start + value_start]);
    repaired.push('0');
    repaired.extend(std::iter::repeat_n(' ', end - value_start - 1));
    repaired.push_str(&text[line_start + end..]);
    Some(repaired)
}

#[cfg(test)]
mod tests {
    use super::unterminated_string;

    #[test]
    fn an_unterminated_string_is_closed_where_it_opens() {
        let text = "class A {\n\tvoid f() {\nString s = \"some str\n\t}\n}\n";
        let repaired = unterminated_string(text).expect("a repair");
        assert_eq!(repaired.len(), text.len());
        assert!(repaired.contains("String s = \"\"       \n"));
    }

    #[test]
    fn a_closed_string_and_a_comment_are_left_alone() {
        assert_eq!(unterminated_string("String s = \"a\"; // \"b\n"), None);
    }
}
