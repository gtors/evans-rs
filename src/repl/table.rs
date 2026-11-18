//! A table formatter that mirrors the `tablewriter` output of the original.

/// Formats a table with the given headers and rows.
pub fn format_table(headers: &[String], rows: &[Vec<String>]) -> String {
    let columns = headers.len();
    if columns == 0 {
        return String::new();
    }

    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(columns) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    out.push_str(&border(&widths));
    out.push('\n');
    out.push_str(&row(
        &widths,
        &headers.iter().map(|h| h.to_uppercase()).collect::<Vec<_>>(),
        true,
    ));
    out.push('\n');
    out.push_str(&border(&widths));
    out.push('\n');
    for cells in rows {
        out.push_str(&row(&widths, cells, false));
        out.push('\n');
    }
    out.push_str(&border(&widths));
    out.push('\n');
    out
}

fn border(widths: &[usize]) -> String {
    let mut out = String::from("+");
    for width in widths {
        out.push_str(&"-".repeat(width + 2));
        out.push('+');
    }
    out
}

fn row(widths: &[usize], cells: &[String], center: bool) -> String {
    let mut out = String::from("|");
    for (i, width) in widths.iter().enumerate() {
        let cell = cells.get(i).map(String::as_str).unwrap_or_default();
        let len = cell.chars().count();
        let padding = width.saturating_sub(len);
        if center {
            let left = padding / 2;
            let right = padding - left;
            out.push(' ');
            out.push_str(&" ".repeat(left));
            out.push_str(cell);
            out.push_str(&" ".repeat(right));
            out.push_str(" |");
        } else {
            out.push(' ');
            out.push_str(cell);
            out.push_str(&" ".repeat(padding));
            out.push_str(" |");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_a_single_column_table() {
        let headers = vec!["name".to_string()];
        let rows = vec![vec!["api.Example".to_string()]];
        assert_eq!(
            format_table(&headers, &rows),
            "+-------------+\n\
             |    NAME     |\n\
             +-------------+\n\
             | api.Example |\n\
             +-------------+\n"
        );
    }

    #[test]
    fn formats_an_empty_table() {
        let headers = vec!["name".to_string()];
        assert_eq!(
            format_table(&headers, &[]),
            "+------+\n| NAME |\n+------+\n+------+\n"
        );
    }
}
