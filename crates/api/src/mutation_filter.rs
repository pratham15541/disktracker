/// Display rules for mutation_log-backed commands (history, snapshot diff, top):
/// - Created / Deleted / Renamed: always show, including 0B
/// - Modified: only show when |size_delta| >= 2
pub(crate) fn is_displayable_mutation(kind: &str, size_delta: i64) -> bool {
    match kind {
        "Created" | "Deleted" | "Renamed" => true,
        "Modified" => size_delta.abs() >= 2,
        _ => size_delta.abs() >= 2,
    }
}

/// Whether two consecutive history rows are the same display identity and can be
/// aggregated. Modified rows stay unique (each write changes file content).
pub(crate) fn can_aggregate_history_entries(
    a_kind: &str,
    a_file_id: u64,
    a_name: &str,
    b_kind: &str,
    b_file_id: u64,
    b_name: &str,
) -> bool {
    if a_kind == "Modified" || b_kind == "Modified" {
        return false;
    }
    a_kind == b_kind && a_file_id == b_file_id && a_name == b_name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displayable_allows_zero_byte_create_delete_rename() {
        assert!(is_displayable_mutation("Created", 0));
        assert!(is_displayable_mutation("Deleted", 0));
        assert!(is_displayable_mutation("Renamed", 0));
    }

    #[test]
    fn displayable_modified_requires_two_bytes() {
        assert!(!is_displayable_mutation("Modified", 0));
        assert!(!is_displayable_mutation("Modified", 1));
        assert!(!is_displayable_mutation("Modified", -1));
        assert!(is_displayable_mutation("Modified", 2));
        assert!(is_displayable_mutation("Modified", -2));
    }

    #[test]
    fn aggregate_consecutive_same_create_but_not_modified() {
        assert!(can_aggregate_history_entries(
            "Created", 1, "a.txt", "Created", 1, "a.txt"
        ));
        assert!(!can_aggregate_history_entries(
            "Created", 1, "a.txt", "Created", 2, "a.txt"
        ));
        assert!(!can_aggregate_history_entries(
            "Created", 1, "a.txt", "Deleted", 1, "a.txt"
        ));
        assert!(!can_aggregate_history_entries(
            "Modified", 1, "a.txt", "Modified", 1, "a.txt"
        ));
    }
}
