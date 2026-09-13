//! The envelope's timestamp: RFC 3339, millisecond precision, UTC.

use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::OffsetDateTime;

/// `2026-09-13T05:43:18.700Z`: the one spelling every producer stamps, so the
/// stamps of different streams sort against each other as text.
const FORMAT: &[BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

/// Now, as an envelope's `ts`.
#[must_use]
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(FORMAT)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00.000Z"))
}
