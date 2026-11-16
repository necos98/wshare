//! The `<outcome>[:<detail>]` end of a `transfer-ended` line: how a
//! transfer ended, as a tag, and the text that goes with it (a folder, the
//! technical detail of an error, a path). The interface decides on the tag
//! alone; the detail is only shown.

use std::path::PathBuf;

use protocol::transfer::TransferError;

use crate::transfer::listing::ListingError;
use crate::transfer::safe_path::PathError;
use crate::transfer::{Failure, Outcome};

const REFUSED: &str = "refused";
const FAILED: &str = "failed";
const NO_LONGER_ALLOWED: &str = "failed-no-longer-allowed";
/// The files picked could not be listed: `failed-listing-<why>`.
const LISTING: &str = "failed-listing";
/// Between a path and what follows it in a detail: `|` cannot be in a
/// Windows path.
const PATH_END: char = '|';

/// Tag and detail of `outcome`.
pub(super) fn outcome_tag(outcome: &Outcome) -> (String, Option<String>) {
    let plain = |tag: &str| (tag.to_string(), None);
    match outcome {
        Outcome::Completed { folder } => (
            "done".to_string(),
            folder.as_ref().map(|f| f.display().to_string()),
        ),
        Outcome::Cancelled { by_peer: false } => plain("cancelled"),
        Outcome::Cancelled { by_peer: true } => plain("cancelled-there"),
        Outcome::Refused(reason) => reason_tag(REFUSED, reason),
        Outcome::Failed(Failure::Cause(reason)) => reason_tag(FAILED, reason),
        Outcome::Failed(Failure::NoLongerAllowed) => plain(NO_LONGER_ALLOWED),
        Outcome::Failed(Failure::Listing(error)) => listing_tag(error),
        Outcome::Interrupted => plain("interrupted"),
    }
}

/// The outcome a tag and its detail name; `None` for an unknown tag.
pub(super) fn outcome_of(tag: &str, detail: &str) -> Option<Outcome> {
    Some(match tag {
        "done" => Outcome::Completed {
            folder: (!detail.is_empty()).then(|| PathBuf::from(detail)),
        },
        "cancelled" => Outcome::Cancelled { by_peer: false },
        "cancelled-there" => Outcome::Cancelled { by_peer: true },
        NO_LONGER_ALLOWED => Outcome::Failed(Failure::NoLongerAllowed),
        "interrupted" => Outcome::Interrupted,
        _ => {
            if let Some(error) = listing_of(tag, detail) {
                Outcome::Failed(Failure::Listing(error))
            } else if let Some(reason) = reason_of(REFUSED, tag, detail) {
                Outcome::Refused(reason)
            } else {
                Outcome::Failed(Failure::Cause(reason_of(FAILED, tag, detail)?))
            }
        }
    })
}

/// Tag and detail of `reason` in a line of `prefix` (`refused` or
/// `failed`).
fn reason_tag(prefix: &str, reason: &TransferError) -> (String, Option<String>) {
    let (suffix, detail) = match reason {
        TransferError::NotAllowed => ("-not-allowed", None),
        TransferError::Invalid => ("-invalid", None),
        TransferError::Unavailable => ("-unavailable", None),
        TransferError::TooManyTransfers => ("-too-many", None),
        TransferError::NoFreeName => ("-no-name", None),
        TransferError::CannotCreateFolders(detail) => ("-folders", Some(detail)),
        TransferError::PlanningFailed(detail) => ("-planning", Some(detail)),
        TransferError::StreamFailed(detail) => ("-stream", Some(detail)),
        TransferError::CannotReadFile(detail) => ("-read", Some(detail)),
        TransferError::FileShrank => ("-shrank", None),
        TransferError::CannotCreateFile(detail) => ("-create", Some(detail)),
        TransferError::CannotWriteFile(detail) => ("-write", Some(detail)),
        TransferError::CannotSaveFile(detail) => ("-save", Some(detail)),
        TransferError::TooMuchData => ("-too-much-data", None),
        TransferError::Incomplete => ("-incomplete", None),
        TransferError::NotInList => ("-not-in-list", None),
        TransferError::NotSent => ("-not-sent", None),
        TransferError::NotAsked => ("-not-asked", None),
    };
    (format!("{prefix}{suffix}"), detail.cloned())
}

/// The reason a line of `prefix` names; `None` if `tag` is not one of its.
fn reason_of(prefix: &str, tag: &str, detail: &str) -> Option<TransferError> {
    let detail = detail.to_string();
    Some(match tag.strip_prefix(prefix)? {
        "-not-allowed" => TransferError::NotAllowed,
        "-invalid" => TransferError::Invalid,
        "-unavailable" => TransferError::Unavailable,
        "-too-many" => TransferError::TooManyTransfers,
        "-no-name" => TransferError::NoFreeName,
        "-folders" => TransferError::CannotCreateFolders(detail),
        "-planning" => TransferError::PlanningFailed(detail),
        "-stream" => TransferError::StreamFailed(detail),
        "-read" => TransferError::CannotReadFile(detail),
        "-shrank" => TransferError::FileShrank,
        "-create" => TransferError::CannotCreateFile(detail),
        "-write" => TransferError::CannotWriteFile(detail),
        "-save" => TransferError::CannotSaveFile(detail),
        "-too-much-data" => TransferError::TooMuchData,
        "-incomplete" => TransferError::Incomplete,
        "-not-in-list" => TransferError::NotInList,
        "-not-sent" => TransferError::NotSent,
        "-not-asked" => TransferError::NotAsked,
        _ => return None,
    })
}

/// Tag and detail of files that could not be listed. The detail starts
/// with the path, when there is one, then [`PATH_END`] and the rest.
fn listing_tag(error: &ListingError) -> (String, Option<String>) {
    let path_and =
        |path: &std::path::Path, rest: &str| Some(format!("{}{PATH_END}{rest}", path.display()));
    let path_only = |path: &std::path::Path| Some(path.display().to_string());
    let (suffix, detail) = match error {
        ListingError::Nothing => ("nothing", None),
        ListingError::TooMany => ("too-many", None),
        ListingError::Unnamed(path) => ("unnamed", path_only(path)),
        ListingError::Unreadable { path, detail } => ("unreadable", path_and(path, detail)),
        ListingError::Unsendable { path, reason } => match reason {
            PathError::Empty => ("empty-path", path_only(path)),
            PathError::TooLong => ("long-path", path_only(path)),
            PathError::NotRelative => ("not-relative", path_only(path)),
            PathError::BadName(name) => ("bad-name", path_and(path, name)),
        },
    };
    (format!("{LISTING}-{suffix}"), detail)
}

/// The listing error a tag names; `None` if `tag` is not one of them.
fn listing_of(tag: &str, detail: &str) -> Option<ListingError> {
    let suffix = tag.strip_prefix(LISTING)?.strip_prefix('-')?;
    let path = || PathBuf::from(detail);
    let path_and = || {
        let (path, rest) = detail.split_once(PATH_END)?;
        Some((PathBuf::from(path), rest.to_string()))
    };
    let unsendable = |reason| ListingError::Unsendable {
        path: path(),
        reason,
    };
    Some(match suffix {
        "nothing" => ListingError::Nothing,
        "too-many" => ListingError::TooMany,
        "unnamed" => ListingError::Unnamed(path()),
        "unreadable" => {
            let (path, detail) = path_and()?;
            ListingError::Unreadable { path, detail }
        }
        "empty-path" => unsendable(PathError::Empty),
        "long-path" => unsendable(PathError::TooLong),
        "not-relative" => unsendable(PathError::NotRelative),
        "bad-name" => {
            let (path, name) = path_and()?;
            ListingError::Unsendable {
                path,
                reason: PathError::BadName(name),
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(outcome: Outcome) -> Option<Outcome> {
        let (tag, detail) = outcome_tag(&outcome);
        outcome_of(&tag, detail.as_deref().unwrap_or_default())
    }

    #[test]
    fn every_cause_of_a_file_survives_its_tag() {
        let text = || "disco: pieno | quasi".to_string();
        for reason in [
            TransferError::StreamFailed(text()),
            TransferError::CannotReadFile(text()),
            TransferError::FileShrank,
            TransferError::CannotCreateFile(text()),
            TransferError::CannotWriteFile(text()),
            TransferError::CannotSaveFile(text()),
            TransferError::TooMuchData,
            TransferError::Incomplete,
            TransferError::NotInList,
            TransferError::NotSent,
            TransferError::NotAsked,
        ] {
            for outcome in [
                Outcome::Failed(Failure::Cause(reason.clone())),
                Outcome::Refused(reason),
            ] {
                assert_eq!(round_trip(outcome.clone()), Some(outcome));
            }
        }
    }

    #[test]
    fn every_listing_error_survives_its_tag() {
        let path = || PathBuf::from("C:\\Foto\\a: b.jpg");
        for error in [
            ListingError::Nothing,
            ListingError::TooMany,
            ListingError::Unnamed(PathBuf::from("C:\\")),
            ListingError::Unreadable {
                path: path(),
                detail: "accesso negato | 5".into(),
            },
            ListingError::Unsendable {
                path: path(),
                reason: PathError::Empty,
            },
            ListingError::Unsendable {
                path: path(),
                reason: PathError::TooLong,
            },
            ListingError::Unsendable {
                path: path(),
                reason: PathError::NotRelative,
            },
            ListingError::Unsendable {
                path: path(),
                reason: PathError::BadName("a|b?".into()),
            },
        ] {
            let outcome = Outcome::Failed(Failure::Listing(error));
            assert_eq!(round_trip(outcome.clone()), Some(outcome));
        }
    }

    #[test]
    fn the_tags_of_the_new_causes_are_fixed() {
        let tag = |outcome| outcome_tag(&outcome);
        assert_eq!(
            tag(Outcome::Failed(Failure::Cause(TransferError::Incomplete))),
            ("failed-incomplete".to_string(), None)
        );
        assert_eq!(
            tag(Outcome::Failed(Failure::Cause(
                TransferError::CannotCreateFile("negato".into())
            ))),
            ("failed-create".to_string(), Some("negato".to_string()))
        );
        assert_eq!(
            tag(Outcome::Failed(Failure::Listing(ListingError::Nothing))),
            ("failed-listing-nothing".to_string(), None)
        );
        assert_eq!(
            tag(Outcome::Failed(Failure::Listing(
                ListingError::Unsendable {
                    path: PathBuf::from("C:\\a"),
                    reason: PathError::BadName("x?".into()),
                }
            ))),
            (
                "failed-listing-bad-name".to_string(),
                Some("C:\\a|x?".to_string())
            )
        );
    }

    #[test]
    fn a_listing_tag_without_its_path_is_not_an_outcome() {
        assert_eq!(outcome_of("failed-listing-unreadable", "C:\\a"), None);
        assert_eq!(outcome_of("failed-listing-bad-name", "C:\\a"), None);
        assert_eq!(outcome_of("failed-listing-other", ""), None);
        assert_eq!(outcome_of("failed", "testo inglese"), None);
    }
}
