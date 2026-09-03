use chrono::{DateTime, Utc};
use uuid::Uuid;

pub(super) fn make_sql_conv_error(index: usize, err: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
    )
}

pub(super) fn parse_uuid_for_sql(value: String, index: usize) -> rusqlite::Result<Uuid> {
    value
        .parse::<Uuid>()
        .map_err(|e| make_sql_conv_error(index, e.to_string()))
}

pub(super) fn parse_datetime_for_sql(
    value: String,
    index: usize,
) -> rusqlite::Result<DateTime<Utc>> {
    value
        .parse::<DateTime<Utc>>()
        .map_err(|e| make_sql_conv_error(index, e.to_string()))
}
