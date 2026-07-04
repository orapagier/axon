use super::runner::run_python;
use super::schema::{ToolDefinition, ToolSource};
use anyhow::Context;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Internal tools registered for the workflow builder (they appear as nodes in
/// the UI) but kept out of the agent's callable set by
/// [`ToolRegistry::all_enabled_for_agent`]. Two reasons land a tool here:
///
///   • Trigger tools (`*_trigger`) register webhooks rather than perform a
///     one-shot action, so they have no `handle_internal` arm — offering them
///     would let the agent pick a tool that fails with "Unknown internal tool".
///
///   • Messaging tools (`telegram`/`whatsapp`) DO have `handle_internal` arms,
///     but by design messaging platforms are user↔agent chat gateways (the
///     `messaging/` module) and any agent-initiated sending is expressed as a
///     workflow node — never an agent send-tool. The arms stay for the workflow
///     generic-tool-node path; only the agent is denied them here.
///
/// Either way the full set is still served to the UI via [`all`], so their
/// workflow-node counterparts (dispatched in `workflow.rs`) are unaffected.
const NON_AGENT_INTERNAL_TOOLS: &[&str] = &[
    "telegram_trigger",
    "whatsapp_trigger",
    "telegram",
    "whatsapp",
];

/// Social-platform *write* tools that perform outward-facing, public,
/// hard-to-reverse actions (publish/edit/delete posts, reply/moderate comments,
/// react, send DMs, edit Page settings). This is the standing pattern for social
/// integrations: writes are workflow-only, reads stay on the agent. They are
/// deliberately kept out of the agent's callable set so every public action flows
/// through a defined, reviewable **workflow** path instead of the agent's ad-hoc
/// discretion. The agent keeps the read tools (`*_get_*`, `*_list_*`,
/// `fb_recent_comments`, `*_insights`) for answering questions conversationally.
/// These names are still served to the UI via [`all`] and dispatched by name via
/// [`run`], so the Facebook/Instagram workflow nodes are unaffected — only the
/// agent is denied them here. Matched by name regardless of `ToolSource` (these are
/// in-process MCP tools, not `Internal`). New social platforms follow suit.
const WORKFLOW_ONLY_WRITE_TOOLS: &[&str] = &[
    // ── Facebook ──
    // Page
    "fb_update_page",
    // Posts
    "fb_create_post",
    "fb_create_post_with_image",
    "fb_create_post_with_video",
    "fb_update_post",
    "fb_delete_post",
    "fb_schedule_post",
    // Comments / reactions
    "fb_reply_to_comment",
    "fb_delete_comment",
    "fb_hide_comment",
    "fb_like_object",
    "fb_react_object",
    "fb_unreact_object",
    // Messenger
    "fb_send_message",
    "fb_send_message_image",
    // ── Instagram ──
    // Media
    "ig_create_image_post",
    "ig_create_video_reel",
    "ig_create_post", // back-compat alias for older workflow nodes
    // Comments
    "ig_reply_to_comment",
    // DMs
    "ig_send_message",
];

/// CRM *write* tools (create/update/delete/convert/archive/restore). Follows
/// the same standing pattern as [`WORKFLOW_ONLY_WRITE_TOOLS`]: mutations flow
/// through the deliberate workflow path while the agent keeps the read tools
/// (list/get/search/overview/pipeline/dashboard/export) for answering
/// questions conversationally. Unlike the social list this gate is *soft* —
/// the operator can grant the agent full CRM read/write by turning on the
/// `crm.agent_write_tools` setting (Settings → CRM); callers of
/// [`ToolRegistry::all_enabled_for_agent`] pass that setting in. Workflow
/// nodes always reach every CRM tool via [`ToolRegistry::all`] / `run`.
const CRM_WRITE_TOOLS: &[&str] = &[
    // Leads
    "crm_lead_create",
    "crm_lead_update",
    "crm_lead_delete",
    "crm_lead_convert_to_deal",
    // Deals
    "crm_deal_create",
    "crm_deal_update",
    "crm_deal_delete",
    // Organizations
    "crm_org_create",
    "crm_org_update",
    "crm_org_delete",
    // Activities
    "crm_activity_log",
    "crm_activity_update",
    "crm_activity_delete",
    // Archive lifecycle
    "crm_record_archive",
    "crm_record_restore",
];

fn internal_tools() -> Vec<ToolDefinition> {
    vec![
        {
            let mut d = ToolDefinition::internal(
                "update_plan",
                "Create or update your step-by-step plan for a multi-step task. Call this FIRST with a numbered list of steps, then again each time you finish a step — always send the FULL list, marking finished steps with status \"done\". Give your final answer only when every step is done or explicitly skipped (say why).",
                serde_json::json!({
                    "steps": {
                        "type": "array",
                        "description": "The full plan, every call. Each item: {step, status}.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step":   {"type": "string", "description": "What this step accomplishes"},
                                "status": {"type": "string", "enum": ["pending", "done"], "default": "pending"}
                            },
                            "required": ["step"]
                        }
                    }
                }),
                vec!["steps".into()],
            );
            // Bookkeeping, not a side effect: must never satisfy the claim
            // guard's "successful mutating execution" receipt (the name-derived
            // default classifies the `update` verb as mutating).
            d.is_mutating = false;
            d
        },
        ToolDefinition::internal(
            "search_tools",
            "Search the full tool registry by keywords when the capability you need is not in your current tool list (e.g. 'send email attachment', 'calendar events', 'upload drive', 'facebook post'). Matching tools are attached to this conversation immediately, so you can call them in your next step. Always search before claiming a capability is unavailable.",
            serde_json::json!({
                "query": {"type": "string", "description": "Keywords describing the capability, e.g. 'send email' or 'list calendar events'"},
                "max_results": {"type": "integer", "default": 5, "description": "Max matching tools to attach (up to 10)"}
            }),
            vec!["query".into()],
        ),
        ToolDefinition::internal(
            "cron_job_tool",
            "Create, edit, pause, resume, or delete a scheduled periodic cron job. IMPORTANT: ALWAYS use this tool to schedule cron jobs, periodic tasks, or scheduled follow-ups. NEVER attempt to manually edit crontabs or systemd timers via ssh_tool.",
            serde_json::json!({
                "action":       {"type":"string","enum":["create","edit","pause","resume","delete","list"]},
                "job_id":       {"type":"string","description":"ID of the job (required for edit/pause/resume/delete)", "displayOptions": {"show": {"action": ["edit", "pause", "resume", "delete"]}}},
                "name":         {"type":"string", "displayOptions": {"show": {"action": ["create"]}}},
                "task":         {"type":"string", "displayOptions": {"show": {"action": ["create"]}}},
                "schedule_nl":  {"type":"string", "displayOptions": {"show": {"action": ["create"]}}},
                "new_name":     {"type":"string","description":"New name (for edit)", "displayOptions": {"show": {"action": ["edit"]}}},
                "new_task":     {"type":"string","description":"New task description (for edit)", "displayOptions": {"show": {"action": ["edit"]}}},
                "new_schedule": {"type":"string","description":"New schedule in plain English (for edit)", "displayOptions": {"show": {"action": ["edit"]}}},
                "stop_condition":{"type":"object","properties":{
                    "condition_type":{"type":"string"},"value":{"type":"string"}}, "displayOptions": {"show": {"action": ["create", "edit"]}}}
            }),
            vec!["action".into()],
        ),
        ToolDefinition::internal(
            "agent_memory_tool",
            "Store or retrieve information from long-term memory.",
            serde_json::json!({
                "action":   {"type":"string","enum":["store","search","delete"]},
                "content":  {"type":"string", "displayOptions": {"show": {"action": ["store", "search"]}}},
                "tags":     {"type":"array","items":{"type":"string"}, "displayOptions": {"show": {"action": ["store", "search"]}}},
                "memory_id":{"type":"integer", "displayOptions": {"show": {"action": ["delete"]}}}
            }),
            vec!["action".into(), "content".into()],
        ),
        ToolDefinition::internal(
            "ssh_tool",
            "Execute bash commands and transfer files on REMOTE/EXTERNAL Linux servers via SSH/SFTP. Use this ONLY when you need to connect to OTHER servers over the network. DO NOT use this to check the local server where you are hosted.",
            serde_json::json!({
                "action": {"type": "string", "enum": ["run", "upload_file", "download_file", "list_servers"], "description": "Action to perform"},
                "command": {"type": "string", "description": "The BASH command to execute on the remote server.", "displayOptions": {"show": {"action": ["run"]}}},
                "server_name": {"type": "string", "description": "Server name from config", "displayOptions": {"show": {"action": ["run", "upload_file", "download_file"]}}},
                "remote_path": {"type": "string", "description": "Remote file path", "displayOptions": {"show": {"action": ["upload_file", "download_file"]}}},
                "local_path": {"type": "string", "description": "Local file path for upload/download", "displayOptions": {"show": {"action": ["upload_file", "download_file"]}}},
                "timeout_seconds": {"type": "integer", "description": "Optional max execution time in seconds. Defaults to 30. Use exactly 5 max for continuous commands like `top` or `ping`, and 120+ for installations/builds.", "displayOptions": {"show": {"action": ["run"]}}}
            }),
            vec!["action".into()],
        ),
        ToolDefinition::internal(
            "parallel_worker",
            "Execute multiple independent sub-tasks in parallel iterations. Use this specifically to speed up execution when tasks don't depend on each other (e.g. checking 5 different servers, analyzing 3 different files concurrently).",
            serde_json::json!({
                "sub_tasks": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "A list of standalone task descriptions to run in parallel."
                }
            }),
            vec!["sub_tasks".into()],
        ),

        ToolDefinition::internal(
            "shell_tool",
            "Execute native bash commands directly on the LOCAL server hosting the agent. \
            Use this ONLY for checking your own underlying system (e.g. your local RAM, disk, processes). DO NOT use this for external servers. \
            CRITICAL RESTRICTION: You MUST NOT execute destructive commands that alter server permissions, lock out users, or damage the system (e.g., rm -rf /, chmod, chown, iptables, ufw, passwd, mkfs). \
            These commands are strictly blocked and will fail. \
            Use this for read-only local system checks, standard file operations, or running scripts locally.",
            serde_json::json!({
                "command": {"type": "string", "description": "The bash command to execute."},
                "timeout_seconds": {"type": "integer", "description": "Optional max execution time in seconds. Defaults to 30. Use exactly 5 max for continuous commands like `top` or `ping`, and 120+ for installations/builds."}
            }),
            vec!["command".into()],
        ),
        ToolDefinition::internal(
            "web_search",
            "Search the web using Tavily. Automatically rotates through configured API accounts. Use this to look up current information, news, prices, or anything not in your training data.",
            serde_json::json!({
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "top_n": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10,
                    "default": 5,
                    "description": "Number of results (max 10)"
                }
            }),
            vec!["query".into()],
        ),
        ToolDefinition::internal(
            "watcher_tool",
            "Add, edit, pause, resume, or delete a command/task watcher. \
             A watcher runs a shell command on a schedule and fires an agent task when a \
             trigger condition is met (e.g. output changed, output contains a keyword, command failed). \
             ALWAYS use this tool to set up automated command monitoring â€” never use cron_job_tool for watchers.",
            serde_json::json!({
                "action": {
                    "type": "string",
                    "enum": ["add","edit","pause","resume","delete","list"],
                    "description": "Action to perform"
                },
                "watcher_id": {
                    "type": "string",
                    "description": "ID of the watcher (required for edit/pause/resume/delete)",
                    "displayOptions": {"show": {"action": ["edit", "pause", "resume", "delete"]}}
                },
                "name": {"type": "string", "description": "Human-readable name for this watcher", "displayOptions": {"show": {"action": ["add"]}}},
                "watch_command": {
                    "type": "string",
                    "description": "Shell command to run on each poll (e.g. 'df -h /', 'cat /var/log/app.log | tail -5')",
                    "displayOptions": {"show": {"action": ["add", "edit"]}}
                },
                "trigger_task": {
                    "type": "string",
                    "description": "What the agent should do when the trigger condition is met",
                    "displayOptions": {"show": {"action": ["add", "edit"]}}
                },
                "schedule_nl": {
                    "type": "string",
                    "description": "How often to poll, in plain English (e.g. 'every 5 minutes')",
                    "displayOptions": {"show": {"action": ["add", "edit"]}}
                },
                "trigger_condition": {
                    "type": "string",
                    "description": "When to fire: 'always' | 'output_changed' | 'exit_nonzero' | 'output_contains:<value>'",
                    "displayOptions": {"show": {"action": ["add", "edit"]}}
                }
            }),
            vec!["action".into()],
        ),
        crate::tools::http::tool_definition(),
        crate::tools::http::list_saved_tool_definition(),
        crate::tools::http::run_saved_tool_definition(),
        // Workflow tools
        ToolDefinition::internal(
            "list_workflows",
            "List all configured workflows. Returns workflow names, trigger types, node counts, and last status.",
            json!({}),
            vec![],
        ),
        ToolDefinition::internal(
            "run_workflow",
            "Execute a saved workflow by name or ID. The workflow will run all its nodes sequentially, passing data between them. Returns the final node's output.",
            json!({
                "name_or_id": {
                    "type": "string",
                    "description": "The name or ID of the workflow to execute"
                }
            }),
            vec!["name_or_id".into()],
        ),
        crate::tools::telegram::tool_definition(),
        crate::tools::telegram::trigger_tool_definition(),
        crate::tools::whatsapp::tool_definition(),
        crate::tools::whatsapp::trigger_tool_definition(),
        crate::tools::myelin::tool_definition(),
                ToolDefinition::internal(
            "image_tool",
            "Process images and create videos. Actions: \
             'process' (pipeline with resize/crop/filters/text/overlay steps), \
             'quote_image' (text overlay on background), \
             'filters' (apply blur/sharpen/sepia/etc), \
             'info' (get dimensions/EXIF/dominant color), \
             'video' (create video from image+audio, requires ffmpeg), \
             'slideshow' (multi-image video, requires ffmpeg).",
            image_tool_parameters(),
            vec!["action".into()],
        ),
    ]
}

fn image_tool_parameters() -> serde_json::Value {
    let mut props = serde_json::Map::new();

    props.insert(
        "action".into(),
        json!({
            "type": "string",
            "enum": ["process", "quote_image", "filters", "info", "video", "slideshow"],
            "description": "Which image operation to perform"
        }),
    );
    props.insert(
        "input".into(),
        json!({
            "type": "string",
            "description": "Input image file path (required for process/quote_image/filters/info)",
            "displayOptions": {"show": {"action": ["process", "quote_image", "filters", "info"]}}
        }),
    );
    props.insert(
        "output".into(),
        json!({
            "type": "string",
            "description": "Output file path",
            "displayOptions": {"show": {"action": ["process", "quote_image", "filters"]}}
        }),
    );
    props.insert(
        "output_filename".into(),
        json!({
            "type": "string",
            "description": "Output filename (saved to data/files/). Extension auto-detected from preset if omitted (e.g. 'devotional' -> 'devotional.mp4'). Reuse the same name to overwrite previous renders.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "slideshow_image_source".into(),
        json!({
            "type": "string",
            "enum": ["folder", "upstream"],
            "description": "For 'slideshow': choose whether images come from a data/files folder or an explicit images array.",
            "displayOptions": {"show": {"action": ["slideshow"]}}
        }),
    );
    props.insert(
        "image_folder".into(),
        json!({
            "type": "string",
            "description": "For 'slideshow': folder under data/files to scan recursively for images.",
            "displayOptions": {"show": {"action": ["slideshow"]}}
        }),
    );
    props.insert(
        "steps".into(),
        json!({
            "type": "array",
            "items": {"type": "object"},
            "description": "For 'process': array of {op, ...params}. Ops: resize, resize_fit, resize_fill, crop, crop_center, pad, rotate, flip_horizontal, flip_vertical, blur, sharpen, brightness, contrast, grayscale, sepia, invert, saturation, hue_rotate, vignette, color_overlay, gradient_overlay, rounded_corners",
            "displayOptions": {"show": {"action": ["process"]}}
        }),
    );
    props.insert(
        "text".into(),
        json!({
            "type": "string",
            "description": "For 'quote_image': main text to render",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "alignment".into(),
        json!({
            "type": "string",
            "enum": ["left", "center", "right"],
            "description": "For 'quote_image': horizontal alignment for the main quote text.",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "font_color".into(),
        json!({
            "type": "string",
            "description": "For 'quote_image': optional main quote font color override in #RRGGBB or #RRGGBBAA format.",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "attribution".into(),
        json!({
            "type": "string",
            "description": "For 'quote_image': attribution line",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "attribution_alignment".into(),
        json!({
            "type": "string",
            "enum": ["left", "center", "right"],
            "description": "For 'quote_image': horizontal alignment for the attribution text.",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "attribution_font_color".into(),
        json!({
            "type": "string",
            "description": "For 'quote_image': optional attribution font color override in #RRGGBB or #RRGGBBAA format.",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "font_path".into(),
        json!({
            "type": "string",
            "description": "Path to TTF font file",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "attribution_font_path".into(),
        json!({
            "type": "string",
            "description": "Path to TTF font file for the attribution line. Leave empty to reuse the main font.",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "font_size".into(),
        json!({
            "type": "number",
            "description": "Override auto font size",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "attribution_font_size".into(),
        json!({
            "type": "number",
            "description": "Override auto attribution font size",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "overlay_image_path".into(),
        json!({
            "type": "string",
            "description": "For 'quote_image': optional path or URL for an image/icon/logo overlay.",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "overlay_position".into(),
        json!({
            "type": "string",
            "enum": ["top-left", "top-center", "top-right", "bottom-left", "bottom-center", "bottom-right"],
            "description": "For 'quote_image': anchor position for the overlay image.",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "additional_texts".into(),
        json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "position": { "type": "string" },
                    "alignment": { "type": "string" },
                    "font_color": { "type": "string" },
                    "font_size": { "type": "number" }
                }
            },
            "description": "For 'quote_image': optional extra text overlays with per-item position, alignment, color, and size settings.",
            "displayOptions": {"show": {"action": ["quote_image"]}}
        }),
    );
    props.insert(
        "width".into(),
        json!({
            "type": "integer",
            "description": "Target width for image processing operations",
            "displayOptions": {"show": {"action": ["process"]}}
        }),
    );
    props.insert(
        "height".into(),
        json!({
            "type": "integer",
            "description": "Target height for image processing operations",
            "displayOptions": {"show": {"action": ["process"]}}
        }),
    );
    props.insert(
        "filters".into(),
        json!({
            "type": "array",
            "items": {"type": "object"},
            "description": "For 'filters': array of {name, ...params}. Names: blur, sharpen, brightness, contrast, grayscale, sepia, invert, saturation, hue_rotate, vignette",
            "displayOptions": {"show": {"action": ["filters"]}}
        }),
    );
    props.insert(
        "image_path".into(),
        json!({
            "type": "string",
            "description": "For 'video': still image path",
            "displayOptions": {"show": {"action": ["video"]}}
        }),
    );
    props.insert(
        "audio_path".into(),
        json!({
            "type": "string",
            "description": "For 'video'/'slideshow': audio track path",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "images".into(),
        json!({
            "type": "array",
            "items": {"type": "string"},
            "description": "For 'slideshow': array of image file paths",
            "displayOptions": {"show": {"action": ["slideshow"]}}
        }),
    );
    props.insert(
        "slide_duration_secs".into(),
        json!({
            "type": "number",
            "description": "For 'slideshow': seconds per slide (default 5)",
            "displayOptions": {"show": {"action": ["slideshow"]}}
        }),
    );
    props.insert(
        "preset".into(),
        json!({
            "type": "string",
            "enum": ["workflow_default", "social_media", "instagram_reel", "high_quality", "web_stream"],
            "description": "Video quality preset (default: workflow_default)",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "encoder_preset".into(),
        json!({
            "type": "string",
            "enum": ["ultrafast", "superfast", "veryfast", "faster", "fast", "medium", "slow", "slower", "veryslow", "placebo"],
            "description": "Manual x264/x265 encoder preset override. Leave empty to keep the selected quality preset value.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "crf".into(),
        json!({
            "type": "integer",
            "description": "Manual CRF quality override. Lower is better quality (larger files).",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "target_resolution".into(),
        json!({
            "type": "string",
            "enum": ["", "1280x720", "1920x1080", "1080x1920", "1080x1080", "720x1280", "source", "custom"],
            "description": "Output resolution override. 'source' keeps original dimensions. 'custom' uses target_width/target_height.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "target_width".into(),
        json!({
            "type": "integer",
            "description": "Custom output width (used when target_resolution is custom).",
            "displayOptions": {"show": {"action": ["video", "slideshow"], "target_resolution": ["custom"]}}
        }),
    );
    props.insert(
        "target_height".into(),
        json!({
            "type": "integer",
            "description": "Custom output height (used when target_resolution is custom).",
            "displayOptions": {"show": {"action": ["video", "slideshow"], "target_resolution": ["custom"]}}
        }),
    );
    props.insert(
        "fps".into(),
        json!({
            "type": "integer",
            "description": "Frames per second override.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "video_bitrate".into(),
        json!({
            "type": "string",
            "description": "Average video bitrate override (e.g., 3500k). Use 'none'/'auto' to clear.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "audio_bitrate".into(),
        json!({
            "type": "string",
            "description": "Audio bitrate override (e.g., 128k, 192k).",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "keyframe_interval".into(),
        json!({
            "type": "integer",
            "description": "Keyframe interval in frames.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "max_bitrate".into(),
        json!({
            "type": "string",
            "description": "Peak bitrate cap override (e.g., 3500k). Use 'none'/'auto' to clear.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "buf_size".into(),
        json!({
            "type": "string",
            "description": "VBV buffer size override (e.g., 7000k). Use 'none'/'auto' to clear.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "video_codec".into(),
        json!({
            "type": "string",
            "enum": ["h264", "h265", "vp9", "copy"],
            "description": "Video codec override.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "audio_codec".into(),
        json!({
            "type": "string",
            "enum": ["aac", "mp3", "opus", "copy"],
            "description": "Audio codec override.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "container".into(),
        json!({
            "type": "string",
            "enum": ["mp4", "webm", "mkv", "mov"],
            "description": "Output container override.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "pixel_format".into(),
        json!({
            "type": "string",
            "enum": ["yuv420p", "yuv444p", "yuva420p"],
            "description": "Pixel format override.",
            "displayOptions": {"show": {"action": ["video", "slideshow"]}}
        }),
    );
    props.insert(
        "format".into(),
        json!({
            "type": "string",
            "enum": ["png", "jpeg", "webp"],
            "description": "Output format override",
            "displayOptions": {"show": {"action": ["process", "quote_image", "filters"]}}
        }),
    );

    serde_json::Value::Object(props)
}

#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<RwLock<HashMap<String, ToolDefinition>>>,
    timeout_sec: u64,
    pub mcp_manager: Option<Arc<crate::mcp::McpManager>>,
}

impl ToolRegistry {
    pub async fn new(dir: &str, timeout_sec: u64) -> anyhow::Result<Self> {
        let r = ToolRegistry {
            tools: Arc::new(RwLock::new(HashMap::new())),
            timeout_sec,
            mcp_manager: None,
        };
        r.load_dir(dir).await?;
        r.load_internal().await;
        Ok(r)
    }

    pub fn set_mcp_manager(&mut self, mcp: Arc<crate::mcp::McpManager>) {
        self.mcp_manager = Some(mcp);
    }
    pub async fn load_internal(&self) {
        let mut m = self.tools.write().await;
        for mut t in internal_tools() {
            if t.name == "image_tool" {
                if let Some(font_files) = crate::tools::image_tool::discover_fonts() {
                    if let Some(props) = t.parameters.get_mut("properties") {
                        if let Some(font_path) = props.get_mut("font_path") {
                            font_path["enum"] = serde_json::json!(font_files);
                        }
                        if let Some(attr_font_path) = props.get_mut("attribution_font_path") {
                            attr_font_path["enum"] = serde_json::json!(font_files);
                        }
                    }
                }
            }
            m.insert(t.name.clone(), t);
        }
    }
    pub async fn load_dir(&self, dir: &str) -> anyhow::Result<()> {
        let path = Path::new(dir);
        if !path.exists() {
            return Ok(());
        }
        let mut m = self.tools.write().await;
        let mut n = 0usize;
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let fp = entry.path();
            if fp.extension().and_then(|e| e.to_str()) != Some("py") {
                continue;
            }
            if fp
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('_'))
                .unwrap_or(false)
            {
                continue;
            }
            match ToolDefinition::from_python_file(fp.to_str().unwrap_or("")) {
                Ok(def) => {
                    tracing::info!("Registered tool: {}", def.name);
                    m.insert(def.name.clone(), def);
                    n += 1;
                }
                Err(e) => tracing::warn!("Skipping {:?}: {}", fp, e),
            }
        }
        tracing::info!("Loaded {} tools from {}", n, dir);
        Ok(())
    }
    pub async fn register(&self, def: ToolDefinition) {
        self.tools.write().await.insert(def.name.clone(), def);
    }
    pub async fn remove(&self, name: &str) {
        self.tools.write().await.remove(name);
    }
    pub async fn all(&self) -> Vec<ToolDefinition> {
        self.tools.read().await.values().cloned().collect()
    }
    pub async fn all_enabled(&self) -> Vec<ToolDefinition> {
        self.tools
            .read()
            .await
            .values()
            .filter(|t| t.enabled)
            .cloned()
            .collect()
    }
    /// Enabled tools the agent is allowed to *call*. This is a subset of
    /// [`all_enabled`] that drops three groups:
    ///   • internal tools that exist only as workflow-builder nodes (e.g. webhook
    ///     triggers) and have no `handle_internal` dispatch arm — offering them
    ///     would let the agent pick a tool that fails with "Unknown internal tool";
    ///   • social-platform outward-facing write actions (Facebook/Instagram —
    ///     [`WORKFLOW_ONLY_WRITE_TOOLS`]), restricted to the deliberate workflow path;
    ///   • CRM write tools ([`CRM_WRITE_TOOLS`]) unless the operator granted the
    ///     agent CRM writes (`crm.agent_write_tools` setting → `allow_crm_writes`).
    /// The full set is still served to the UI via [`all`] and dispatched by name
    /// via [`run`], so the workflow node palette and execution are unaffected.
    pub async fn all_enabled_for_agent(&self, allow_crm_writes: bool) -> Vec<ToolDefinition> {
        self.tools
            .read()
            .await
            .values()
            .filter(|t| t.enabled)
            .filter(|t| {
                !(t.source == ToolSource::Internal
                    && NON_AGENT_INTERNAL_TOOLS.contains(&t.name.as_str()))
            })
            // Outward-facing Facebook write actions are workflow-only — never
            // exposed to the agent (workflow nodes still reach them via `all`/`run`).
            .filter(|t| !WORKFLOW_ONLY_WRITE_TOOLS.contains(&t.name.as_str()))
            .filter(|t| allow_crm_writes || !CRM_WRITE_TOOLS.contains(&t.name.as_str()))
            .cloned()
            .collect()
    }
    pub async fn get(&self, name: &str) -> Option<ToolDefinition> {
        self.tools.read().await.get(name).cloned()
    }
    pub async fn run(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let def = self
            .tools
            .read()
            .await
            .get(name)
            .cloned()
            .with_context(|| format!("Tool '{}' not found", name))?;
        match &def.source {
            ToolSource::Python { path } | ToolSource::Temp { path } => {
                run_python(path, args, self.timeout_sec).await
            }
            ToolSource::Internal => anyhow::bail!("Internal tool '{}' handled by agent loop", name),
            ToolSource::Mcp {
                server_name,
                tool_name,
            } => {
                if let Some(mcp) = &self.mcp_manager {
                    mcp.call(server_name, tool_name, args).await
                } else {
                    anyhow::bail!("MCP manager not initialized");
                }
            }
        }
    }

    pub async fn set_enabled(&self, name: &str, enabled: bool) {
        let mut m = self.tools.write().await;
        if let Some(t) = m.get_mut(name) {
            t.enabled = enabled;
        }
    }

    pub async fn reload(&self, dir: &str) -> anyhow::Result<usize> {
        {
            let mut m = self.tools.write().await;
            m.clear();
        }
        self.load_internal().await;
        self.load_dir(dir).await?;
        let count = self.tools.read().await.len();
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str) -> ToolDefinition {
        ToolDefinition::internal(name, "test tool", serde_json::json!({}), vec![])
    }

    #[tokio::test]
    async fn crm_writes_are_agent_gated_by_flag_and_workflows_unaffected() {
        let r = ToolRegistry {
            tools: Arc::new(RwLock::new(HashMap::new())),
            timeout_sec: 5,
            mcp_manager: None,
        };
        for name in [
            "crm_lead_create", // CRM write — gated
            "crm_lead_list",   // CRM read — always agent-callable
            "fb_create_post",  // social write — always workflow-only
            "gmail_list",      // unrelated — always agent-callable
        ] {
            r.register(def(name)).await;
        }

        let names =
            |tools: Vec<ToolDefinition>| tools.into_iter().map(|t| t.name).collect::<Vec<_>>();

        let agent_default = names(r.all_enabled_for_agent(false).await);
        assert!(agent_default.contains(&"crm_lead_list".to_string()));
        assert!(agent_default.contains(&"gmail_list".to_string()));
        assert!(!agent_default.contains(&"crm_lead_create".to_string()));
        assert!(!agent_default.contains(&"fb_create_post".to_string()));

        let agent_with_writes = names(r.all_enabled_for_agent(true).await);
        assert!(agent_with_writes.contains(&"crm_lead_create".to_string()));
        // The toggle grants CRM writes only — social stays workflow-only.
        assert!(!agent_with_writes.contains(&"fb_create_post".to_string()));

        // The workflow path sees everything regardless of the toggle.
        let workflow = names(r.all_enabled().await);
        for name in [
            "crm_lead_create",
            "crm_lead_list",
            "fb_create_post",
            "gmail_list",
        ] {
            assert!(workflow.contains(&name.to_string()), "missing {name}");
        }
    }
}
