//! Rows to CSV, for `GET /api/stats/export.csv`.

use micaforge_core::{Error, Result};
use serde::Serialize;

/// Serialise a slice of rows as CSV, header row first.
///
/// The header comes from the first row's field names, so every row must be the
/// same shape — which every report's row type is.
///
/// **Flat rows only.** A CSV cell holds one value, so a row carrying a `Vec` or
/// a nested struct (an error group's sparkline, a coverage row's list of agents,
/// a journey's nodes) has no honest representation here and is refused rather
/// than flattened into something that looks like data. The caller exports the
/// JSON for those, or picks the columns it wants first.
///
/// An empty slice produces an empty string rather than a lone header row: there
/// is no first record to take field names from, and inventing them would mean
/// this function knowing about every report's shape.
///
/// **Formula cells are neutralised.** Ingest is public, so an `event_name`, a
/// page title or a referrer can be any text a stranger chose, including
/// `=HYPERLINK(...)` or a DDE payload. A spreadsheet opening the export would
/// run it. Every cell that starts with `=`, `+`, `-`, `@`, a tab or a carriage
/// return gets a leading `'`, which spreadsheets read as "this is text", unless
/// the whole cell is a number: `-3.1` is a delta, not a formula.
pub fn to_csv<T: Serialize>(rows: &[T]) -> Result<String> {
    if rows.is_empty() {
        return Ok(String::new());
    }

    // Serialised once as-is, so the header and the refusal of nested rows stay
    // the `csv` crate's decision, then read back and rewritten cell by cell.
    let mut writer = csv::Writer::from_writer(Vec::new());
    for row in rows {
        writer
            .serialize(row)
            .map_err(|e| Error::BadRequest(format!("these rows cannot be written as CSV: {e}")))?;
    }
    let raw = writer
        .into_inner()
        .map_err(|e| Error::Internal(format!("csv writer: {e}")))?;

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(raw.as_slice());
    let mut out = csv::Writer::from_writer(Vec::new());
    for (index, record) in reader.records().enumerate() {
        let record = record.map_err(|e| Error::Internal(format!("csv reread: {e}")))?;
        let result = if index == 0 {
            // The header row is field names from this crate's own structs.
            out.write_record(&record)
        } else {
            let cells: Vec<std::borrow::Cow<'_, str>> = record.iter().map(neutralise).collect();
            out.write_record(cells.iter().map(|cell| cell.as_bytes()))
        };
        result.map_err(|e| Error::Internal(format!("csv writer: {e}")))?;
    }

    let bytes = out
        .into_inner()
        .map_err(|e| Error::Internal(format!("csv writer: {e}")))?;

    String::from_utf8(bytes).map_err(|e| Error::Internal(format!("csv is not utf-8: {e}")))
}

/// One cell, made safe to open in a spreadsheet. See [`to_csv`].
fn neutralise(cell: &str) -> std::borrow::Cow<'_, str> {
    let risky = cell
        .chars()
        .next()
        .is_some_and(|c| matches!(c, '=' | '+' | '-' | '@' | '\t' | '\r'));
    if risky && cell.parse::<f64>().is_err() {
        std::borrow::Cow::Owned(format!("'{cell}"))
    } else {
        std::borrow::Cow::Borrowed(cell)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Row {
        path: String,
        visitors: u64,
    }

    #[test]
    fn flat_rows_get_a_header_and_one_line_each() {
        let csv = to_csv(&[
            Row {
                path: "/docs".into(),
                visitors: 12,
            },
            Row {
                path: "/pricing".into(),
                visitors: 3,
            },
        ])
        .unwrap();
        assert_eq!(csv, "path,visitors\n/docs,12\n/pricing,3\n");
    }

    /// A value containing the delimiter is quoted by the writer, not mangled.
    #[test]
    fn values_are_quoted_not_stripped() {
        let csv = to_csv(&[Row {
            path: "/a,b".into(),
            visitors: 1,
        }])
        .unwrap();
        assert!(csv.contains("\"/a,b\""));
    }

    #[test]
    fn formula_cells_are_neutralised_and_numbers_are_not() {
        #[derive(Serialize)]
        struct Event {
            event_name: String,
            delta: f64,
        }
        let rows = [
            ("=HYPERLINK(\"https://evil.example\",\"x\")", -0.031),
            ("+cmd|' /C calc'!A0", 1.0),
            ("-2+3", 0.0),
            ("@SUM(A1:A2)", 0.0),
            ("\tlead", 0.0),
            ("\rlead", 0.0),
            ("signup", 2.5),
            ("-12", -12.0),
        ];
        let csv = to_csv(
            &rows
                .iter()
                .map(|(name, delta)| Event {
                    event_name: (*name).to_string(),
                    delta: *delta,
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();

        let mut reader = csv::Reader::from_reader(csv.as_bytes());
        let cells: Vec<(String, String)> = reader
            .records()
            .map(|r| {
                let r = r.unwrap();
                (r[0].to_string(), r[1].to_string())
            })
            .collect();

        assert_eq!(cells[0].0, "'=HYPERLINK(\"https://evil.example\",\"x\")");
        assert_eq!(cells[0].1, "-0.031", "a negative number stays a number");
        assert_eq!(cells[1].0, "'+cmd|' /C calc'!A0");
        assert_eq!(cells[2].0, "'-2+3");
        assert_eq!(cells[3].0, "'@SUM(A1:A2)");
        assert_eq!(cells[4].0, "'\tlead");
        assert_eq!(cells[5].0, "'\rlead");
        assert_eq!(cells[6].0, "signup");
        assert_eq!(cells[7], ("-12".to_string(), "-12.0".to_string()));
        assert!(
            csv.starts_with("event_name,delta\n"),
            "the header is untouched"
        );
    }

    #[test]
    fn an_empty_slice_is_an_empty_document() {
        let rows: Vec<Row> = Vec::new();
        assert_eq!(to_csv(&rows).unwrap(), "");
    }

    /// A nested field has no honest CSV cell, so it is refused by name.
    #[test]
    fn nested_rows_are_refused() {
        #[derive(Serialize)]
        struct Nested {
            path: String,
            sparkline: Vec<u64>,
        }
        let err = to_csv(&[Nested {
            path: "/docs".into(),
            sparkline: vec![1, 2, 3],
        }])
        .unwrap_err();
        assert_eq!(err.code(), "bad_request");
    }
}
