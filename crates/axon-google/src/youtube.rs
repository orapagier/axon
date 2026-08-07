use crate::auth::access_token;
use anyhow::{anyhow, bail, Result};
use axon_core::{AppState, EnsureOk};
use rmcp::model::Tool;
use serde_json::{json, Map, Value};
use std::sync::Arc;

const BASE: &str = "https://www.googleapis.com/youtube/v3";
const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/youtube/v3";

struct ActionSpec {
    tool: &'static str,
    description: &'static str,
    method: &'static str,
    path: &'static str,
    requires_part: bool,
    supports_upload: bool,
    media_required: bool,
    returns_binary: bool,
    path_params: &'static [&'static str],
    required_query: &'static [&'static str],
}

const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        tool: "gyoutube_activities_list",
        description: "YouTube activities.list: list channel activity events.",
        method: "GET",
        path: "/activities",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_captions_list",
        description: "YouTube captions.list: list caption tracks for a video.",
        method: "GET",
        path: "/captions",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["videoId"],
    },
    ActionSpec {
        tool: "gyoutube_captions_insert",
        description: "YouTube captions.insert: upload a caption track.",
        method: "POST",
        path: "/captions",
        requires_part: true,
        supports_upload: true,
        media_required: true,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_captions_update",
        description: "YouTube captions.update: update caption metadata/file.",
        method: "PUT",
        path: "/captions",
        requires_part: true,
        supports_upload: true,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_captions_download",
        description: "YouTube captions.download: download caption track bytes.",
        method: "GET",
        path: "/captions/{id}",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: true,
        path_params: &["id"],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_captions_delete",
        description: "YouTube captions.delete: delete a caption track.",
        method: "DELETE",
        path: "/captions",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id"],
    },
    ActionSpec {
        tool: "gyoutube_channel_banners_insert",
        description: "YouTube channelBanners.insert: upload a channel banner image.",
        method: "POST",
        path: "/channelBanners/insert",
        requires_part: false,
        supports_upload: true,
        media_required: true,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_channels_list",
        description: "YouTube channels.list: list channels.",
        method: "GET",
        path: "/channels",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_channels_update",
        description: "YouTube channels.update: update channel metadata.",
        method: "PUT",
        path: "/channels",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_channel_sections_list",
        description: "YouTube channelSections.list: list channel sections.",
        method: "GET",
        path: "/channelSections",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_channel_sections_insert",
        description: "YouTube channelSections.insert: create channel section.",
        method: "POST",
        path: "/channelSections",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_channel_sections_update",
        description: "YouTube channelSections.update: update channel section.",
        method: "PUT",
        path: "/channelSections",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_channel_sections_delete",
        description: "YouTube channelSections.delete: delete channel section.",
        method: "DELETE",
        path: "/channelSections",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id"],
    },
    ActionSpec {
        tool: "gyoutube_comments_list",
        description: "YouTube comments.list: list comments.",
        method: "GET",
        path: "/comments",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_comments_insert",
        description: "YouTube comments.insert: create a reply comment.",
        method: "POST",
        path: "/comments",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_comments_update",
        description: "YouTube comments.update: update a comment.",
        method: "PUT",
        path: "/comments",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_comments_set_moderation_status",
        description: "YouTube comments.setModerationStatus: change moderation status for comments.",
        method: "POST",
        path: "/comments/setModerationStatus",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id", "moderationStatus"],
    },
    ActionSpec {
        tool: "gyoutube_comments_delete",
        description: "YouTube comments.delete: delete a comment.",
        method: "DELETE",
        path: "/comments",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id"],
    },
    ActionSpec {
        tool: "gyoutube_comment_threads_list",
        description: "YouTube commentThreads.list: list comment threads.",
        method: "GET",
        path: "/commentThreads",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_comment_threads_insert",
        description: "YouTube commentThreads.insert: create top-level comment.",
        method: "POST",
        path: "/commentThreads",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_i18n_languages_list",
        description: "YouTube i18nLanguages.list: list supported interface languages.",
        method: "GET",
        path: "/i18nLanguages",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_i18n_regions_list",
        description: "YouTube i18nRegions.list: list supported content regions.",
        method: "GET",
        path: "/i18nRegions",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_members_list",
        description: "YouTube members.list: list channel members.",
        method: "GET",
        path: "/members",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_memberships_levels_list",
        description: "YouTube membershipsLevels.list: list memberships levels.",
        method: "GET",
        path: "/membershipsLevels",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlist_images_list",
        description: "YouTube playlistImages.list: list playlist images.",
        method: "GET",
        path: "/playlistImages",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlist_images_insert",
        description: "YouTube playlistImages.insert: add an image to a playlist.",
        method: "POST",
        path: "/playlistImages",
        requires_part: true,
        supports_upload: true,
        media_required: true,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlist_images_update",
        description: "YouTube playlistImages.update: update playlist image metadata/media.",
        method: "PUT",
        path: "/playlistImages",
        requires_part: true,
        supports_upload: true,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlist_images_delete",
        description: "YouTube playlistImages.delete: delete a playlist image.",
        method: "DELETE",
        path: "/playlistImages",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id"],
    },
    ActionSpec {
        tool: "gyoutube_playlist_items_list",
        description: "YouTube playlistItems.list: list playlist items.",
        method: "GET",
        path: "/playlistItems",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlist_items_insert",
        description: "YouTube playlistItems.insert: add an item to a playlist.",
        method: "POST",
        path: "/playlistItems",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlist_items_update",
        description: "YouTube playlistItems.update: update playlist item.",
        method: "PUT",
        path: "/playlistItems",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlist_items_delete",
        description: "YouTube playlistItems.delete: delete a playlist item.",
        method: "DELETE",
        path: "/playlistItems",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id"],
    },
    ActionSpec {
        tool: "gyoutube_playlists_list",
        description: "YouTube playlists.list: list playlists.",
        method: "GET",
        path: "/playlists",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlists_insert",
        description: "YouTube playlists.insert: create a playlist.",
        method: "POST",
        path: "/playlists",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlists_update",
        description: "YouTube playlists.update: update playlist metadata.",
        method: "PUT",
        path: "/playlists",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_playlists_delete",
        description: "YouTube playlists.delete: delete a playlist.",
        method: "DELETE",
        path: "/playlists",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id"],
    },
    ActionSpec {
        tool: "gyoutube_search_list",
        description: "YouTube search.list: search across YouTube resources. 'q' (text query) is \
                      optional — you can also search by channelId, order (e.g. date for latest \
                      uploads), type, publishedAfter, etc.",
        method: "GET",
        path: "/search",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_subscriptions_list",
        description: "YouTube subscriptions.list: list subscriptions.",
        method: "GET",
        path: "/subscriptions",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_subscriptions_insert",
        description: "YouTube subscriptions.insert: subscribe to a channel.",
        method: "POST",
        path: "/subscriptions",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_subscriptions_delete",
        description: "YouTube subscriptions.delete: unsubscribe.",
        method: "DELETE",
        path: "/subscriptions",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id"],
    },
    ActionSpec {
        tool: "gyoutube_thumbnails_set",
        description: "YouTube thumbnails.set: upload a video thumbnail.",
        method: "POST",
        path: "/thumbnails/set",
        requires_part: false,
        supports_upload: true,
        media_required: true,
        returns_binary: false,
        path_params: &[],
        required_query: &["videoId"],
    },
    ActionSpec {
        tool: "gyoutube_video_abuse_report_reasons_list",
        description: "YouTube videoAbuseReportReasons.list: list abuse report reasons.",
        method: "GET",
        path: "/videoAbuseReportReasons",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_video_categories_list",
        description: "YouTube videoCategories.list: list video categories.",
        method: "GET",
        path: "/videoCategories",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_videos_list",
        description: "YouTube videos.list: list videos by IDs/chart/search filters.",
        method: "GET",
        path: "/videos",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_videos_insert",
        description: "YouTube videos.insert: upload a new video.",
        method: "POST",
        path: "/videos",
        requires_part: true,
        supports_upload: true,
        media_required: true,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_videos_update",
        description: "YouTube videos.update: update video metadata.",
        method: "PUT",
        path: "/videos",
        requires_part: true,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_videos_rate",
        description: "YouTube videos.rate: set a video rating.",
        method: "POST",
        path: "/videos/rate",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id", "rating"],
    },
    ActionSpec {
        tool: "gyoutube_videos_get_rating",
        description: "YouTube videos.getRating: get rating for one or more videos.",
        method: "GET",
        path: "/videos/getRating",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id"],
    },
    ActionSpec {
        tool: "gyoutube_videos_report_abuse",
        description: "YouTube videos.reportAbuse: report abusive content.",
        method: "POST",
        path: "/videos/reportAbuse",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &[],
    },
    ActionSpec {
        tool: "gyoutube_videos_delete",
        description: "YouTube videos.delete: delete a video.",
        method: "DELETE",
        path: "/videos",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["id"],
    },
    ActionSpec {
        tool: "gyoutube_watermarks_set",
        description: "YouTube watermarks.set: upload a channel watermark image.",
        method: "POST",
        path: "/watermarks/set",
        requires_part: false,
        supports_upload: true,
        media_required: true,
        returns_binary: false,
        path_params: &[],
        required_query: &["channelId"],
    },
    ActionSpec {
        tool: "gyoutube_watermarks_unset",
        description: "YouTube watermarks.unset: remove a channel watermark.",
        method: "POST",
        path: "/watermarks/unset",
        requires_part: false,
        supports_upload: false,
        media_required: false,
        returns_binary: false,
        path_params: &[],
        required_query: &["channelId"],
    },
];

/// Valid `part` values per action, best default first. List actions expose every
/// readable section; insert/update actions expose only the writable ones, because
/// YouTube rejects a write whose part list names a read-only section.
fn part_options(tool: &str) -> &'static [&'static str] {
    match tool {
        "gyoutube_activities_list" => &["snippet", "contentDetails", "id"],
        "gyoutube_captions_list" => &["snippet", "id"],
        "gyoutube_captions_insert" | "gyoutube_captions_update" => &["snippet"],
        "gyoutube_channels_list" => &[
            "snippet",
            "contentDetails",
            "statistics",
            "status",
            "brandingSettings",
            "topicDetails",
            "localizations",
            "contentOwnerDetails",
            "id",
        ],
        "gyoutube_channels_update" => &["brandingSettings", "localizations", "invideoPromotion"],
        "gyoutube_channel_sections_list" => &[
            "snippet",
            "contentDetails",
            "localizations",
            "targeting",
            "id",
        ],
        "gyoutube_channel_sections_insert" | "gyoutube_channel_sections_update" => {
            &["snippet", "contentDetails", "localizations", "targeting"]
        }
        "gyoutube_comments_list" => &["snippet", "id"],
        "gyoutube_comments_insert" | "gyoutube_comments_update" => &["snippet"],
        "gyoutube_comment_threads_list" => &["snippet", "replies", "id"],
        "gyoutube_comment_threads_insert" => &["snippet"],
        "gyoutube_i18n_languages_list" | "gyoutube_i18n_regions_list" => &["snippet"],
        "gyoutube_members_list" => &["snippet"],
        "gyoutube_memberships_levels_list" => &["snippet", "id"],
        "gyoutube_playlist_images_list" => &["snippet", "id"],
        "gyoutube_playlist_images_insert" | "gyoutube_playlist_images_update" => &["snippet"],
        "gyoutube_playlist_items_list" => &["snippet", "contentDetails", "status", "id"],
        "gyoutube_playlist_items_insert" | "gyoutube_playlist_items_update" => {
            &["snippet", "contentDetails", "status"]
        }
        "gyoutube_playlists_list" => &[
            "snippet",
            "contentDetails",
            "status",
            "player",
            "localizations",
            "id",
        ],
        "gyoutube_playlists_insert" | "gyoutube_playlists_update" => {
            &["snippet", "status", "localizations"]
        }
        "gyoutube_search_list" => &["snippet", "id"],
        "gyoutube_subscriptions_list" => &["snippet", "contentDetails", "subscriberSnippet", "id"],
        "gyoutube_subscriptions_insert" => &["snippet"],
        "gyoutube_video_abuse_report_reasons_list" => &["snippet", "id"],
        "gyoutube_video_categories_list" => &["snippet", "id"],
        "gyoutube_videos_list" => &[
            "snippet",
            "contentDetails",
            "statistics",
            "status",
            "player",
            "topicDetails",
            "recordingDetails",
            "liveStreamingDetails",
            "localizations",
            "fileDetails",
            "processingDetails",
            "suggestions",
            "id",
        ],
        "gyoutube_videos_insert" | "gyoutube_videos_update" => {
            &["snippet", "status", "recordingDetails", "localizations"]
        }
        _ => &["snippet", "id"],
    }
}

/// What each `part` section actually returns, so the picker can explain itself
/// instead of listing bare API strings.
fn part_description(part: &str) -> &'static str {
    match part {
        "snippet" => "Title, description, thumbnails and other basic details.",
        "contentDetails" => "Content info — durations, item counts, related playlists.",
        "statistics" => "View, like, comment and subscriber counts.",
        "status" => "Privacy, upload and licensing status.",
        "player" => "Embeddable player HTML.",
        "topicDetails" => "Topics associated with the resource.",
        "brandingSettings" => "Channel branding — banner, keywords, watermarks.",
        "localizations" => "Translated titles and descriptions.",
        "contentOwnerDetails" => "Content-owner linkage (CMS-linked channels only).",
        "invideoPromotion" => "In-video promotion campaign settings.",
        "recordingDetails" => "Where and when the video was recorded.",
        "liveStreamingDetails" => "Live broadcast timings and viewer counts.",
        "fileDetails" => "Uploaded file metadata (your own videos only).",
        "processingDetails" => "Upload processing progress (your own videos only).",
        "suggestions" => "Improvement suggestions (your own videos only).",
        "replies" => "Reply comments attached to the thread.",
        "subscriberSnippet" => "Details of the subscribing channel.",
        "targeting" => "Language, region and country targeting.",
        "id" => "IDs only — the lightest response.",
        _ => "",
    }
}

/// Query parameters with a fixed value set. These get their own dropdown instead
/// of hiding inside the free-form `params` object.
fn query_enums(tool: &str) -> &'static [(&'static str, &'static [&'static str])] {
    match tool {
        "gyoutube_comments_set_moderation_status" => &[(
            "moderationStatus",
            &["published", "heldForReview", "rejected"],
        )],
        "gyoutube_videos_rate" => &[("rating", &["like", "dislike", "none"])],
        "gyoutube_videos_list" => &[
            ("chart", &["mostPopular"]),
            ("myRating", &["like", "dislike"]),
        ],
        "gyoutube_search_list" => &[
            ("type", &["video", "channel", "playlist"]),
            (
                "order",
                &[
                    "relevance",
                    "date",
                    "rating",
                    "title",
                    "videoCount",
                    "viewCount",
                ],
            ),
            ("eventType", &["live", "upcoming", "completed"]),
            ("videoDuration", &["any", "short", "medium", "long"]),
            ("safeSearch", &["moderate", "none", "strict"]),
        ],
        "gyoutube_subscriptions_list" => &[("order", &["relevance", "alphabetical", "unread"])],
        "gyoutube_comment_threads_list" => &[
            ("order", &["time", "relevance"]),
            (
                "moderationStatus",
                &["published", "heldForReview", "likelySpam", "rejected"],
            ),
            ("textFormat", &["html", "plainText"]),
        ],
        "gyoutube_comments_list" => &[("textFormat", &["html", "plainText"])],
        _ => &[],
    }
}

/// Query keys this action exposes as their own field rather than through `params`.
fn dedicated_query_keys(spec: &ActionSpec) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = spec.required_query.to_vec();
    for (key, _) in query_enums(spec.tool) {
        if !keys.contains(key) {
            keys.push(key);
        }
    }
    keys
}

pub fn tool_list() -> Vec<Tool> {
    ACTIONS.iter().map(tool_from_spec).collect()
}

pub async fn try_call(
    state: &AppState,
    name: &str,
    args: &Map<String, Value>,
) -> Result<Option<Value>> {
    let Some(spec) = ACTIONS.iter().find(|spec| spec.tool == name) else {
        return Ok(None);
    };
    Ok(Some(call_action(state, spec, args).await?))
}

fn tool_from_spec(spec: &ActionSpec) -> Tool {
    let mut properties = Map::new();

    if spec.requires_part {
        let options = part_options(spec.tool);
        let descriptions: Map<String, Value> = options
            .iter()
            .map(|p| {
                (
                    (*p).to_string(),
                    Value::String(part_description(p).to_string()),
                )
            })
            .collect();
        properties.insert(
            "part".to_string(),
            json!({
                "type": "array",
                "items": { "type": "string", "enum": options },
                "enumDescriptions": descriptions,
                "default": [options[0]],
                "description": "Resource sections to return. Only the listed values are accepted; \
                                anything else is rejected by the API."
            }),
        );
    }

    // Required parameters and fixed-value filters each get their own field, so
    // they are pickable instead of only reachable by hand-writing `params` JSON.
    let enums = query_enums(spec.tool);
    for key in spec.required_query {
        let mut schema = json!({
            "type": "string",
            "description": format!("Required by {}.", spec.tool),
        });
        // Deliberately no default: nodes saved before this field existed still
        // carry the value inside `params`, and a seeded default would outrank it.
        if let Some((_, values)) = enums.iter().find(|(k, _)| k == key) {
            schema["enum"] = json!(values);
        }
        properties.insert((*key).to_string(), schema);
    }
    for (key, values) in enums {
        if spec.required_query.contains(key) {
            continue;
        }
        // The leading "" keeps the filter optional — the UI renders it as "Any".
        let mut choices = vec![""];
        choices.extend_from_slice(values);
        properties.insert(
            (*key).to_string(),
            json!({
                "type": "string",
                "enum": choices,
                "default": "",
                "description": format!("Optional '{key}' filter."),
            }),
        );
    }

    if !spec.required_query.is_empty() || spec.method == "GET" || spec.method == "DELETE" {
        properties.insert(
            "params".to_string(),
            json!({
                "type": "object",
                "description": "Any further query string parameters as a JSON object, e.g. {\"maxResults\":25}."
            }),
        );
    }
    if spec.method == "POST" || spec.method == "PUT" || spec.method == "PATCH" {
        properties.insert(
            "body".to_string(),
            json!({
                "type": "object",
                "description": "Request body JSON object for insert/update/report actions."
            }),
        );
    }

    if spec.tool == "gyoutube_videos_insert" || spec.tool == "gyoutube_videos_update" {
        properties.insert(
            "title".to_string(),
            json!({
                "type": "string",
                "description": "Video title."
            }),
        );
        properties.insert(
            "description".to_string(),
            json!({
                "type": "string",
                "description": "Video description."
            }),
        );
    }

    if spec.supports_upload {
        properties.insert(
            "upload_file_path".to_string(),
            json!({
                "type": "string",
                "description": "Local file path for media upload."
            }),
        );
        properties.insert(
            "upload_mime_type".to_string(),
            json!({
                "type": "string",
                "default": "application/octet-stream",
                "description": "MIME type for uploaded media."
            }),
        );
    }

    if spec.returns_binary {
        properties.insert(
            "download_filename".to_string(),
            json!({
                "type": "string",
                "description": "Optional local filename for downloaded media."
            }),
        );
    }

    for key in spec.path_params {
        properties.insert(
            (*key).to_string(),
            json!({
                "type": "string",
                "description": format!("Path parameter '{key}' required by this action.")
            }),
        );
    }

    let mut required: Vec<String> = Vec::new();
    if spec.requires_part {
        required.push("part".to_string());
    }
    if spec.media_required {
        required.push("upload_file_path".to_string());
    }
    for key in spec.path_params {
        required.push((*key).to_string());
    }
    for key in spec.required_query {
        required.push((*key).to_string());
    }

    let schema = json!({
        "type": "object",
        "properties": properties,
        "required": required,
    });
    let input_schema = schema.as_object().cloned().unwrap_or_default();

    Tool::new(spec.tool, spec.description, Arc::new(input_schema))
}

async fn call_action(
    state: &AppState,
    spec: &ActionSpec,
    args: &Map<String, Value>,
) -> Result<Value> {
    let token = access_token(state).await?;
    let path = build_path(spec, args)?;

    let mut query = build_query(spec, args)?;
    let mut body = parse_json_value_arg(args, "body")?;

    if spec.tool == "gyoutube_videos_insert" || spec.tool == "gyoutube_videos_update" {
        let title = opt_string_arg(args, "title");
        let desc = opt_string_arg(args, "description");
        if title.is_some() || desc.is_some() {
            let mut body_obj = match body {
                Some(Value::Object(m)) => m,
                _ => Map::new(),
            };
            let mut snippet = match body_obj.get("snippet") {
                Some(Value::Object(m)) => m.clone(),
                _ => Map::new(),
            };
            if let Some(t) = title {
                snippet.insert("title".to_string(), Value::String(t));
            }
            if let Some(d) = desc {
                snippet.insert("description".to_string(), Value::String(d));
            }
            body_obj.insert("snippet".to_string(), Value::Object(snippet));
            body = Some(Value::Object(body_obj));
        }
    }

    let upload_path = opt_string_arg(args, "upload_file_path");
    if upload_path.is_some() && !spec.supports_upload {
        bail!("{} does not support media upload", spec.tool);
    }
    if spec.media_required && upload_path.is_none() {
        bail!("{} requires upload_file_path", spec.tool);
    }

    let url_base = if upload_path.is_some() {
        UPLOAD_BASE
    } else {
        BASE
    };
    let url = format!("{url_base}{path}");

    let response = if let Some(file_path) = upload_path {
        let bytes = tokio::fs::read(&file_path)
            .await
            .map_err(|e| anyhow!("failed to read upload_file_path '{file_path}': {e}"))?;
        let mime = opt_string_arg(args, "upload_mime_type")
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let req = if body.as_ref().is_some_and(|v| !v.is_null()) {
            query.push(("uploadType".to_string(), "multipart".to_string()));
            let boundary = "axon_youtube_boundary";
            let body_json = body.unwrap_or_else(|| json!({}));
            let mut payload = Vec::new();
            payload.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{}\r\n",
                    serde_json::to_string(&body_json)?
                )
                .as_bytes(),
            );
            payload.extend_from_slice(
                format!("--{boundary}\r\nContent-Type: {mime}\r\n\r\n").as_bytes(),
            );
            payload.extend_from_slice(&bytes);
            payload.extend_from_slice(format!("\r\n--{boundary}--").as_bytes());

            let req = match spec.method {
                "POST" => state.client.post(&url),
                "PUT" => state.client.put(&url),
                "PATCH" => state.client.patch(&url),
                _ => bail!(
                    "unsupported upload method {} for {}",
                    spec.method,
                    spec.tool
                ),
            };
            req.bearer_auth(&token)
                .query(&query)
                .header(
                    "Content-Type",
                    format!("multipart/related; boundary={boundary}"),
                )
                .body(payload)
        } else {
            query.push(("uploadType".to_string(), "media".to_string()));
            let req = match spec.method {
                "POST" => state.client.post(&url),
                "PUT" => state.client.put(&url),
                "PATCH" => state.client.patch(&url),
                _ => bail!(
                    "unsupported upload method {} for {}",
                    spec.method,
                    spec.tool
                ),
            };
            req.bearer_auth(&token)
                .query(&query)
                .header("Content-Type", mime)
                .body(bytes)
        };
        req.send().await?.ensure_ok().await?
    } else {
        let mut req = match spec.method {
            "GET" => state.client.get(&url),
            "POST" => state.client.post(&url),
            "PUT" => state.client.put(&url),
            "DELETE" => state.client.delete(&url),
            "PATCH" => state.client.patch(&url),
            other => bail!("unsupported HTTP method '{other}' for {}", spec.tool),
        }
        .bearer_auth(&token)
        .query(&query);

        if let Some(body_json) = body {
            if spec.method != "GET" && spec.method != "DELETE" {
                req = req.json(&body_json);
            }
        }

        req.send().await?.ensure_ok().await?
    };

    if spec.returns_binary {
        let filename = opt_string_arg(args, "download_filename")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("{}_download.bin", spec.tool));
        let download_dir = axon_core::data_files_dir();
        tokio::fs::create_dir_all(&download_dir).await?;
        let path = download_dir.join(&filename);
        let bytes = response.bytes().await?;
        tokio::fs::write(&path, &bytes).await?;
        return Ok(json!({
            "success": true,
            "name": filename,
            "bytes": bytes.len(),
            "file_path": path.to_string_lossy(),
            "message": "Download complete. Use file_path to access the downloaded file."
        }));
    }

    if response.status().as_u16() == 204 {
        return Ok(json!({ "success": true }));
    }

    Ok(response.json().await?)
}

fn build_path(spec: &ActionSpec, args: &Map<String, Value>) -> Result<String> {
    let mut path = spec.path.to_string();
    for key in spec.path_params {
        let raw = required_string_arg(args, key)?;
        let encoded = url_escape(raw);
        path = path.replace(&format!("{{{key}}}"), &encoded);
    }
    Ok(path)
}

fn build_query(spec: &ActionSpec, args: &Map<String, Value>) -> Result<Vec<(String, String)>> {
    let mut query: Vec<(String, String)> = Vec::new();

    if let Some(part) = part_arg(args) {
        query.push(("part".to_string(), part));
    }

    // A dedicated field wins over the same key inside `params`.
    for key in dedicated_query_keys(spec) {
        let Some(value) = args.get(key) else { continue };
        let rendered = value_to_query(value);
        if rendered.trim().is_empty() {
            continue;
        }
        query.push((key.to_string(), rendered));
    }

    let params = parse_json_object_arg(args, "params")?;
    for (k, v) in params {
        if !v.is_null() && !query.iter().any(|(qk, _)| qk == &k) {
            query.push((k, value_to_query(&v)));
        }
    }

    if spec.requires_part && !query.iter().any(|(k, _)| k == "part") {
        bail!(
            "{} requires 'part'. Choose one or more of: {}",
            spec.tool,
            part_options(spec.tool).join(", ")
        );
    }

    for key in spec.required_query {
        if !query.iter().any(|(k, _)| k == key) {
            bail!("{} requires '{}'", spec.tool, key);
        }
    }

    Ok(query)
}

/// `part` arrives as a comma-separated string from agent calls and as an array
/// from the workflow multi-select; normalise both to the comma list the API wants.
fn part_arg(args: &Map<String, Value>) -> Option<String> {
    let raw: Vec<String> = match args.get("part")? {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => return None,
    };
    let joined = raw
        .iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    (!joined.is_empty()).then_some(joined)
}

fn parse_json_object_arg(args: &Map<String, Value>, key: &str) -> Result<Map<String, Value>> {
    let Some(value) = args.get(key) else {
        return Ok(Map::new());
    };

    match value {
        Value::Object(map) => Ok(map.clone()),
        Value::String(s) => {
            if s.trim().is_empty() {
                return Ok(Map::new());
            }
            let parsed: Value = serde_json::from_str(s)
                .map_err(|e| anyhow!("{key} must be valid JSON object string: {e}"))?;
            let Some(obj) = parsed.as_object() else {
                bail!("{key} must be a JSON object");
            };
            Ok(obj.clone())
        }
        _ => bail!("{key} must be a JSON object or JSON object string"),
    }
}

fn parse_json_value_arg(args: &Map<String, Value>, key: &str) -> Result<Option<Value>> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };

    match value {
        Value::Null => Ok(None),
        Value::String(s) => {
            if s.trim().is_empty() {
                return Ok(None);
            }
            match serde_json::from_str::<Value>(s) {
                Ok(v) => Ok(Some(v)),
                Err(_) => Ok(Some(Value::String(s.clone()))),
            }
        }
        other => Ok(Some(other.clone())),
    }
}

fn required_string_arg<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("missing required argument '{key}'"))
}

fn opt_string_arg(args: &Map<String, Value>, key: &str) -> Option<String> {
    args.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

fn value_to_query(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(arr) => arr.iter().map(value_to_query).collect::<Vec<_>>().join(","),
        Value::Object(_) => serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
        Value::Null => "".to_string(),
    }
}

fn url_escape(raw: &str) -> String {
    url::form_urlencoded::byte_serialize(raw.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(tool: &str) -> &'static ActionSpec {
        ACTIONS.iter().find(|s| s.tool == tool).expect("known tool")
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn part_accepts_the_multi_select_array() {
        let query = build_query(
            spec("gyoutube_channels_list"),
            &args(json!({ "part": ["snippet", "statistics"] })),
        )
        .expect("query");
        assert_eq!(
            query,
            vec![("part".to_string(), "snippet,statistics".to_string())]
        );
    }

    #[test]
    fn part_still_accepts_a_comma_string() {
        let query = build_query(
            spec("gyoutube_channels_list"),
            &args(json!({ "part": "snippet, statistics" })),
        )
        .expect("query");
        assert_eq!(
            query,
            vec![("part".to_string(), "snippet,statistics".to_string())]
        );
    }

    #[test]
    fn missing_part_names_the_valid_sections() {
        let err = build_query(spec("gyoutube_channels_list"), &args(json!({ "part": [] })))
            .expect_err("part is required")
            .to_string();
        assert!(err.contains("brandingSettings"), "{err}");
    }

    #[test]
    fn dedicated_field_outranks_the_params_blob() {
        let query = build_query(
            spec("gyoutube_videos_rate"),
            &args(json!({ "id": "abc", "rating": "like", "params": { "rating": "dislike" } })),
        )
        .expect("query");
        assert!(query.contains(&("rating".to_string(), "like".to_string())));
        assert_eq!(query.iter().filter(|(k, _)| k == "rating").count(), 1);
    }

    #[test]
    fn params_still_satisfies_required_query_for_older_nodes() {
        let query = build_query(
            spec("gyoutube_videos_rate"),
            &args(json!({ "params": { "id": "abc", "rating": "dislike" } })),
        )
        .expect("query");
        assert!(query.contains(&("rating".to_string(), "dislike".to_string())));
    }

    #[test]
    fn unset_optional_filter_is_not_sent() {
        // The form seeds untouched dropdowns with "" — that must not reach the API.
        let query = build_query(
            spec("gyoutube_search_list"),
            &args(json!({ "part": ["snippet"], "order": "", "type": "video" })),
        )
        .expect("query");
        assert!(!query.iter().any(|(k, _)| k == "order"));
        assert!(query.contains(&("type".to_string(), "video".to_string())));
    }

    #[test]
    fn part_schema_is_a_bounded_multi_select() {
        let tool = tool_from_spec(spec("gyoutube_channels_list"));
        let properties = tool.input_schema.get("properties").expect("properties");
        let part = &properties["part"];
        assert_eq!(part["type"], "array");
        assert_eq!(part["items"]["enum"][0], "snippet");
        assert_eq!(part["default"], json!(["snippet"]));
        assert!(!part["enumDescriptions"]["statistics"]
            .as_str()
            .unwrap_or_default()
            .is_empty());
    }

    #[test]
    fn write_actions_only_offer_writable_parts() {
        // channels.update rejects `snippet`; offering it would guarantee a 400.
        assert!(!part_options("gyoutube_channels_update").contains(&"snippet"));
        assert!(part_options("gyoutube_channels_update").contains(&"brandingSettings"));
    }

    #[test]
    fn required_query_gets_its_own_field() {
        let tool = tool_from_spec(spec("gyoutube_videos_rate"));
        let properties = tool.input_schema.get("properties").expect("properties");
        assert_eq!(
            properties["rating"]["enum"],
            json!(["like", "dislike", "none"])
        );
        // A default here would outrank the value an older node stored in `params`.
        assert!(properties["rating"].get("default").is_none());
        let required = tool.input_schema.get("required").expect("required");
        assert!(required
            .as_array()
            .expect("array")
            .iter()
            .any(|v| v == "rating"));
    }
}
