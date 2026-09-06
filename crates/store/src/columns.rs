//! Resolve a codec's column names once for a streaming query. Single-row
//! callers can keep using names; both paths use the same typed decoder.

macro_rules! column_map {
    ($visibility:vis struct $name:ident { $($field:ident: $column:literal),+ $(,)? }) => {
        $visibility struct $name<I> {
            $(pub(crate) $field: I),+
        }

        impl $name<&'static str> {
            pub(crate) const NAMED: Self = Self { $($field: $column),+ };

            pub(crate) fn resolve(
                statement: &rusqlite::Statement<'_>,
            ) -> rusqlite::Result<$name<usize>> {
                Ok($name { $($field: statement.column_index($column)?),+ })
            }
        }
    };
}

pub(crate) use column_map;

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, Error};

    column_map! {
        struct Columns {
            content: "content",
            detail: "detail",
        }
    }

    #[test]
    fn resolves_each_statement_layout_and_preserves_nulls() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in [
            "SELECT 'body' AS content, NULL AS detail",
            "SELECT 42 AS unrelated, NULL AS detail, 'body' AS content",
        ] {
            let mut statement = conn.prepare(sql).unwrap();
            let columns = Columns::resolve(&statement).unwrap();
            statement
                .query_row([], |row| {
                    assert_eq!(row.get::<_, String>(columns.content)?, "body");
                    assert_eq!(row.get::<_, Option<String>>(columns.detail)?, None);
                    assert_eq!(
                        row.get::<_, String>(columns.content)?,
                        row.get::<_, String>(Columns::NAMED.content)?
                    );
                    Ok(())
                })
                .unwrap();
        }
    }

    #[test]
    fn missing_column_fails_before_the_scan() {
        let conn = Connection::open_in_memory().unwrap();
        let statement = conn.prepare("SELECT 'body' AS content").unwrap();
        assert!(matches!(
            Columns::resolve(&statement),
            Err(Error::InvalidColumnName(name)) if name == "detail"
        ));
    }
}
