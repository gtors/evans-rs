/// Formats a list of names, one per line.
pub fn format_names(names: &[String]) -> String {
    names.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_names_with_newlines() {
        let names = vec!["api.Example".to_string(), "api.Other".to_string()];
        assert_eq!(format_names(&names), "api.Example\napi.Other");
    }

    #[test]
    fn formats_an_empty_list() {
        assert_eq!(format_names(&[]), "");
    }
}
