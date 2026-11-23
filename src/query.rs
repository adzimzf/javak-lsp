//! How a workspace-symbol query matches a name, as jdtls matches it.
//!
//! - a lower-case query is a prefix: `circle` finds `CircleTest`, not
//!   `SemiCircleTest`;
//! - a query of capitals is the initials of the name's first segments:
//!   `CT` finds `CircleTest` and `CircleTools`, not `SemiCircleTest`;
//! - any other query is its own camel-case segments, each the prefix of a
//!   segment of the name, run together anywhere in it: `CirTe` finds
//!   `CircleTest` and `SemiCircleTest`.

pub fn matches(query: &str, name: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    if !query.chars().any(char::is_uppercase) {
        return name.to_lowercase().starts_with(&query.to_lowercase());
    }
    let segments = camel_segments(name);
    let wanted = camel_segments(query);
    if query.chars().all(|character| !character.is_lowercase()) {
        return starts_at(&segments, &wanted);
    }
    (0..segments.len()).any(|skipped| starts_at(&segments[skipped..], &wanted))
}

/// Whether each wanted segment is a prefix of the segment beside it.
fn starts_at(segments: &[&str], wanted: &[&str]) -> bool {
    wanted.len() <= segments.len()
        && wanted.iter().zip(segments).all(|(wanted, segment)| {
            segment.to_lowercase().starts_with(&wanted.to_lowercase())
        })
}

/// `SemiCircleTest` as `["Semi", "Circle", "Test"]`.
fn camel_segments(name: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (offset, character) in name.char_indices() {
        if offset > start && character.is_uppercase() {
            segments.push(&name[start..offset]);
            start = offset;
        }
    }
    if start < name.len() {
        segments.push(&name[start..]);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn a_query_matches_the_names_jdtls_matches() {
        assert!(matches("circle", "CircleTools"));
        assert!(!matches("circle", "SemiCircleTest"));
        assert!(matches("CT", "CircleTest"));
        assert!(!matches("CT", "SemiCircleTest"));
        assert!(matches("CirTe", "SemiCircleTest"));
        assert!(!matches("CirTe", "CircleTools"));
        assert!(matches("IFoo", "IFoo"));
        assert!(!matches("Abracadabra", "Circle"));
    }
}
