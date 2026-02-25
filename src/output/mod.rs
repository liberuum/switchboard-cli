mod table;
mod json;

pub use table::print_table;
pub use json::print_json;

use clap::ValueEnum;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    Raw,
}
