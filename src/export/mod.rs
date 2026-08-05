//! Framework-free export domain: the zips a Snapchat "My Data" dump arrives as and the json they
//! hold, read off disk and turned into types the rest of the crate can trust.
//!
//! [`self::zip`] finds the `mydata~*` parts and unpacks them. [`schema`] transcribes the wire,
//! [`model`] validates it. [`ExportJson`] is the whole `json/` dir in one value; the six files
//! phases 2-4 build on arrive as `model` types, the other thirteen as typed [`schema`]
//! passthroughs until a screen needs more from them. [`env`] covers the other half of what a run
//! depends on: the optional tools installed and the room left on disk. [`memories`] joins the
//! media on disk to the entries `memories_history.json` names and enrolls the result in
//! [`manifest`].
//!
//! Phase 2's local-fix leg builds on that: [`overlay`] composites a memory's caption layer back
//! over it, [`timezone`] turns its coordinates into the offset local clocks were at, [`exif`]
//! writes the result into the image (and owns the guard type that keeps `little_exif` on its one
//! safe path), [`video`] does the same job for an MP4's container metadata (and owns the guard type
//! that keeps `mp4ameta` off its chapter legs), [`ffmpeg`] is the only thing that touches video
//! pixels, and [`local_fix`] is the pass that drives all of them and records the outcome in
//! [`manifest`].
//!
//! Phase 3 starts at [`chat_media`], which does for a `chat_media` dir what [`memories`] does for a
//! `memories` one and shares the directory walk both need. What it does NOT share is the join: a
//! chat-media filename carries an id, so the pairing is a stem match and the history join is a
//! string equality, with none of the date bucketing memories has to fall back on.

pub mod chat_media;
pub mod env;
pub mod exif;
pub mod ffmpeg;
pub mod local_fix;
pub mod manifest;
pub mod memories;
pub mod memories_run;
pub mod model;
pub mod overlay;
pub mod schema;
pub mod timezone;
pub mod video;
mod walk;
pub mod zip;

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::error::Category;

use crate::export::model::ParseError;

/// The union of every schema filename seen in a real export's `json/` dir, across two observed
/// exports (2026-07-26 and 2026-08-04).
///
/// Each export held 19 files, but the SETS differed by two names: the second dropped
/// `memories_history.json` and added `in_app_reports.json`, both present below. Membership is
/// decided by which data categories the user ticked when requesting the export, so this list is a
/// union of observations, never a contract, and a third export can both drop a name already here
/// and add one neither export has shown. [`read_schema`] already treats every file as optional, so
/// a name from this list missing off a given export's `json/` dir is expected, not a failure. It is
/// mirrored by the redactor's `test_every_real_export_schema_filename_survives_verbatim`, which
/// pins the same union. `tests/export.rs`'s
/// `schema_files_and_the_redactors_real_schema_filenames_agree` cross-checks the two lists against
/// each other, since a pin on each side alone does not catch a name landing on only one of them.
pub const SCHEMA_FILES: [&str; 20] = [
    "account.json",
    "account_history.json",
    "bitmoji.json",
    "chat_history.json",
    "custom_sticker.json",
    "email_campaign_history.json",
    "feature_emails.json",
    "friends.json",
    "in_app_reports.json",
    "location_history.json",
    "memories_history.json",
    "ranking.json",
    "snap_ads.json",
    "snap_history.json",
    "snap_pro.json",
    "snapchat_ai.json",
    "snapchat_plus.json",
    "story_history.json",
    "terms_history.json",
    "user_profile.json",
];

/// Something went wrong getting a file off disk and into a type: the export did not arrive as
/// expected. Distinct from [`ParseError`], which means the export arrived and one of its values
/// is unusable.
#[derive(Debug)]
pub enum LoadError {
    Io { file: &'static str, source: io::Error },
    Json { file: &'static str, source: serde_json::Error },
    Invalid { file: &'static str, source: ParseError },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { file, source } => {
                write!(f, "could not read {file} from the export's json dir: {source}")
            }
            // `serde_json::Error` covers two failures with opposite fixes. Broken bytes are worth
            // re-extracting; a shape this build does not expect never is, and telling someone to
            // re-unzip a perfectly good file sends them in a circle.
            Self::Json { file, source } => match source.classify() {
                Category::Syntax | Category::Eof | Category::Io => {
                    write!(f, "{file} is not valid json ({source}); re-extract the export part holding json/")
                }
                Category::Data => write!(
                    f,
                    "{file} is valid json in a shape this build does not know, at line {} column {} ({source}); \
                     the export's schema has moved, so this needs a parser update rather than another extraction",
                    source.line(),
                    source.column()
                ),
            },
            Self::Invalid { file, source } => write!(f, "{file}: {source}"),
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Invalid { source, .. } => Some(source),
        }
    }
}

/// A whole `mydata~<id>/json/` dir, parsed.
///
/// Every field is optional because a file Snapchat omits for a given user (nobody has a
/// `snap_ads.json` worth shipping without a business account) must not fail the load. A file
/// that is present and broken still does.
#[derive(Debug)]
pub struct ExportJson {
    // Modelled: the files phases 2-4 build on.
    pub account: Option<model::Account>,
    pub chat_history: Option<model::ChatHistory>,
    pub friends: Option<model::Friends>,
    pub memories: Option<model::Memories>,
    pub snap_history: Option<model::SnapHistory>,
    pub user_profile: Option<model::UserProfile>,

    // Typed passthroughs: parsed and held, no domain type until a screen needs one.
    pub account_history: Option<schema::AccountHistory>,
    pub bitmoji: Option<schema::Bitmoji>,
    pub custom_sticker: Option<schema::CustomSticker>,
    pub email_campaign_history: Option<schema::EmailCampaignHistory>,
    pub feature_emails: Option<schema::FeatureEmails>,
    pub location_history: Option<schema::LocationHistory>,
    pub ranking: Option<schema::Ranking>,
    pub snap_ads: Option<schema::SnapAds>,
    pub snap_pro: Option<schema::SnapPro>,
    pub snapchat_ai: Option<schema::SnapchatAi>,
    pub snapchat_plus: Option<schema::SnapchatPlus>,
    pub story_history: Option<schema::StoryHistory>,
    pub terms_history: Option<schema::TermsHistory>,
}

impl ExportJson {
    /// Reads and parses every file in `json_dir`.
    ///
    /// Fail-fast: the first present-but-unusable file stops the load and the error names it. A
    /// missing file is not a failure, it lands as `None`.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError`] when a file cannot be read, does not hold json, or holds a value
    /// [`model`] cannot validate.
    pub fn load_dir(json_dir: impl AsRef<Path>) -> Result<Self, LoadError> {
        let dir = json_dir.as_ref();
        Ok(Self {
            account: read_model::<schema::Account, _>(dir, "account.json")?,
            chat_history: read_model::<schema::ChatHistory, _>(dir, "chat_history.json")?,
            friends: read_model::<schema::Friends, _>(dir, "friends.json")?,
            memories: read_model::<schema::MemoriesHistory, _>(dir, "memories_history.json")?,
            snap_history: read_model::<schema::SnapHistory, _>(dir, "snap_history.json")?,
            user_profile: read_model::<schema::UserProfile, _>(dir, "user_profile.json")?,

            account_history: read_schema(dir, "account_history.json")?,
            bitmoji: read_schema(dir, "bitmoji.json")?,
            custom_sticker: read_schema(dir, "custom_sticker.json")?,
            email_campaign_history: read_schema(dir, "email_campaign_history.json")?,
            feature_emails: read_schema(dir, "feature_emails.json")?,
            location_history: read_schema(dir, "location_history.json")?,
            ranking: read_schema(dir, "ranking.json")?,
            snap_ads: read_schema(dir, "snap_ads.json")?,
            snap_pro: read_schema(dir, "snap_pro.json")?,
            snapchat_ai: read_schema(dir, "snapchat_ai.json")?,
            snapchat_plus: read_schema(dir, "snapchat_plus.json")?,
            story_history: read_schema(dir, "story_history.json")?,
            terms_history: read_schema(dir, "terms_history.json")?,
        })
    }
}

fn read_schema<T: DeserializeOwned>(dir: &Path, file: &'static str) -> Result<Option<T>, LoadError> {
    let bytes = match fs::read(dir.join(file)) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(LoadError::Io { file, source }),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|source| LoadError::Json { file, source })
}

fn read_model<S, M>(dir: &Path, file: &'static str) -> Result<Option<M>, LoadError>
where
    S: DeserializeOwned,
    M: TryFrom<S, Error = ParseError>,
{
    read_schema::<S>(dir, file)?.map(|raw| M::try_from(raw).map_err(|source| LoadError::Invalid { file, source })).transpose()
}
