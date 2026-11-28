//! Javadoc as Markdown, the way jdtls renders it for a hover or a
//! completion item: the text with its `*` margins gone and its HTML as
//! Markdown (line breaks kept, as jdtls's converter keeps them), inline
//! tags as code, links or plain text, and the block tags (`@param`,
//! `@return`, …) as lists under a heading each.
//!
//! A `{@link}` is a link only where its target exists, and a `{@value}`
//! shows the constant's value: what the code says is asked of a
//! [`Resolve`].

/// What rendering asks of the code around a comment.
pub trait Resolve {
    /// Where a `{@link}` or `@see` target (`#area()`, `Shape#area`,
    /// `String`) is declared, as a link target; `None` where nothing is.
    fn link(&self, target: &str) -> Option<String>;
    /// The value of the constant a `{@value}` names, as written, and
    /// where it is declared.
    fn value(&self, target: &str) -> Option<(String, String)>;
}

/// Renders without the code: no link resolves, no value is known.
pub struct Unresolved;

impl Resolve for Unresolved {
    fn link(&self, _target: &str) -> Option<String> {
        None
    }

    fn value(&self, _target: &str) -> Option<(String, String)> {
        None
    }
}

/// A javadoc comment as markdown, as jdtls's hover renders one.
pub fn javadoc_markdown(comment: &str, resolve: &dyn Resolve) -> String {
    let (description, tags) = javadoc_parts(comment);
    let markdown = prose(&description, resolve);
    javadoc_with_tags(markdown, &tags, resolve)
}

/// A Javadoc comment as a completion item's documentation shows it, as
/// jdtls's: a `@deprecated` note first, then the description; the other
/// tags are left to the hover.
pub fn javadoc_summary(comment: &str, resolve: &dyn Resolve) -> String {
    let (description, tags) = javadoc_parts(comment);
    let mut parts: Vec<String> = tags
        .iter()
        .filter(|(name, _)| name == "deprecated")
        .map(|(_, text)| format!("**Deprecated.** *{}*", prose(text, resolve)))
        .collect();
    let description = prose(&description, resolve);
    if !description.is_empty() {
        parts.push(description);
    }
    parts.join("\n\n")
}

/// The description of a comment and the documentation of its parameters,
/// as jdtls shows what a method inherits into a Markdown comment (`///`):
/// each parameter a list of its own, the lists apart, nothing more.
pub fn javadoc_inherited(comment: &str, resolve: &dyn Resolve) -> String {
    let (description, tags) = javadoc_parts(comment);
    let mut parts = vec![prose(&description, resolve)];
    let parameters: Vec<String> = tags
        .iter()
        .filter(|(name, _)| name == "param")
        .map(|(_, text)| {
            let text = prose(text, resolve);
            match text.split_once(' ') {
                Some((first, rest)) => format!("* **{first}** {rest}"),
                None => format!("* **{text}**"),
            }
        })
        .collect();
    if !parameters.is_empty() {
        parts.push(parameters.join("\n\n<!-- -->\n\n"));
    }
    parts.retain(|part| !part.is_empty());
    parts.join("\n\n")
}

/// Javadoc prose as Markdown: `{@snippet}`s as code, the HTML, then the
/// inline tags.
fn prose(text: &str, resolve: &dyn Resolve) -> String {
    let text = snippets(text.trim());
    inline_tags(&html_to_markdown(&text), resolve)
}

/// A Javadoc comment's description and its block tags, each with its
/// text on one line.
fn javadoc_parts(comment: &str) -> (String, Vec<(String, String)>) {
    let body = comment
        .trim_start_matches("/**")
        .trim_end_matches("*/");
    let lines: Vec<&str> = body
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let trimmed = trimmed.strip_prefix('*').unwrap_or(trimmed);
            trimmed.strip_prefix(' ').unwrap_or(trimmed).trim_end()
        })
        .collect();
    let mut description = String::new();
    let mut tags: Vec<(String, String)> = Vec::new();
    // A `{@snippet …}` holds lines that may look like tags.
    let mut open_braces = 0i32;
    for line in lines {
        let starts_tag = open_braces == 0
            && line.trim_start().starts_with('@')
            && line.trim_start()[1..].starts_with(|character: char| character.is_ascii_alphabetic());
        for character in line.chars() {
            match character {
                '{' => open_braces += 1,
                '}' => open_braces -= 1,
                _ => {}
            }
        }
        open_braces = open_braces.max(0);
        if starts_tag {
            let line = line.trim_start();
            let (tag, rest) = line[1..].split_once(char::is_whitespace).unwrap_or((&line[1..], ""));
            tags.push((tag.to_owned(), rest.trim().to_owned()));
        } else if let Some((_, text)) = tags.last_mut() {
            if !line.trim().is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(line.trim());
            }
        } else {
            description.push_str(line);
            description.push('\n');
        }
    }
    (description, tags)
}

/// The block tags after `markdown`, as jdtls lists them: the ones it
/// knows under their heading, in its order (`@see` without one), then any
/// other under its own name (`@jls`).
fn javadoc_with_tags(mut markdown: String, tags: &[(String, String)], resolve: &dyn Resolve) -> String {
    // `@exception` is `@throws`.
    const HEADINGS: &[(&[&str], Option<&str>)] = &[
        (&["param"], Some("Parameters:")),
        (&["return"], Some("Returns:")),
        (&["throws", "exception"], Some("Throws:")),
        (&["since"], Some("Since:")),
        (&["uses"], Some("Uses:")),
        (&["provides"], Some("Provides:")),
        (&["see"], None),
        (&["author"], Some("Author:")),
        (&["deprecated"], Some("Deprecated.")),
    ];
    let known: Vec<&str> = HEADINGS.iter().flat_map(|(names, _)| names.iter().copied()).collect();
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for (names, heading) in HEADINGS {
        let entries: Vec<String> = tags
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .map(|(name, text)| match name.as_str() {
                "param" | "throws" | "exception" => {
                    let text = prose(text, resolve);
                    match text.split_once(' ') {
                        Some((first, rest)) => format!("**{first}** {rest}"),
                        None => format!("**{text}**"),
                    }
                }
                "see" => see(text, resolve),
                _ => prose(text, resolve),
            })
            .collect();
        if !entries.is_empty() {
            sections.push((heading.map(|heading| format!("**{heading}**")).unwrap_or_default(), entries));
        }
    }
    for (name, text) in tags {
        if known.contains(&name.as_str()) || matches!(name.as_str(), "inheritDoc" | "hidden" | "serial" | "serialData" | "serialField" | "version" | "apiNote" | "implSpec" | "implNote") {
            continue;
        }
        let text = prose(text, resolve);
        let entries = if text.is_empty() { Vec::new() } else { vec![text] };
        sections.push((format!("**@{name}**"), entries));
    }
    for (heading, entries) in sections {
        if !markdown.is_empty() {
            markdown.push_str("\n\n");
        }
        if heading.is_empty() {
            let listed: Vec<String> = entries.iter().map(|entry| format!("* {entry}")).collect();
            markdown.push_str(&listed.join("\n"));
            continue;
        }
        markdown.push_str(&format!("* {heading}"));
        for entry in entries {
            markdown.push_str(&format!("\n  * {entry}"));
        }
    }
    markdown.trim().to_owned()
}

/// An `@see` reference: a link to what it names where that exists, the
/// link it writes (`<a href>`) or its text.
fn see(text: &str, resolve: &dyn Resolve) -> String {
    let text = text.trim();
    if text.starts_with('<') || text.starts_with('"') {
        return prose(text, resolve);
    }
    let (target, label) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    link_text(target, label.trim(), resolve)
}

/// A `{@link target label}` as jdtls writes it: `[label](where)` where the
/// target exists, the label (or the target, `#` read as `.`) as text where
/// it does not.
fn link_text(target: &str, label: &str, resolve: &dyn Resolve) -> String {
    let shown = if label.is_empty() {
        target.trim_start_matches('#').replace('#', ".")
    } else {
        label.to_owned()
    };
    match resolve.link(target) {
        Some(at) => format!("[{shown}]({at})"),
        None if label.is_empty() && !target.contains('#') => format!("`{shown}`"),
        None => shown,
    }
}

/// Each `{@snippet : …}` of `text` as a `<pre>` block, a `@highlight`
/// comment's text in bold and the comment gone.
fn snippets(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{@snippet") {
        out.push_str(&rest[..start]);
        let inside = &rest[start + 1..];
        let Some(end) = closing_brace(inside) else {
            out.push_str(&rest[start..]);
            return out;
        };
        let body = &inside[..end];
        let code = body.split_once(':').map_or("", |(_, code)| code);
        let lines: Vec<String> = code.lines().map(highlighted).collect();
        out.push_str("<pre>\n");
        out.push_str(&lines.join("\n"));
        out.push_str("\n</pre>");
        rest = &inside[end + 1..];
    }
    out.push_str(rest);
    out
}

/// A snippet's line with its `// @highlight substring="…"` applied.
fn highlighted(line: &str) -> String {
    let Some((code, comment)) = line.split_once("// @highlight") else {
        return line.trim_end().to_owned();
    };
    let code = code.trim_end();
    let substring = comment
        .split_once("substring=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(substring, _)| substring);
    match substring {
        Some(substring) if !substring.is_empty() => code.replacen(substring, &format!("**{substring}**"), 1),
        _ => format!("**{}**", code.trim()),
    }
}

/// Where the `}` closing a tag that starts `text` (after its `{`) is.
fn closing_brace(text: &str) -> Option<usize> {
    let mut depth = 1;
    for (index, character) in text.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// The few HTML tags javadoc is written with, as markdown. A `<pre>`
/// keeps its lines, fenced, and a `<blockquote>` is quoted with `> `, as
/// jdtls's converter writes them: a fence is never on a line of prose.
pub(crate) fn html_to_markdown(text: &str) -> String {
    let lowered = lowercase_tags(text);
    let text = lowered.as_str();
    const BLOCK_TAGS: [&str; 4] = ["<pre>", "</pre>", "<blockquote>", "</blockquote>"];
    let mut blocks: Vec<(bool, String)> = Vec::new();
    let mut in_pre = false;
    let mut quoted = 0usize;
    let mut rest = text;
    loop {
        let next = BLOCK_TAGS
            .iter()
            .filter_map(|tag| rest.find(tag).map(|at| (at, *tag)))
            .min();
        let (segment, tag) = match next {
            Some((at, tag)) => (&rest[..at], Some(tag)),
            None => (rest, None),
        };
        if in_pre {
            let code = preformatted(segment);
            if !code.is_empty() {
                blocks.push((quoted > 0, format!("```\n{code}\n```")));
            }
        } else {
            for paragraph in inline_html_to_markdown(segment) {
                blocks.push((quoted > 0, paragraph));
            }
        }
        let Some(tag) = tag else { break };
        match tag {
            "<pre>" => in_pre = true,
            "</pre>" => in_pre = false,
            "<blockquote>" => quoted += 1,
            _ => quoted = quoted.saturating_sub(1),
        }
        rest = &rest[segment.len() + tag.len()..];
    }
    blocks
        .into_iter()
        .map(|(quoted, block)| {
            if quoted {
                block.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n")
            } else {
                block
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `text` with each HTML tag's name in lower case (`<PRE>` is `<pre>`):
/// HTML does not tell them apart, and older javadoc writes them in capitals.
fn lowercase_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let slash = usize::from(after.starts_with('/'));
        let name_length = after[slash..]
            .find(|character: char| !character.is_ascii_alphanumeric())
            .unwrap_or(after.len() - slash);
        out.push('<');
        out.push_str(&after[..slash]);
        out.push_str(&after[slash..slash + name_length].to_ascii_lowercase());
        rest = &after[slash + name_length..];
    }
    out.push_str(rest);
    out
}

/// The lines of a `<pre>` block, without the blank lines around them or
/// the indentation they all share.
fn preformatted(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    let (Some(first), Some(last)) = (first, last) else {
        return String::new();
    };
    let lines = &lines[first..=last];
    let indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|line| decode_entities(line.get(indent..).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// Prose with its inline HTML as markdown, one string a paragraph, its
/// lines as written.
fn inline_html_to_markdown(text: &str) -> Vec<String> {
    let mut out = anchors(text);
    for level in 1..=6 {
        out = out
            .replace(&format!("<h{level}>"), &format!("\n\n{} ", "#".repeat(level)))
            .replace(&format!("</h{level}>"), "\n\n");
    }
    for (tag, replacement) in [
        ("<p>", "\n\n"),
        ("</p>", "\n\n"),
        ("<br>", "\n"),
        ("<br/>", "\n"),
        ("<code>", "`"),
        ("</code>", "`"),
        ("<b>", "**"),
        ("</b>", "**"),
        ("<strong>", "**"),
        ("</strong>", "**"),
        ("<i>", "*"),
        ("</i>", "*"),
        ("<em>", "*"),
        ("</em>", "*"),
        ("<dfn>", "_"),
        ("</dfn>", "_"),
        ("<ul>", "\n"),
        ("</ul>", "\n"),
        ("<li>", "\n* "),
        ("</li>", ""),
        ("<dl>", "\n\n"),
        ("</dl>", "\n\n"),
        ("<dt>", "\n\n"),
        ("</dt>", "\n\n"),
        ("<dd>", "\n\n"),
        ("</dd>", "\n\n"),
    ] {
        out = out.replace(tag, replacement);
    }
    let out = decode_entities(&out);
    out.split("\n\n")
        .map(|paragraph| {
            paragraph
                .lines()
                .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|joined| !joined.is_empty())
        .collect()
}

/// `<a href="u">text</a>` as `[text](u)`; an anchor without a target
/// (`<a id="…">`) as its text.
fn anchors(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("<a ") {
        let Some(close) = rest[open..].find('>').map(|at| open + at) else { break };
        let Some(end) = rest[close..].find("</a>").map(|at| close + at) else { break };
        out.push_str(&rest[..open]);
        let attributes = &rest[open + 3..close];
        let inner = &rest[close + 1..end];
        let href = attributes
            .split_once("href=\"")
            .and_then(|(_, after)| after.split_once('"'))
            .map(|(href, _)| href);
        match href {
            Some(href) => out.push_str(&format!("[{inner}]({href})")),
            None => out.push_str(inner),
        }
        rest = &rest[end + 4..];
    }
    out.push_str(rest);
    out
}

/// The inline tags of javadoc prose: `{@code}` and `{@literal}` as code,
/// `{@link}` as [`link_text`], `{@value}` as the constant's value linked
/// to it, the others as their text.
fn inline_tags(text: &str, resolve: &dyn Resolve) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{@") {
        out.push_str(&rest[..start]);
        let inside = &rest[start + 2..];
        // Braces nest in `{@code Map<K, V> {}}`.
        let Some(end) = closing_brace(inside) else {
            out.push_str(&rest[start..]);
            return out;
        };
        let tag = &inside[..end];
        let (name, argument) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        let argument = argument.trim();
        match name {
            "code" | "literal" => out.push_str(&format!("`{argument}`")),
            "value" => match resolve.value(argument) {
                Some((value, at)) => out.push_str(&format!("[{value}]({at})")),
                None => out.push_str(&format!("`{argument}`")),
            },
            "link" | "linkplain" => {
                let (target, label) = argument.split_once(char::is_whitespace).unwrap_or((argument, ""));
                if target.starts_with("##") {
                    // An anchor of the page: its label, as text.
                    out.push_str(label.trim());
                } else if name == "linkplain" && resolve.link(target).is_none() && !label.trim().is_empty() {
                    out.push_str(label.trim());
                } else {
                    out.push_str(&link_text(target, label.trim(), resolve));
                }
            }
            "inheritDoc" => {}
            "index" => out.push_str(argument.split_whitespace().next().unwrap_or(argument)),
            _ => out.push_str(argument),
        }
        rest = &inside[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{Unresolved, html_to_markdown, javadoc_markdown};

    #[test]
    fn a_quoted_pre_block_keeps_its_lines_behind_the_quote() {
        let javadoc = "is equivalent to:\n<blockquote><pre>\n    char data[] = {'a', 'b'};\n    \
                       String str = new String(data);\n</pre></blockquote><p>\nHere are more.";
        assert_eq!(
            html_to_markdown(javadoc),
            "is equivalent to:\n\n> ```\n> char data[] = {'a', 'b'};\n> \
             String str = new String(data);\n> ```\n\nHere are more."
        );
    }

    #[test]
    fn a_tag_in_capitals_is_the_same_tag() {
        assert_eq!(
            html_to_markdown("Classes use:\n<PRE>\n Object readResolve();\n</PRE><P>\nMore."),
            "Classes use:\n\n```\nObject readResolve();\n```\n\nMore."
        );
    }

    #[test]
    fn a_pre_block_is_fenced_on_lines_of_its_own() {
        assert_eq!(
            html_to_markdown("Use it so: <pre>\n  List&lt;String&gt; names;\n</pre> and done."),
            "Use it so:\n\n```\nList<String> names;\n```\n\nand done."
        );
    }

    #[test]
    fn a_paragraph_keeps_its_lines() {
        assert_eq!(html_to_markdown("One line\nand the next."), "One line\nand the next.");
    }

    #[test]
    fn a_heading_and_an_anchor_are_markdown() {
        assert_eq!(
            html_to_markdown("<h4><a id=\"x\">Special</a></h4>\nText <a href=\"u\">there</a>."),
            "#### Special\n\nText [there](u)."
        );
    }

    #[test]
    fn a_tag_jdtls_does_not_know_is_listed_under_its_name() {
        let comment = "/**\n * Text.\n * @since 9\n * @jls 9.6 Target\n * @moduleGraph\n */";
        assert_eq!(
            javadoc_markdown(comment, &Unresolved),
            "Text.\n\n* **Since:**\n  * 9\n\n* **@jls**\n  * 9.6 Target\n\n* **@moduleGraph**"
        );
    }

    #[test]
    fn a_snippet_is_code_with_its_highlight_in_bold() {
        let comment = "/**\n * A program.\n * {@snippet :\n *   say(\"hi\"); // @highlight substring=\"say\"\n * }\n */";
        assert_eq!(javadoc_markdown(comment, &Unresolved), "A program.\n\n```\n**say**(\"hi\");\n```");
    }
}
