mod json;
mod table;

pub use json::print_json;
pub use table::print_table;

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Raw,
}
