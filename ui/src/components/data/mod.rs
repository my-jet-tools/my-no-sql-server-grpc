pub mod cell;

mod tables_pane;
pub use tables_pane::*;

mod partitions_pane;
pub use partitions_pane::*;

mod table_header;
pub use table_header::*;

mod table_toolbar;
pub use table_toolbar::*;

mod rows_table;
pub use rows_table::*;

mod row_drawer;
pub use row_drawer::*;

mod pagination;
pub use pagination::*;

pub fn format_compact_count(n: u64) -> String {
    let v = n as f64;
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}K", v / 1_000.0)
    } else {
        format!("{}", n)
    }
}
