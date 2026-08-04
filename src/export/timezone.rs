//! Where a coordinate sits in time: the UTC offset in force at a place and an instant, worked out
//! entirely offline.
//!
//! A memory entry's `Date` is UTC and its `Location` is a coordinate pair. Neither says what the
//! clock on the wall read, which is what `DateTimeOriginal` means. This module is the bridge:
//! `tzf-rs` turns the coordinate into an IANA zone name against bundled boundary polygons, and
//! `chrono-tz` turns that name plus the instant into the offset, so daylight saving is resolved at
//! the memory's own date rather than at today's.
//!
//! Both answers are `Option`. A coordinate the boundary data names no zone for — mid-ocean, or a
//! gap between polygons — is a fact about the data, not a failure of the run, and every caller
//! already has a no-offset path to fall back to.

use std::sync::LazyLock;

use chrono::{FixedOffset, NaiveDateTime, Offset, TimeZone};
use chrono_tz::Tz;
use tzf_rs::DefaultFinder;

use crate::export::model::LocationPoint;

/// Built once for the process, on first use.
///
/// `DefaultFinder::default` parses several megabytes of bundled polygon data and builds its
/// spatial index, which is work no run can afford once per memory. `LazyLock` rather than an
/// argument threaded through every caller: the finder is immutable, has no configuration this
/// crate varies, and is `Sync`, so there is nothing for a caller to own.
static FINDER: LazyLock<DefaultFinder> = LazyLock::new(DefaultFinder::default);

/// The IANA zone `location` falls in.
///
/// Takes a [`LocationPoint`] rather than two `f64`s on purpose: `tzf-rs` reads **longitude
/// first**, which is the opposite of how every coordinate in this export is written, and a
/// validated pair is what makes that swap unexpressible at the call site.
///
/// # Examples
///
/// ```
/// use exportsnap::export::model::{Field, LocationPoint};
/// use exportsnap::export::timezone;
///
/// let berlin = LocationPoint::parse(Field::Location, "Latitude, Longitude: 52.52, 13.405").unwrap();
/// assert_eq!(timezone::zone(berlin).unwrap().name(), "Europe/Berlin");
/// ```
#[must_use]
pub fn zone(location: LocationPoint) -> Option<Tz> {
    // An empty name is how `tzf-rs` spells "no polygon covers this point".
    let name = FINDER.get_tz_name(location.longitude(), location.latitude());
    if name.is_empty() { None } else { name.parse().ok() }
}

/// The offset local clocks at `location` were at when it was `utc`.
///
/// `utc` decides which side of a daylight-saving boundary the answer falls on, so a memory taken
/// in July and one taken in December at the same place resolve differently.
///
/// # Examples
///
/// ```
/// use chrono::NaiveDate;
/// use exportsnap::export::model::{Field, LocationPoint};
/// use exportsnap::export::timezone;
///
/// let berlin = LocationPoint::parse(Field::Location, "Latitude, Longitude: 52.52, 13.405").unwrap();
/// let summer = NaiveDate::from_ymd_opt(2021, 7, 1).unwrap().and_hms_opt(12, 0, 0).unwrap();
/// let winter = NaiveDate::from_ymd_opt(2021, 1, 1).unwrap().and_hms_opt(12, 0, 0).unwrap();
///
/// assert_eq!(timezone::offset(berlin, summer).unwrap().local_minus_utc(), 2 * 3600);
/// assert_eq!(timezone::offset(berlin, winter).unwrap().local_minus_utc(), 3600);
/// ```
#[must_use]
pub fn offset(location: LocationPoint, utc: NaiveDateTime) -> Option<FixedOffset> {
    Some(zone(location)?.offset_from_utc_datetime(&utc).fix())
}
