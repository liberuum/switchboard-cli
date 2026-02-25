use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL};

/// Print a table from column headers and rows of strings
pub fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.set_header(headers);

    for row in rows {
        table.add_row(row);
    }

    println!("{table}");
}
