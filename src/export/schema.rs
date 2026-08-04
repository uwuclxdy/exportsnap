//! Serde mirrors of the json files a Snapchat export can hold under `mydata~<id>/json/`. Not
//! every name is mirrored: [`crate::export::SCHEMA_FILES`] is the union across every export
//! observed so far, and `in_app_reports.json` has no struct here yet.
//!
//! One struct per file, named after the file. This layer is a transcription, not a domain: it
//! holds whatever the wire holds, and [`crate::export::model`] is where values become validated
//! types.
//!
//! Three conventions run through the whole file.
//!
//! Every field carries `#[serde(rename)]` with the wire key verbatim. The keys are Title Case
//! with spaces (and one is the empty string), which no `rename_all` rule produces, so a
//! per-field rename is the only spelling available. Those literals are Snapchat's published
//! on-disk keys, not Rust identifiers — never touch one with a mechanical find-and-replace.
//!
//! Every struct carries `#[serde(default)]` and none carries `deny_unknown_fields`. The schema
//! is observed from ONE real export (`docs/design.md`, "Observed export shape"), so a section
//! Snapchat drops for a given user must arrive empty rather than fatal, and a section this
//! parser has never seen must not fail the load.
//!
//! Sections that are empty in the one observed export keep `serde_json::Value` elements. Their
//! real element shape is unknown, and guessing one would turn a passthrough nobody reads yet
//! into a hard parse failure on the first export that populates it.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

// ---- account.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Account {
    #[serde(rename = "Basic Information")]
    pub basic_information: AccountBasicInformation,
    #[serde(rename = "Device Information")]
    pub device_information: DeviceInformation,
    #[serde(rename = "Device History")]
    pub device_history: Vec<DeviceHistoryEntry>,
    #[serde(rename = "Privacy Policy and Terms of Service Acceptance History")]
    pub privacy_policy_acceptance_history: Vec<Value>,
    #[serde(rename = "Custom Creative Tools Terms")]
    pub custom_creative_tools_terms: Vec<Value>,
    #[serde(rename = "Login History")]
    pub login_history: Vec<LoginHistoryEntry>,
    #[serde(rename = "Family Center")]
    pub family_center: Vec<Value>,
    #[serde(rename = "Associated Accounts by Cloud Account ID")]
    pub associated_accounts: Vec<AssociatedAccount>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AccountBasicInformation {
    #[serde(rename = "Username")]
    pub username: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Creation Date")]
    pub creation_date: String,
    #[serde(rename = "Registration IP")]
    pub registration_ip: String,
    #[serde(rename = "Country")]
    pub country: String,
    #[serde(rename = "Last Active")]
    pub last_active: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DeviceInformation {
    #[serde(rename = "Make")]
    pub make: String,
    #[serde(rename = "Model ID")]
    pub model_id: String,
    #[serde(rename = "Model Name")]
    pub model_name: String,
    #[serde(rename = "Language")]
    pub language: String,
    #[serde(rename = "OS Type")]
    pub os_type: String,
    #[serde(rename = "OS Version")]
    pub os_version: String,
    #[serde(rename = "Connection Type")]
    pub connection_type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DeviceHistoryEntry {
    #[serde(rename = "Make")]
    pub make: String,
    #[serde(rename = "Model")]
    pub model: String,
    #[serde(rename = "Start Time")]
    pub start_time: String,
    #[serde(rename = "Device Type")]
    pub device_type: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LoginHistoryEntry {
    #[serde(rename = "IP")]
    pub ip: String,
    #[serde(rename = "Country")]
    pub country: String,
    #[serde(rename = "Created")]
    pub created: String,
    #[serde(rename = "Status")]
    pub status: String,
    #[serde(rename = "Device")]
    pub device: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AssociatedAccount {
    #[serde(rename = "Device ID")]
    pub device_id: String,
    #[serde(rename = "User ID")]
    pub user_id: String,
    #[serde(rename = "Request Time")]
    pub request_time: String,
    #[serde(rename = "Approximate Last Seen")]
    pub approximate_last_seen: String,
}

// ---- account_history.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AccountHistory {
    #[serde(rename = "Display Name Change")]
    pub display_name_change: Vec<DisplayNameChange>,
    #[serde(rename = "Email Change")]
    pub email_change: Vec<EmailChange>,
    #[serde(rename = "Mobile Number Change")]
    pub mobile_number_change: Vec<MobileNumberChange>,
    #[serde(rename = "Password Change")]
    pub password_change: Vec<PasswordChange>,
    #[serde(rename = "Snapchat Linked to Bitmoji")]
    pub linked_to_bitmoji: Vec<Value>,
    #[serde(rename = "Spectacles")]
    pub spectacles: Vec<Value>,
    #[serde(rename = "Two-Factor Authentication")]
    pub two_factor_authentication: Vec<Value>,
    #[serde(rename = "Account deactivated / reactivated")]
    pub deactivated_or_reactivated: Vec<Value>,
    #[serde(rename = "Download My Data Reports")]
    pub download_my_data_reports: Vec<DownloadMyDataReport>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DisplayNameChange {
    #[serde(rename = "Date")]
    pub date: String,
    #[serde(rename = "Display Name")]
    pub display_name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EmailChange {
    #[serde(rename = "Date")]
    pub date: String,
    #[serde(rename = "Email Address")]
    pub email_address: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MobileNumberChange {
    #[serde(rename = "Date")]
    pub date: String,
    #[serde(rename = "Mobile Number")]
    pub mobile_number: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PasswordChange {
    #[serde(rename = "Date")]
    pub date: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DownloadMyDataReport {
    #[serde(rename = "Date")]
    pub date: String,
    #[serde(rename = "Status")]
    pub status: String,
    #[serde(rename = "Email Address")]
    pub email_address: String,
}

// ---- bitmoji.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Bitmoji {
    #[serde(rename = "Basic Information")]
    pub basic_information: BitmojiBasicInformation,
    #[serde(rename = "Analytics")]
    pub analytics: BitmojiAnalytics,
    #[serde(rename = "Terms of Service Acceptance History")]
    pub terms_acceptance_history: Vec<Value>,
    #[serde(rename = "Search History")]
    pub search_history: Vec<Value>,
    #[serde(rename = "Support Cases")]
    pub support_cases: Vec<Value>,
    #[serde(rename = "Selfies")]
    pub selfies: Vec<Value>,
    #[serde(rename = "Keyboard Enable Full Access History (iOS only)")]
    pub keyboard_full_access_history: Vec<Value>,
    #[serde(rename = "Connected Apps")]
    pub connected_apps: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BitmojiBasicInformation {
    #[serde(rename = "First Name")]
    pub first_name: String,
    #[serde(rename = "Last Name")]
    pub last_name: String,
    #[serde(rename = "Email")]
    pub email: String,
    #[serde(rename = "Phone Number")]
    pub phone_number: String,
    #[serde(rename = "Account Creation Date")]
    pub account_creation_date: String,
    #[serde(rename = "Account Creation User Agent")]
    pub account_creation_user_agent: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BitmojiAnalytics {
    #[serde(rename = "App Open Count")]
    pub app_open_count: u64,
    #[serde(rename = "Avatar Gender")]
    pub avatar_gender: String,
    #[serde(rename = "Outfit Save Count")]
    pub outfit_save_count: u64,
    #[serde(rename = "Share Count")]
    pub share_count: u64,
}

// ---- chat_history.json ----

/// Keyed by conversation: a friend's username for a one-to-one thread, a uuid for a group.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct ChatHistory {
    pub conversations: BTreeMap<String, Vec<ChatEntry>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ChatEntry {
    #[serde(rename = "From")]
    pub from: String,
    #[serde(rename = "Media Type")]
    pub media_type: String,
    #[serde(rename = "Created")]
    pub created: String,
    #[serde(rename = "Content")]
    pub content: Option<String>,
    #[serde(rename = "Conversation Title")]
    pub conversation_title: Option<String>,
    #[serde(rename = "IsSender")]
    pub is_sender: bool,
    /// The key says microseconds; see [`crate::export::model::ChatMessage::created_epoch_ms`].
    #[serde(rename = "Created(microseconds)")]
    pub created_epoch: i64,
    #[serde(rename = "IsSaved")]
    pub is_saved: bool,
    #[serde(rename = "Media IDs")]
    pub media_ids: String,
}

// ---- custom_sticker.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CustomSticker {
    #[serde(rename = "My Custom Stickers")]
    pub my_custom_stickers: Vec<CustomStickerEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CustomStickerEntry {
    #[serde(rename = "Created")]
    pub created: String,
    #[serde(rename = "Sticker ID")]
    pub sticker_id: String,
    #[serde(rename = "Content")]
    pub content: String,
}

// ---- email_campaign_history.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EmailCampaignHistory {
    #[serde(rename = "Email Campaign Subscriptions")]
    pub subscriptions: Vec<EmailCampaignSubscription>,
    #[serde(rename = "Email Campaign History")]
    pub history: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EmailCampaignSubscription {
    #[serde(rename = "Email Campaign")]
    pub email_campaign: String,
    #[serde(rename = "Opt Out Status")]
    pub opt_out_status: String,
}

// ---- feature_emails.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FeatureEmails {
    #[serde(rename = "Email Used to Join")]
    pub email_used_to_join: Vec<Value>,
}

// ---- friends.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Friends {
    #[serde(rename = "Friends")]
    pub friends: Vec<FriendEntry>,
    #[serde(rename = "Friend Requests Sent")]
    pub friend_requests_sent: Vec<FriendEntry>,
    #[serde(rename = "Blocked Users")]
    pub blocked_users: Vec<FriendEntry>,
    #[serde(rename = "Deleted Friends")]
    pub deleted_friends: Vec<FriendEntry>,
    #[serde(rename = "Hidden Friend Suggestions")]
    pub hidden_friend_suggestions: Vec<FriendEntry>,
    #[serde(rename = "Ignored Snapchatters")]
    pub ignored_snapchatters: Vec<FriendEntry>,
    #[serde(rename = "Pending Requests")]
    pub pending_requests: Vec<FriendEntry>,
    #[serde(rename = "Shortcuts")]
    pub shortcuts: Vec<ShortcutEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FriendEntry {
    #[serde(rename = "Username")]
    pub username: String,
    #[serde(rename = "Display Name")]
    pub display_name: String,
    #[serde(rename = "Creation Timestamp")]
    pub creation_timestamp: String,
    #[serde(rename = "Last Modified Timestamp")]
    pub last_modified_timestamp: String,
    #[serde(rename = "Source")]
    pub source: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ShortcutEntry {
    #[serde(rename = "Shortcut Name")]
    pub shortcut_name: String,
    #[serde(rename = "Created")]
    pub created: String,
}

// ---- location_history.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct LocationHistory {
    #[serde(rename = "Frequent Locations")]
    pub frequent_locations: Vec<PlaceEntry>,
    #[serde(rename = "Latest Location")]
    pub latest_location: Vec<PlaceEntry>,
    /// Keyed by place role, not by a fixed set of fields.
    #[serde(rename = "Home, School & Work")]
    pub home_school_work: BTreeMap<String, String>,
    #[serde(rename = "Daily Top Locations")]
    pub daily_top_locations: Vec<Value>,
    #[serde(rename = "Top Locations Per Six-Day Period")]
    pub top_locations_per_six_day_period: Vec<Value>,
    #[serde(rename = "Location History")]
    pub location_history: Vec<Value>,
    /// Keyed by an opaque 22-character business id, so this is a map and not a fixed-shape
    /// container (`docs/design.md`, "Observed export shape").
    #[serde(rename = "Businesses and places you may have visited")]
    pub businesses_visited: BTreeMap<String, Vec<Value>>,
    #[serde(rename = "Actiomoji information from places you may have visited")]
    pub actiomoji_information: Vec<Value>,
    #[serde(rename = "Areas you may have visited in the last two years")]
    pub areas_visited: Vec<AreaEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PlaceEntry {
    #[serde(rename = "City")]
    pub city: String,
    #[serde(rename = "Country")]
    pub country: String,
    #[serde(rename = "Region")]
    pub region: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AreaEntry {
    #[serde(rename = "Time")]
    pub time: String,
    #[serde(rename = "City")]
    pub city: String,
    #[serde(rename = "Region")]
    pub region: String,
    #[serde(rename = "Postal Code")]
    pub postal_code: String,
}

// ---- memories_history.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MemoriesHistory {
    #[serde(rename = "Saved Media")]
    pub saved_media: Vec<SavedMediaEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SavedMediaEntry {
    #[serde(rename = "Date")]
    pub date: String,
    #[serde(rename = "Media Type")]
    pub media_type: String,
    #[serde(rename = "Location")]
    pub location: String,
    #[serde(rename = "Download Link")]
    pub download_link: String,
    #[serde(rename = "Media Download Url")]
    pub media_download_url: String,
}

// ---- ranking.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Ranking {
    #[serde(rename = "Statistics")]
    pub statistics: RankingStatistics,
    /// Heterogeneous in the one observed export (an integer followed by an object), so its
    /// elements stay untyped.
    #[serde(rename = "Spotlight")]
    pub spotlight: Vec<Value>,
}

/// Snapchat ships these counts as strings, not numbers.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RankingStatistics {
    #[serde(rename = "Snapscore")]
    pub snapscore: String,
    #[serde(rename = "Your Total Friends")]
    pub total_friends: String,
    #[serde(rename = "The Number of Accounts You Follow")]
    pub accounts_followed: String,
}

// ---- snap_ads.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SnapAds {
    #[serde(rename = "Organization Members")]
    pub organization_members: Vec<OrganizationMember>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OrganizationMember {
    #[serde(rename = "Display Name")]
    pub display_name: String,
    #[serde(rename = "Invitation E-mail Address")]
    pub invitation_email_address: String,
    #[serde(rename = "Organization Name")]
    pub organization_name: String,
    #[serde(rename = "Contact Name")]
    pub contact_name: String,
    #[serde(rename = "Contact Phone Number")]
    pub contact_phone_number: String,
    #[serde(rename = "Contact E-mail Address")]
    pub contact_email_address: String,
    #[serde(rename = "Active Roles")]
    pub active_roles: String,
}

// ---- snap_history.json ----

/// Keyed the same way as [`ChatHistory`], and the ids join across the two files.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct SnapHistory {
    pub conversations: BTreeMap<String, Vec<SnapEntry>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SnapEntry {
    #[serde(rename = "From")]
    pub from: String,
    #[serde(rename = "Media Type")]
    pub media_type: String,
    #[serde(rename = "Created")]
    pub created: String,
    #[serde(rename = "Conversation Title")]
    pub conversation_title: Option<String>,
    #[serde(rename = "IsSender")]
    pub is_sender: bool,
    /// The key says microseconds; see [`crate::export::model::Snap::created_epoch_ms`].
    #[serde(rename = "Created(microseconds)")]
    pub created_epoch: i64,
}

// ---- snap_pro.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SnapPro {
    #[serde(rename = "Profile")]
    pub profile: SnapProProfile,
    #[serde(rename = "Spotlights")]
    pub spotlights: Vec<Value>,
    #[serde(rename = "Saved Stories")]
    pub saved_stories: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SnapProProfile {
    #[serde(rename = "Created")]
    pub created: String,
    #[serde(rename = "Profile Title")]
    pub profile_title: String,
    #[serde(rename = "Location")]
    pub location: String,
    #[serde(rename = "Profile Website")]
    pub profile_website: String,
    #[serde(rename = "Profile Photo")]
    pub profile_photo: String,
    #[serde(rename = "Hero Image")]
    pub hero_image: String,
}

// ---- snapchat_ai.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SnapchatAi {
    #[serde(rename = "My AI Content")]
    pub my_ai_content: Vec<AiContentEntry>,
    #[serde(rename = "My AI Memory")]
    pub my_ai_memory: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AiContentEntry {
    #[serde(rename = "Timestamp")]
    pub timestamp: String,
    #[serde(rename = "IP Address")]
    pub ip_address: String,
    #[serde(rename = "Type")]
    pub kind: String,
    #[serde(rename = "Content")]
    pub content: String,
}

// ---- snapchat_plus.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SnapchatPlus {
    /// The export's only unnamed section: the wire key really is the empty string.
    #[serde(rename = "")]
    pub subscriptions: Vec<SubscriptionEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SubscriptionEntry {
    #[serde(rename = "Purchase Date")]
    pub purchase_date: String,
    #[serde(rename = "Purchase Type")]
    pub purchase_type: String,
    #[serde(rename = "Purchase Provider")]
    pub purchase_provider: String,
    /// The only float leaf in the whole export (`docs/design.md`, "Observed export shape").
    #[serde(rename = "Price")]
    pub price: f64,
    #[serde(rename = "End Date (if applicable)")]
    pub end_date: String,
}

// ---- story_history.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StoryHistory {
    #[serde(rename = "Your Story Views")]
    pub your_story_views: Vec<StoryViewEntry>,
    #[serde(rename = "Friend and Public Story Views")]
    pub friend_and_public_story_views: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct StoryViewEntry {
    #[serde(rename = "Story Date")]
    pub story_date: String,
    #[serde(rename = "Story Views")]
    pub story_views: u64,
    #[serde(rename = "Story Replies")]
    pub story_replies: u64,
}

// ---- terms_history.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TermsHistory {
    #[serde(rename = "Terms of Service and Privacy Policy Acceptance History")]
    pub terms_acceptance_history: Vec<TermsAcceptance>,
    #[serde(rename = "Business Services Terms")]
    pub business_services_terms: Vec<Value>,
    #[serde(rename = "Custom Creative Tools Terms")]
    pub custom_creative_tools_terms: Vec<Value>,
    #[serde(rename = "Spectacles User Agreement")]
    pub spectacles_user_agreement: Vec<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TermsAcceptance {
    #[serde(rename = "Version")]
    pub version: String,
    #[serde(rename = "Acceptance Date")]
    pub acceptance_date: String,
}

// ---- user_profile.json ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UserProfile {
    #[serde(rename = "App Profile")]
    pub app_profile: AppProfile,
    #[serde(rename = "Demographics")]
    pub demographics: Demographics,
    #[serde(rename = "Subscriptions")]
    pub subscriptions: Vec<Value>,
    #[serde(rename = "Engagement")]
    pub engagement: Vec<EngagementEntry>,
    #[serde(rename = "Discover Channels Viewed")]
    pub discover_channels_viewed: Vec<Value>,
    #[serde(rename = "Breakdown of Time Spent on App")]
    pub time_spent_breakdown: Vec<String>,
    #[serde(rename = "Ads You Interacted With")]
    pub ads_interacted_with: Vec<Value>,
    #[serde(rename = "Interest Categories")]
    pub interest_categories: Vec<Value>,
    #[serde(rename = "Content Categories")]
    pub content_categories: Vec<Value>,
    #[serde(rename = "Geographic Information")]
    pub geographic_information: Vec<Value>,
    #[serde(rename = "Interactions")]
    pub interactions: Interactions,
    #[serde(rename = "Off-Platform Sharing")]
    pub off_platform_sharing: Vec<Value>,
    #[serde(rename = "Mobile Ad Id")]
    pub mobile_ad_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AppProfile {
    #[serde(rename = "Country")]
    pub country: String,
    #[serde(rename = "Creation Time")]
    pub creation_time: String,
    #[serde(rename = "Account Creation Country")]
    pub account_creation_country: String,
    #[serde(rename = "Platform Version")]
    pub platform_version: String,
    #[serde(rename = "In-app Language")]
    pub in_app_language: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Demographics {
    #[serde(rename = "Cohort Age")]
    pub cohort_age: String,
    #[serde(rename = "Derived Ad Demographic")]
    pub derived_ad_demographic: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EngagementEntry {
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "Occurrences")]
    pub occurrences: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Interactions {
    #[serde(rename = "Web Interactions")]
    pub web: Vec<String>,
    /// Untyped for the reason at the top of this file: it is `[]` in the one observed export.
    /// The sibling `Web Interactions` being a string list is not evidence about this key, and
    /// `ExportJson::load_dir` is fail-fast, so a wrong guess here would fail the WHOLE export
    /// load on the first user whose app interactions are populated.
    #[serde(rename = "App Interactions")]
    pub app: Vec<Value>,
}
