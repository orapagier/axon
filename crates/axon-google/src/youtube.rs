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
        // `localizations` and `targeting` are channelSection *resource* properties
        // but not `part` values on any channelSections method — list, insert and
        // update all document exactly id, snippet and contentDetails. Naming one
        // 400s the call, and because a read asks for every option, leaving them
        // here broke channel_sections_list on every run.
        "gyoutube_channel_sections_list" => &["snippet", "contentDetails", "id"],
        "gyoutube_channel_sections_insert" | "gyoutube_channel_sections_update" => {
            &["snippet", "contentDetails"]
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

/// Sentinel `part` value meaning "every section this action returns". Expanded
/// to the real list before the request goes out — the API itself has no such
/// value.
const ALL_PARTS: &str = "*";

/// Only read actions offer "All sections". On insert/update the part list names
/// the sections being written, so it has to match the body actually sent.
fn offers_all_parts(tool: &str) -> bool {
    tool.ends_with("_list")
}

/// Sections the API serves only to the resource's owner (or a CMS-linked
/// account). "All sections" leaves them out: asking for one on someone else's
/// video fails the *whole* request with a 403, which no amount of extra data is
/// worth. They stay individually selectable for your own resources.
fn owner_only_part(part: &str) -> bool {
    matches!(
        part,
        "fileDetails" | "processingDetails" | "suggestions" | "contentOwnerDetails"
    )
}

/// What `*` stands for on a given action. `id` is dropped because every response
/// carries the resource id regardless of the part list — asking for it alongside
/// other sections adds nothing.
fn all_parts(tool: &str) -> Vec<&'static str> {
    let expanded: Vec<&'static str> = part_options(tool)
        .iter()
        .copied()
        .filter(|p| *p != "id" && !owner_only_part(p))
        .collect();
    if expanded.is_empty() {
        part_options(tool).to_vec()
    } else {
        expanded
    }
}

/// The `part` a call sends when nothing asks for a specific one — which is now
/// the normal case, since the field is gone from the form.
///
/// A read takes every section the action offers. A write takes the sections its
/// body actually carries: `part` on insert/update names what is being written,
/// so a section named but absent from the body is *cleared* on the resource, and
/// a section present but unnamed is silently dropped. Deriving it from the body
/// is the only reading that can't corrupt the resource.
fn default_part(spec: &ActionSpec, args: &Map<String, Value>) -> String {
    if offers_all_parts(spec.tool) {
        return all_parts(spec.tool).join(",");
    }

    let options = part_options(spec.tool);
    let body = parse_json_object_arg(args, "body").unwrap_or_default();
    let mut parts: Vec<&str> = options
        .iter()
        .copied()
        .filter(|section| body.contains_key(*section))
        .collect();

    // `title`/`description` are folded into snippet after the query is built, so
    // the body alone does not show they are coming.
    let writes_snippet_fields = non_empty_string_arg(args, "title").is_some()
        || non_empty_string_arg(args, "description").is_some();
    if writes_snippet_fields && options.contains(&"snippet") && !parts.contains(&"snippet") {
        parts.push("snippet");
    }

    if parts.is_empty() {
        parts.push(options[0]);
    }
    parts.join(",")
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
            // No `rejected` here: comments.setModerationStatus writes that state
            // but commentThreads.list cannot read it back, and offering it as a
            // filter only ever produced a 400.
            (
                "moderationStatus",
                &["published", "heldForReview", "likelySpam"],
            ),
            ("textFormat", &["html", "plainText"]),
        ],
        "gyoutube_comments_list" => &[("textFormat", &["html", "plainText"])],
        _ => &[],
    }
}

/// A query parameter the API accepts only while a companion field holds one of a
/// set of values — the cross-field version of the "exactly one filter" rule.
///
/// Sending one outside its window is a 400 that names neither field clearly, so
/// the form hides it until the companion is set and `build_query` refuses it
/// before the call goes out.
struct Companion {
    key: &'static str,
    /// Field it rides on. `filter_by` means the chosen list filter.
    depends_on: &'static str,
    allowed: &'static [&'static str],
    /// Why, phrased to drop into the error message after an em dash.
    reason: &'static str,
}

/// search.list documents these one by one as "If you specify a value for this
/// parameter, you must also set the type parameter's value to video."
const SEARCH_COMPANIONS: &[Companion] = &[
    Companion {
        key: "eventType",
        depends_on: "type",
        allowed: &["video"],
        reason: "search.list only applies live-event filtering to videos",
    },
    Companion {
        key: "videoDuration",
        depends_on: "type",
        allowed: &["video"],
        reason: "search.list only applies duration filtering to videos",
    },
];

/// commentThreads.list serves `moderationStatus` on a channel- or video-wide
/// listing, but rejects it beside the `id` filter — ids already name the threads.
const COMMENT_THREAD_COMPANIONS: &[Companion] = &[Companion {
    key: "moderationStatus",
    depends_on: "filter_by",
    allowed: &["videoId", "allThreadsRelatedToChannelId"],
    reason: "the API refuses it alongside the 'id' filter",
}];

/// The scope an action needs on top of the ones `auth::SCOPES` asks for, or
/// `None` when nothing is missing.
///
/// Checked against that list rather than against the token, so it is exact: a
/// scope absent there was never consented to. It also means the guard removes
/// itself — add the scope in `auth.rs` and this returns `None` on its own.
fn missing_scope(tool: &str) -> Option<&'static str> {
    let needed = match tool {
        // members.list and membershipsLevels.list are served only under this
        // scope, and only to a channel in the YouTube Partner Program with
        // channel memberships switched on. Requesting it unconditionally would
        // put a "see your channel members" line on the consent screen of every
        // account connected here, Gmail-only ones included, so it stays opt-in.
        "gyoutube_members_list" | "gyoutube_memberships_levels_list" => {
            "https://www.googleapis.com/auth/youtube.channel-memberships.creator"
        }
        _ => return None,
    };
    (!crate::auth::SCOPES.contains(&needed)).then_some(needed)
}

fn companions(tool: &str) -> &'static [Companion] {
    match tool {
        "gyoutube_search_list" => SEARCH_COMPANIONS,
        "gyoutube_comment_threads_list" => COMMENT_THREAD_COMPANIONS,
        _ => &[],
    }
}

/// One of the mutually-exclusive filters a list endpoint selects rows by.
struct Filter {
    key: &'static str,
    /// Field title in the form.
    label: &'static str,
    /// What to enter, shown as the hint and beside the choice in the dropdown.
    hint: &'static str,
    /// Fixed value set; empty means free text.
    values: &'static [&'static str],
    /// A flag filter carries no value of its own — it is sent as `key=true`.
    flag: bool,
}

const fn by_value(key: &'static str, label: &'static str, hint: &'static str) -> Filter {
    Filter {
        key,
        label,
        hint,
        values: &[],
        flag: false,
    }
}

const fn by_flag(key: &'static str, label: &'static str, hint: &'static str) -> Filter {
    Filter {
        key,
        label,
        hint,
        values: &[],
        flag: true,
    }
}

const fn by_choice(
    key: &'static str,
    label: &'static str,
    hint: &'static str,
    values: &'static [&'static str],
) -> Filter {
    Filter {
        key,
        label,
        hint,
        values,
        flag: false,
    }
}

const ACTIVITY_FILTERS: &[Filter] = &[
    by_flag("mine", "Mine", "Your own channel's activity."),
    by_value("channelId", "Channel ID", "Channel ID, starts with UC…"),
];

// `managedByMe` is deliberately absent. It is a content-partner filter: YouTube
// serves it only when the request also carries `onBehalfOfContentOwner` under a
// token holding the `youtubepartner` scope. We send neither, so it answers 403
// every time — the same reason `owner_only_part` sections stay out of "All
// sections", applied to a filter instead of a part.
const CHANNEL_FILTERS: &[Filter] = &[
    by_flag("mine", "Mine", "Your own channel."),
    by_value(
        "id",
        "Channel ID",
        "Channel ID, starts with UC… Comma-separate for several.",
    ),
    by_value("forHandle", "Handle", "Channel handle, e.g. @SomeChannel."),
    by_value("forUsername", "Username", "Legacy YouTube username."),
];

const CHANNEL_SECTION_FILTERS: &[Filter] = &[
    by_flag("mine", "Mine", "Sections on your own channel."),
    by_value("channelId", "Channel ID", "Channel ID, starts with UC…"),
    by_value(
        "id",
        "Section ID",
        "Channel section ID. Comma-separate for several.",
    ),
];

const COMMENT_FILTERS: &[Filter] = &[
    by_value(
        "parentId",
        "Parent Comment ID",
        "Top-level comment whose replies to list.",
    ),
    by_value(
        "id",
        "Comment ID",
        "Comment ID. Comma-separate for several.",
    ),
];

const COMMENT_THREAD_FILTERS: &[Filter] = &[
    by_value(
        "videoId",
        "Video ID",
        "Video whose comment threads to list.",
    ),
    by_value(
        "allThreadsRelatedToChannelId",
        "Channel ID",
        "Every thread on the channel and on its videos.",
    ),
    by_value(
        "id",
        "Thread ID",
        "Comment thread ID. Comma-separate for several.",
    ),
];

const PLAYLIST_ITEM_FILTERS: &[Filter] = &[
    by_value("playlistId", "Playlist ID", "Playlist whose items to list."),
    by_value(
        "id",
        "Item ID",
        "Playlist item ID. Comma-separate for several.",
    ),
];

const PLAYLIST_IMAGE_FILTERS: &[Filter] = &[
    by_value("playlistId", "Playlist ID", "Playlist whose images to list."),
    by_value(
        "id",
        "Image ID",
        "Playlist image ID. Comma-separate for several.",
    ),
];

const PLAYLIST_FILTERS: &[Filter] = &[
    by_flag("mine", "Mine", "Your own playlists."),
    by_value("channelId", "Channel ID", "Channel ID, starts with UC…"),
    by_value(
        "id",
        "Playlist ID",
        "Playlist ID. Comma-separate for several.",
    ),
];

const SUBSCRIPTION_FILTERS: &[Filter] = &[
    by_flag("mine", "Mine", "Channels you subscribe to."),
    by_value(
        "channelId",
        "Channel ID",
        "Channel whose subscriptions to list.",
    ),
    by_value(
        "id",
        "Subscription ID",
        "Subscription ID. Comma-separate for several.",
    ),
    by_flag(
        "mySubscribers",
        "My subscribers",
        "Channels subscribed to you.",
    ),
    by_flag(
        "myRecentSubscribers",
        "My recent subscribers",
        "Your most recent subscribers.",
    ),
];

const VIDEO_CATEGORY_FILTERS: &[Filter] = &[
    by_value(
        "regionCode",
        "Region Code",
        "ISO 3166-1 country code, e.g. US.",
    ),
    by_value(
        "id",
        "Category ID",
        "Video category ID. Comma-separate for several.",
    ),
];

const VIDEO_FILTERS: &[Filter] = &[
    by_value("id", "Video ID", "Video ID. Comma-separate for several."),
    by_choice("chart", "Chart", "A YouTube chart.", &["mostPopular"]),
    by_choice(
        "myRating",
        "My Rating",
        "Videos you rated.",
        &["like", "dislike"],
    ),
];

/// The mutually-exclusive filters a list action selects rows by.
///
/// YouTube rejects these endpoints outright when none is given ("No filter
/// selected. Expected one of: …") and again when two are given, so an action that
/// declares filters here always requires exactly one. Modelling the choice as a
/// single `filter_by` field makes picking two structurally impossible, and lets
/// the node ask for the value up front instead of failing at the API.
fn filters(tool: &str) -> &'static [Filter] {
    match tool {
        "gyoutube_activities_list" => ACTIVITY_FILTERS,
        "gyoutube_channels_list" => CHANNEL_FILTERS,
        "gyoutube_channel_sections_list" => CHANNEL_SECTION_FILTERS,
        "gyoutube_comments_list" => COMMENT_FILTERS,
        "gyoutube_comment_threads_list" => COMMENT_THREAD_FILTERS,
        "gyoutube_playlist_images_list" => PLAYLIST_IMAGE_FILTERS,
        "gyoutube_playlist_items_list" => PLAYLIST_ITEM_FILTERS,
        "gyoutube_playlists_list" => PLAYLIST_FILTERS,
        "gyoutube_subscriptions_list" => SUBSCRIPTION_FILTERS,
        "gyoutube_video_categories_list" => VIDEO_CATEGORY_FILTERS,
        "gyoutube_videos_list" => VIDEO_FILTERS,
        _ => &[],
    }
}

/// List actions that page. The fixed-catalogue endpoints (i18n languages and
/// regions, video categories, abuse reasons) return everything at once and reject
/// paging parameters, so they are deliberately absent.
fn supports_paging(tool: &str) -> bool {
    matches!(
        tool,
        "gyoutube_activities_list"
            | "gyoutube_channels_list"
            | "gyoutube_comments_list"
            | "gyoutube_comment_threads_list"
            | "gyoutube_members_list"
            | "gyoutube_memberships_levels_list"
            | "gyoutube_playlist_images_list"
            | "gyoutube_playlist_items_list"
            | "gyoutube_playlists_list"
            | "gyoutube_search_list"
            | "gyoutube_subscriptions_list"
            | "gyoutube_videos_list"
    )
}

/// Free-text query parameters common enough to deserve their own field.
fn text_query_keys(tool: &str) -> &'static [(&'static str, &'static str, &'static str)] {
    match tool {
        "gyoutube_search_list" => &[(
            "q",
            "Search Terms",
            "What to search for. Optional — you can also search by channel, order or date alone.",
        )],
        _ => &[],
    }
}

/// Query keys this action exposes as their own field rather than through `params`.
/// Filter keys are excluded: `resolve_filter` owns those.
fn dedicated_query_keys(spec: &ActionSpec) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = spec.required_query.to_vec();
    for (key, _) in query_enums(spec.tool) {
        if !keys.contains(key) {
            keys.push(key);
        }
    }
    for (key, _, _) in text_query_keys(spec.tool) {
        keys.push(key);
    }
    if supports_paging(spec.tool) {
        keys.push("maxResults");
        keys.push("pageToken");
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

    // `part` has no field: it is derived in `default_part`. Reads take every
    // section, writes take the ones the body carries. Nobody was picking sections
    // for the fun of it, and a half-picked list is only ever a smaller answer.

    // The list filter: one choice, plus a value field per filter that takes one.
    // Each value field is gated on the choice, so exactly one is ever visible and
    // the user cannot express the "two filters" the API also rejects.
    let action_filters = filters(spec.tool);
    if !action_filters.is_empty() {
        let keys: Vec<&str> = action_filters.iter().map(|f| f.key).collect();
        let hints: Map<String, Value> = action_filters
            .iter()
            .map(|f| (f.key.to_string(), Value::String(f.hint.to_string())))
            .collect();
        // Pre-picked so the node runs without a trip to the dropdown. A flag
        // filter ("mine") needs nothing else, so it wins where the action has
        // one; otherwise the first filter leads and its value box opens with it.
        let preset = action_filters
            .iter()
            .find(|f| f.flag)
            .unwrap_or(&action_filters[0]);
        properties.insert(
            "filter_by".to_string(),
            json!({
                "type": "string",
                "title": "Filter By",
                "enum": keys,
                "enumDescriptions": hints,
                "default": preset.key,
                "description": "Which filter to list by. This endpoint requires exactly one — \
                                the API rejects a call with none, and one with two.",
            }),
        );
        for filter in action_filters.iter().filter(|f| !f.flag) {
            let mut schema = json!({
                "type": "string",
                "title": filter.label,
                "description": filter.hint,
                "displayOptions": { "show": { "filter_by": [filter.key] } },
            });
            if !filter.values.is_empty() {
                schema["enum"] = json!(filter.values);
                // Safe to preselect: a filter that was not chosen never reaches
                // the query, so this only ever fills in the field you opened.
                schema["default"] = json!(filter.values[0]);
            }
            properties.insert(filter.key.to_string(), schema);
        }
    }

    if supports_paging(spec.tool) {
        properties.insert(
            "maxResults".to_string(),
            json!({
                "type": "integer",
                "title": "Max Results",
                "description": "Results per page (1–50). Blank uses the API default.",
            }),
        );
        properties.insert(
            "pageToken".to_string(),
            json!({
                "type": "string",
                "title": "Page Token",
                "description": "The nextPageToken from a previous response, to fetch the next page.",
            }),
        );
    }

    for (key, label, description) in text_query_keys(spec.tool) {
        properties.insert(
            (*key).to_string(),
            json!({ "type": "string", "title": label, "description": description }),
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
        let mut schema = json!({
            "type": "string",
            "enum": choices,
            "default": "",
            "description": format!("Optional '{key}' filter."),
        });
        // A parameter the API only accepts beside another one stays out of the
        // form until that other one is set, the same way a filter's value field
        // waits on `filter_by`.
        if let Some(companion) = companions(spec.tool).iter().find(|c| c.key == *key) {
            schema["description"] = json!(format!(
                "Optional '{key}' filter. Needs {} set to {} — {}.",
                companion.depends_on,
                companion.allowed.join(" or "),
                companion.reason
            ));
            schema["displayOptions"] =
                json!({ "show": { companion.depends_on: companion.allowed } });
        }
        properties.insert((*key).to_string(), schema);
    }

    if !spec.required_query.is_empty() || spec.method == "GET" || spec.method == "DELETE" {
        let mut description = "Any further query string parameters as a JSON object, e.g. \
                               {\"maxResults\":25}."
            .to_string();
        if spec.requires_part {
            // The escape hatch for the field that is no longer on the form: the
            // response carries every section unless something narrows it here.
            description.push_str(&format!(
                " Every section is returned by default; pass \"part\" here (e.g. \
                 {{\"part\":\"snippet\"}}) for a leaner response. Valid sections: {}.",
                part_options(spec.tool).join(", ")
            ));
        }
        properties.insert(
            "params".to_string(),
            json!({
                "type": "object",
                "description": description,
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
    if spec.media_required {
        required.push("upload_file_path".to_string());
    }
    for key in spec.path_params {
        required.push((*key).to_string());
    }
    for key in spec.required_query {
        required.push((*key).to_string());
    }
    if !filters(spec.tool).is_empty() {
        required.push("filter_by".to_string());
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
    if let Some(scope) = missing_scope(spec.tool) {
        bail!(
            "{} needs the '{scope}' scope, which this app does not request — the call would come \
             back 403 whatever you pass it. To enable it, add that scope to SCOPES in \
             crates/axon-google/src/auth.rs and reconnect the Google account; note the channel \
             must also be in the YouTube Partner Program with channel memberships turned on.",
            spec.tool
        );
    }

    let token = access_token(state).await?;
    let path = build_path(spec, args)?;

    let mut query = build_query(spec, args)?;
    let mut body = parse_json_value_arg(args, "body")?;

    if spec.tool == "gyoutube_videos_insert" || spec.tool == "gyoutube_videos_update" {
        let title = non_empty_string_arg(args, "title");
        let desc = non_empty_string_arg(args, "description");
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

    let upload_path = non_empty_string_arg(args, "upload_file_path");
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
        let filename = non_empty_string_arg(args, "download_filename")
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

    let params = parse_json_object_arg(args, "params")?;

    // No field asks for `part` any more, so it is derived. An explicit value —
    // a node saved before the field went away, or a caller after a leaner
    // response — still wins.
    if spec.requires_part {
        let part = part_arg(spec, args)
            .or_else(|| part_arg(spec, &params))
            .unwrap_or_else(|| default_part(spec, args));
        query.push(("part".to_string(), part));
    }

    let chosen_filter = resolve_filter(spec, args, &params)?;
    let filter_key = chosen_filter.as_ref().map(|(key, _)| *key);
    if let Some((key, value)) = chosen_filter {
        query.push((key.to_string(), value));
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

    // Whatever else the caller wants to pass through. Filter keys are skipped —
    // the endpoint takes exactly one and `resolve_filter` already chose it, so
    // letting a leftover second filter through here would earn a 400.
    let filter_keys: Vec<&str> = filters(spec.tool).iter().map(|f| f.key).collect();
    for (k, v) in params {
        if v.is_null() || k == "filter_by" || filter_keys.contains(&k.as_str()) {
            continue;
        }
        if !query.iter().any(|(qk, _)| qk == &k) {
            query.push((k, value_to_query(&v)));
        }
    }

    for key in spec.required_query {
        if !query.iter().any(|(k, _)| k == key) {
            bail!("{} requires '{}'", spec.tool, key);
        }
    }

    reject_orphan_companions(spec, &query, filter_key)?;

    Ok(query)
}

/// Refuse a parameter whose companion field is unset or holds a value the API
/// will not pair it with. The form hides these, but an agent call or a node
/// saved before the rule existed can still carry one, and the API's own answer
/// ("invalid combination of search filters") names neither field.
fn reject_orphan_companions(
    spec: &ActionSpec,
    query: &[(String, String)],
    filter_key: Option<&'static str>,
) -> Result<()> {
    for companion in companions(spec.tool) {
        let sent = query
            .iter()
            .find(|(k, _)| k == companion.key)
            .map(|(_, v)| v.as_str());
        let Some(sent) = sent.filter(|v| !v.trim().is_empty()) else {
            continue;
        };

        let current = if companion.depends_on == "filter_by" {
            filter_key
        } else {
            query
                .iter()
                .find(|(k, _)| k == companion.depends_on)
                .map(|(_, v)| v.as_str())
        };
        if current.is_some_and(|value| companion.allowed.contains(&value)) {
            continue;
        }

        bail!(
            "{} cannot send '{}={}' here — {}. Set '{}' to {}, or leave '{}' unset.",
            spec.tool,
            companion.key,
            sent,
            companion.reason,
            companion.depends_on,
            companion.allowed.join(" or "),
            companion.key,
        );
    }
    Ok(())
}

/// Pick the single list filter the endpoint requires.
///
/// Prefers the dedicated `filter_by` choice. Nodes and agent calls written before
/// that field existed passed the filter straight through `params`, so a lone
/// filter key there is still honoured — but two are refused here rather than at
/// the API, where the message is far less clear.
fn resolve_filter(
    spec: &ActionSpec,
    args: &Map<String, Value>,
    params: &Map<String, Value>,
) -> Result<Option<(&'static str, String)>> {
    let action_filters = filters(spec.tool);
    if action_filters.is_empty() {
        return Ok(None);
    }

    let choices = || {
        action_filters
            .iter()
            .map(|f| f.key)
            .collect::<Vec<_>>()
            .join(", ")
    };

    let chosen = non_empty_string_arg(args, "filter_by").or_else(|| {
        params
            .get("filter_by")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });

    let filter = match chosen {
        Some(key) => action_filters
            .iter()
            .find(|f| f.key == key)
            .ok_or_else(|| {
                anyhow!(
                    "{} cannot filter by '{}'. Choose one of: {}",
                    spec.tool,
                    key,
                    choices()
                )
            })?,
        None => {
            let supplied: Vec<&Filter> = action_filters
                .iter()
                .filter(|f| filter_value(f, args, params).is_some())
                .collect();
            match supplied.as_slice() {
                [only] => *only,
                [] => bail!(
                    "{} needs one filter. Set 'filter_by' to one of: {}",
                    spec.tool,
                    choices()
                ),
                more => bail!(
                    "{} takes exactly one filter but {} are set ({}). Set 'filter_by' to the one you want.",
                    spec.tool,
                    more.len(),
                    more.iter().map(|f| f.key).collect::<Vec<_>>().join(", ")
                ),
            }
        }
    };

    if filter.flag {
        return Ok(Some((filter.key, "true".to_string())));
    }

    let value = filter_value(filter, args, params).ok_or_else(|| {
        anyhow!(
            "{} filter '{}' needs a value — {}",
            spec.tool,
            filter.key,
            filter.hint
        )
    })?;

    if !filter.values.is_empty() && !filter.values.contains(&value.as_str()) {
        bail!(
            "{} filter '{}' must be one of: {}",
            spec.tool,
            filter.key,
            filter.values.join(", ")
        );
    }

    Ok(Some((filter.key, value)))
}

/// The value a filter carries, from its own field or the `params` fallback.
fn filter_value(
    filter: &Filter,
    args: &Map<String, Value>,
    params: &Map<String, Value>,
) -> Option<String> {
    let raw = args.get(filter.key).or_else(|| params.get(filter.key))?;
    if filter.flag {
        // A flag is a checkbox: only an explicit true means "filter by this".
        return match raw {
            Value::Bool(true) => Some("true".to_string()),
            Value::String(s) if s.trim().eq_ignore_ascii_case("true") => Some("true".to_string()),
            _ => None,
        };
    }
    let rendered = value_to_query(raw);
    (!rendered.trim().is_empty()).then_some(rendered)
}

/// `part` arrives as a comma-separated string from agent calls and as an array
/// from the workflow multi-select; normalise both to the comma list the API wants.
///
/// The "all sections" sentinel expands here rather than at the picker, so a saved
/// node keeps fetching everything even after a section is added to the action.
/// Duplicates are dropped — `*` alongside a hand-picked section is a fair thing
/// to select, and the API 400s on a repeated part.
fn part_arg(spec: &ActionSpec, args: &Map<String, Value>) -> Option<String> {
    let raw: Vec<String> = match args.get("part")? {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => return None,
    };

    let mut parts: Vec<String> = Vec::new();
    for token in raw.iter().flat_map(|s| s.split(',')).map(str::trim) {
        if token.is_empty() {
            continue;
        }
        // "all" is what a model reaches for when the schema says '*'.
        let expanded: Vec<String> = if token == ALL_PARTS || token.eq_ignore_ascii_case("all") {
            all_parts(spec.tool)
                .iter()
                .map(|p| (*p).to_string())
                .collect()
        } else {
            vec![token.to_string()]
        };
        for part in expanded {
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
    }

    (!parts.is_empty()).then(|| parts.join(","))
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

/// A blank field is an unset field. The workflow form seeds every declared
/// property, so an untouched box arrives as `""` — treating that as a supplied
/// value turns optional arguments into spurious failures.
fn non_empty_string_arg(args: &Map<String, Value>, key: &str) -> Option<String> {
    opt_string_arg(args, key).filter(|s| !s.trim().is_empty())
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

    fn part_of(tool: &str, value: Value) -> String {
        build_query(spec(tool), &args(value))
            .expect("query")
            .iter()
            .find(|(k, _)| k == "part")
            .map(|(_, v)| v.clone())
            .expect("part")
    }

    // ── part ────────────────────────────────────────────────────────────────
    // No field asks for it. A read takes everything; a write takes what its body
    // carries; anything explicit still wins.

    #[test]
    fn a_read_with_no_part_asked_for_takes_every_section() {
        assert_eq!(
            part_of("gyoutube_channels_list", json!({ "filter_by": "mine" })),
            "snippet,contentDetails,statistics,status,brandingSettings,topicDetails,localizations"
        );
    }

    #[test]
    fn the_default_read_leaves_out_owner_only_sections() {
        // These 403 the whole request on a video you do not own, so "everything"
        // deliberately stops short of them.
        let part = part_of(
            "gyoutube_videos_list",
            json!({ "filter_by": "id", "id": "abc" }),
        );
        for owner_only in ["fileDetails", "processingDetails", "suggestions"] {
            assert!(!part.contains(owner_only), "{part}");
        }
        assert!(part.contains("statistics"), "{part}");
    }

    /// An explicit `part` outranks the derived one, in either shape it arrives in.
    /// (A node's own stale `part` never gets this far — `tool_args` drops config
    /// keys the schema no longer declares — but agent calls and the workflow
    /// expression path can still send one.)
    #[test]
    fn an_explicit_part_still_wins() {
        assert_eq!(
            part_of(
                "gyoutube_channels_list",
                json!({ "part": ["snippet", "statistics"], "filter_by": "mine" }),
            ),
            "snippet,statistics"
        );
        assert_eq!(
            part_of(
                "gyoutube_channels_list",
                json!({ "part": "snippet, statistics", "filter_by": "mine" }),
            ),
            "snippet,statistics"
        );
    }

    #[test]
    fn params_can_narrow_the_response() {
        // The escape hatch now that the field is gone: a caller that wants less
        // than everything says so in `params`.
        assert_eq!(
            part_of(
                "gyoutube_videos_list",
                json!({ "filter_by": "id", "id": "abc", "params": { "part": "snippet" } }),
            ),
            "snippet"
        );
    }

    #[test]
    fn the_all_sentinel_expands_and_never_repeats_a_section() {
        assert_eq!(
            part_of(
                "gyoutube_channels_list",
                json!({ "part": ["*", "snippet"], "filter_by": "mine" }),
            )
            .matches("snippet")
            .count(),
            1
        );
    }

    #[test]
    fn a_write_takes_the_sections_its_body_carries() {
        // Naming a section the body omits clears it on the resource, so the part
        // list is derived from the body rather than defaulted to everything.
        assert!(!offers_all_parts("gyoutube_videos_update"));
        assert_eq!(
            part_of(
                "gyoutube_videos_update",
                json!({ "body": { "id": "abc", "status": { "privacyStatus": "private" } } }),
            ),
            "status"
        );
    }

    #[test]
    fn a_write_counts_the_title_and_description_fields_as_snippet() {
        // They are folded into snippet after the query is built.
        assert_eq!(
            part_of(
                "gyoutube_videos_update",
                json!({ "body": { "id": "abc" }, "title": "New title" }),
            ),
            "snippet"
        );
    }

    #[test]
    fn channel_sections_reads_only_the_parts_that_endpoint_serves() {
        // `localizations` and `targeting` are channelSection resource properties
        // but not part values, so the old "everything" read 400'd every time.
        let part = part_of("gyoutube_channel_sections_list", json!({ "filter_by": "mine" }));
        assert_eq!(part, "snippet,contentDetails");

        for tool in [
            "gyoutube_channel_sections_list",
            "gyoutube_channel_sections_insert",
            "gyoutube_channel_sections_update",
        ] {
            let options = part_options(tool);
            assert!(!options.contains(&"localizations"), "{tool}: {options:?}");
            assert!(!options.contains(&"targeting"), "{tool}: {options:?}");
        }
    }

    #[test]
    fn a_write_with_nothing_to_go_on_falls_back_to_the_first_section() {
        assert_eq!(
            part_of("gyoutube_playlists_insert", json!({})),
            "snippet",
            "an empty write should still name a valid section"
        );
    }

    // ── List filters ────────────────────────────────────────────────────────
    // YouTube requires exactly one filter on these endpoints and answers with a
    // 400 "No filter selected" otherwise. Every case below is caught here first.

    #[test]
    fn the_filter_comes_pre_picked() {
        // "mine" needs nothing else, so a fresh node runs as-is. Where the action
        // has no flag filter the value is genuinely required — the API has no
        // "list them all" for those — so it leads with the first and opens its box.
        let channels = tool_from_spec(spec("gyoutube_channels_list"));
        assert_eq!(
            channels.input_schema["properties"]["filter_by"]["default"],
            "mine"
        );

        let videos = tool_from_spec(spec("gyoutube_videos_list"));
        assert_eq!(
            videos.input_schema["properties"]["filter_by"]["default"],
            "id"
        );
    }

    #[test]
    fn a_list_endpoint_without_a_filter_is_caught_before_the_api() {
        let err = build_query(
            spec("gyoutube_channels_list"),
            &args(json!({ "part": ["snippet"] })),
        )
        .expect_err("a filter is required")
        .to_string();
        assert!(err.contains("filter_by"), "{err}");
        assert!(err.contains("forHandle"), "{err}");
    }

    #[test]
    fn a_flag_filter_is_sent_as_true() {
        let query = build_query(
            spec("gyoutube_channels_list"),
            &args(json!({ "part": ["snippet"], "filter_by": "mine" })),
        )
        .expect("query");
        assert!(query.contains(&("mine".to_string(), "true".to_string())));
    }

    #[test]
    fn a_value_filter_reads_its_own_field() {
        let query = build_query(
            spec("gyoutube_channels_list"),
            &args(json!({ "part": ["snippet"], "filter_by": "forHandle", "forHandle": "@axon" })),
        )
        .expect("query");
        assert!(query.contains(&("forHandle".to_string(), "@axon".to_string())));
    }

    #[test]
    fn a_value_filter_with_nothing_entered_names_what_it_wants() {
        let err = build_query(
            spec("gyoutube_channels_list"),
            &args(json!({ "part": ["snippet"], "filter_by": "id", "id": "" })),
        )
        .expect_err("the filter needs a value")
        .to_string();
        assert!(err.contains("'id'"), "{err}");
        assert!(err.contains("UC"), "{err}");
    }

    #[test]
    fn unchosen_filters_never_reach_the_query() {
        // The form seeds every declared field, and older nodes left filters in
        // `params`. Sending a second one is the 400 this whole model prevents.
        let query = build_query(
            spec("gyoutube_channels_list"),
            &args(json!({
                "part": ["snippet"],
                "filter_by": "id",
                "id": "UC123",
                "forHandle": "",
                "params": { "mine": true },
            })),
        )
        .expect("query");
        assert!(query.contains(&("id".to_string(), "UC123".to_string())));
        assert!(!query.iter().any(|(k, _)| k == "mine"));
        assert!(!query.iter().any(|(k, _)| k == "forHandle"));
        assert!(!query.iter().any(|(k, _)| k == "filter_by"));
    }

    #[test]
    fn a_filter_left_in_params_by_an_older_node_still_works() {
        let query = build_query(
            spec("gyoutube_playlists_list"),
            &args(json!({ "part": ["snippet"], "params": { "channelId": "UC123" } })),
        )
        .expect("query");
        assert!(query.contains(&("channelId".to_string(), "UC123".to_string())));
    }

    #[test]
    fn two_filters_with_no_choice_made_is_refused_clearly() {
        let err = build_query(
            spec("gyoutube_playlists_list"),
            &args(json!({ "part": ["snippet"], "params": { "channelId": "UC1", "mine": true } })),
        )
        .expect_err("ambiguous")
        .to_string();
        assert!(err.contains("exactly one"), "{err}");
        assert!(err.contains("filter_by"), "{err}");
    }

    #[test]
    fn a_filter_the_endpoint_does_not_have_is_rejected() {
        let err = build_query(
            spec("gyoutube_channels_list"),
            &args(json!({ "part": ["snippet"], "filter_by": "playlistId" })),
        )
        .expect_err("not a channels filter")
        .to_string();
        assert!(err.contains("cannot filter by 'playlistId'"), "{err}");
    }

    #[test]
    fn the_content_partner_filter_is_not_offered() {
        // `managedByMe` is served only alongside `onBehalfOfContentOwner` under a
        // `youtubepartner` token. We send neither, so it answered 403 every time.
        let tool = tool_from_spec(spec("gyoutube_channels_list"));
        let choices = tool.input_schema["properties"]["filter_by"]["enum"]
            .as_array()
            .expect("enum")
            .clone();
        assert!(!choices.iter().any(|v| v == "managedByMe"), "{choices:?}");

        // A node saved while it was still on the list now stops here, with a
        // message naming the filters that work, instead of at the API with a 403.
        let err = build_query(
            spec("gyoutube_channels_list"),
            &args(json!({ "part": ["snippet"], "filter_by": "managedByMe" })),
        )
        .expect_err("no longer a channels filter")
        .to_string();
        assert!(err.contains("cannot filter by 'managedByMe'"), "{err}");
        assert!(err.contains("forHandle"), "{err}");
    }

    #[test]
    fn playlist_images_asks_which_playlist() {
        // playlistImages.list takes exactly one of playlistId/id like its
        // siblings. Declaring none sent a bare `part` and earned a 400.
        let err = build_query(spec("gyoutube_playlist_images_list"), &args(json!({})))
            .expect_err("a filter is required")
            .to_string();
        assert!(err.contains("playlistId"), "{err}");

        let query = build_query(
            spec("gyoutube_playlist_images_list"),
            &args(json!({ "filter_by": "playlistId", "playlistId": "PL123" })),
        )
        .expect("query");
        assert!(query.contains(&("playlistId".to_string(), "PL123".to_string())));
    }

    // ── Cross-field rules ───────────────────────────────────────────────────
    // Parameters the API takes only beside another one. The form hides them
    // until the companion is set; these pin the check that backs that up.

    #[test]
    fn a_video_only_search_filter_needs_type_video() {
        let err = build_query(
            spec("gyoutube_search_list"),
            &args(json!({ "q": "rust", "videoDuration": "long" })),
        )
        .expect_err("videoDuration needs type=video")
        .to_string();
        assert!(err.contains("videoDuration"), "{err}");
        assert!(err.contains("type"), "{err}");

        // The same call with the companion set goes through untouched.
        let query = build_query(
            spec("gyoutube_search_list"),
            &args(json!({ "q": "rust", "type": "video", "videoDuration": "long" })),
        )
        .expect("query");
        assert!(query.contains(&("videoDuration".to_string(), "long".to_string())));

        // Searching channels without touching the video-only boxes still works —
        // the form seeds them as "", which must not read as "supplied".
        let query = build_query(
            spec("gyoutube_search_list"),
            &args(json!({ "q": "rust", "type": "channel", "videoDuration": "", "eventType": "" })),
        )
        .expect("query");
        assert!(query.contains(&("type".to_string(), "channel".to_string())));
    }

    #[test]
    fn moderation_status_is_refused_beside_the_id_filter() {
        let err = build_query(
            spec("gyoutube_comment_threads_list"),
            &args(json!({
                "filter_by": "id",
                "id": "thread1",
                "moderationStatus": "heldForReview",
            })),
        )
        .expect_err("not allowed with the id filter")
        .to_string();
        assert!(err.contains("moderationStatus"), "{err}");

        let query = build_query(
            spec("gyoutube_comment_threads_list"),
            &args(json!({
                "filter_by": "videoId",
                "videoId": "vid1",
                "moderationStatus": "heldForReview",
            })),
        )
        .expect("query");
        assert!(query.contains(&(
            "moderationStatus".to_string(),
            "heldForReview".to_string()
        )));
    }

    #[test]
    fn the_memberships_actions_name_the_scope_they_are_missing() {
        // Neither can work until `auth::SCOPES` asks for the creator scope, so
        // they say so up front instead of spending a call on a 403.
        let scope = missing_scope("gyoutube_members_list").expect("scope is not requested");
        assert!(scope.ends_with("youtube.channel-memberships.creator"), "{scope}");
        assert!(missing_scope("gyoutube_memberships_levels_list").is_some());

        // Every other action runs on scopes the token already carries — a stray
        // guard here would take a working action offline.
        for spec in ACTIONS.iter().filter(|s| {
            s.tool != "gyoutube_members_list" && s.tool != "gyoutube_memberships_levels_list"
        }) {
            assert!(missing_scope(spec.tool).is_none(), "{}", spec.tool);
        }
    }

    #[test]
    fn a_companion_gated_field_is_hidden_until_its_companion_is_set() {
        let tool = tool_from_spec(spec("gyoutube_search_list"));
        let properties = tool.input_schema.get("properties").expect("properties");
        assert_eq!(
            properties["videoDuration"]["displayOptions"]["show"]["type"],
            json!(["video"])
        );
        // `order` pairs with nothing, so it stays unconditional.
        assert!(properties["order"].get("displayOptions").is_none());
    }

    #[test]
    fn every_companion_names_fields_the_action_really_has() {
        // A typo'd key or companion would silently never fire.
        for spec in ACTIONS {
            let enum_keys: Vec<&str> = query_enums(spec.tool).iter().map(|(k, _)| *k).collect();
            for companion in companions(spec.tool) {
                assert!(
                    enum_keys.contains(&companion.key),
                    "{}: companion '{}' is not one of its fields",
                    spec.tool,
                    companion.key
                );
                if companion.depends_on == "filter_by" {
                    let filter_keys: Vec<&str> =
                        filters(spec.tool).iter().map(|f| f.key).collect();
                    for allowed in companion.allowed {
                        assert!(
                            filter_keys.contains(allowed),
                            "{}: '{allowed}' is not one of its filters",
                            spec.tool
                        );
                    }
                } else {
                    let (_, values) = query_enums(spec.tool)
                        .iter()
                        .find(|(k, _)| *k == companion.depends_on)
                        .unwrap_or_else(|| panic!("{}: no '{}'", spec.tool, companion.depends_on));
                    for allowed in companion.allowed {
                        assert!(
                            values.contains(allowed),
                            "{}: '{}' cannot be '{allowed}'",
                            spec.tool,
                            companion.depends_on
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_fixed_value_filter_validates_its_value() {
        let ok = build_query(
            spec("gyoutube_videos_list"),
            &args(json!({ "part": ["snippet"], "filter_by": "chart", "chart": "mostPopular" })),
        )
        .expect("query");
        assert!(ok.contains(&("chart".to_string(), "mostPopular".to_string())));

        let err = build_query(
            spec("gyoutube_videos_list"),
            &args(json!({ "part": ["snippet"], "filter_by": "chart", "chart": "trending" })),
        )
        .expect_err("not a chart")
        .to_string();
        assert!(err.contains("mostPopular"), "{err}");
    }

    #[test]
    fn actions_without_filters_are_untouched() {
        // search.list needs no filter; requiring one would break every search.
        let query = build_query(
            spec("gyoutube_search_list"),
            &args(json!({ "part": ["snippet"], "q": "rust" })),
        )
        .expect("query");
        assert!(query.contains(&("q".to_string(), "rust".to_string())));
    }

    #[test]
    fn filter_fields_are_gated_on_the_choice() {
        let tool = tool_from_spec(spec("gyoutube_channels_list"));
        let properties = tool.input_schema.get("properties").expect("properties");

        // The choice itself lists every filter, flags included.
        let choices = properties["filter_by"]["enum"].as_array().expect("enum");
        assert!(choices.iter().any(|v| v == "mine"));
        assert!(choices.iter().any(|v| v == "forHandle"));

        // A value filter gets a field, shown only when it is the one chosen.
        assert_eq!(
            properties["forHandle"]["displayOptions"]["show"]["filter_by"],
            json!(["forHandle"])
        );
        assert_eq!(properties["forHandle"]["title"], "Handle");

        // A flag filter carries no value, so it gets no field of its own.
        assert!(properties.get("mine").is_none());
    }

    #[test]
    fn paging_fields_only_appear_where_the_endpoint_pages() {
        let paged = tool_from_spec(spec("gyoutube_playlist_items_list"));
        let paged = paged.input_schema.get("properties").expect("properties");
        assert!(paged.get("maxResults").is_some());
        assert!(paged.get("pageToken").is_some());

        // i18nLanguages returns its whole catalogue at once and rejects paging.
        let whole = tool_from_spec(spec("gyoutube_i18n_languages_list"));
        let whole = whole.input_schema.get("properties").expect("properties");
        assert!(whole.get("maxResults").is_none());
    }

    #[test]
    fn filter_keys_never_collide_with_the_actions_other_fields() {
        // A key owned by two mechanisms would be emitted twice and sent twice.
        for spec in ACTIONS {
            let filter_keys: Vec<&str> = filters(spec.tool).iter().map(|f| f.key).collect();
            for key in dedicated_query_keys(spec) {
                assert!(
                    !filter_keys.contains(&key),
                    "{}: '{key}' is both a filter and a dedicated query field",
                    spec.tool
                );
            }
            if !filter_keys.is_empty() {
                assert!(
                    spec.method == "GET",
                    "{}: filters only make sense on reads",
                    spec.tool
                );
            }
        }
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
    fn no_action_asks_for_part() {
        // The form dropped the field; the schema has to agree, or the node keeps
        // rendering it and `tool_args` keeps forwarding a stale saved value.
        for spec in ACTIONS.iter().filter(|s| s.requires_part) {
            let tool = tool_from_spec(spec);
            let properties = tool.input_schema["properties"]
                .as_object()
                .expect("properties");
            assert!(!properties.contains_key("part"), "{} kept it", spec.tool);
            let required = tool.input_schema["required"].as_array().expect("required");
            assert!(!required.iter().any(|v| v == "part"), "{}", spec.tool);
        }
    }

    #[test]
    fn every_read_names_a_part_the_api_accepts() {
        // A derived part that is empty or not on the action's list is a 400.
        for spec in ACTIONS.iter().filter(|s| s.requires_part) {
            let derived = default_part(spec, &Map::new());
            assert!(!derived.is_empty(), "{} derived nothing", spec.tool);
            for section in derived.split(',') {
                assert!(
                    part_options(spec.tool).contains(&section),
                    "{} derived '{section}', which it does not accept",
                    spec.tool
                );
            }
        }
    }

    #[test]
    fn blank_optional_arguments_count_as_unset() {
        // The form seeds every declared property, so untouched boxes arrive as "".
        // Treating those as supplied made read actions fail the upload guard.
        let blank = args(json!({ "upload_file_path": "  ", "title": "" }));
        assert!(non_empty_string_arg(&blank, "upload_file_path").is_none());
        assert!(non_empty_string_arg(&blank, "title").is_none());

        let filled = args(json!({ "title": "Real" }));
        assert_eq!(
            non_empty_string_arg(&filled, "title").as_deref(),
            Some("Real")
        );
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
