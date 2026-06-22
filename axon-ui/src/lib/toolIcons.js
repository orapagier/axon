export const TOOL_ICONS = {
    'facebook': '/icons/facebook.png',
    'instagram': '/icons/instagram.png',
    'google': '/icons/google.png',
    'youtube': '/icons/youtube.png',
    'google_places': '/icons/google_places.png',
    'google_sheets': '/icons/google_sheets.png',
    'google_calendar': '/icons/google_calendar.png',
    'google_docs': '/icons/google_docs.png',
    'gmail': '/icons/gmail.png',
    'google_drive': '/icons/google_drive.png',
    'google_meet': '/icons/google_meet.png',
    'google_contacts': '/icons/google_contacts.png',
    'slack': '/icons/slack.png',
    'discord': '/icons/discord.png',
    'github': '/icons/github.png',
    'search': '/icons/search.png',
    'onedrive': '/icons/onedrive.png',
    'outlook': '/icons/outlook.png',
    'ms_calendar': '/icons/ms_calendar.png',
    'crm': '/icons/crm.png',
};

// Maps an MCP service prefix (the part after "mcp_" in a node_type, e.g.
// "mcp_gsheets" -> "gsheets") to its icon. Lets getToolIcon resolve an MCP
// node's icon synchronously from its node_type, instead of waiting for
// loadMcpTools() to async-populate NODE_TYPES (which caused the 📦 flash).
// Keep in sync with MCP_SERVICE_META in WorkflowsPage.vue until icons move to SVG.
export const MCP_SERVICE_ICONS = {
    crm: '/icons/crm.png',
    gsheets: '/icons/google_sheets.png',
    gmail: '/icons/gmail.png',
    gdrive: '/icons/google_drive.png',
    gcal: '/icons/google_calendar.png',
    gdocs: '/icons/google_docs.png',
    gslides: '/icons/slides.png',
    gcon: '/icons/google_contacts.png',
    gyoutube: '/icons/youtube.png',
    gplaces: '/icons/google_places.png',
    gmeet: '/icons/google_meet.png',
    fb: '/icons/facebook.png',
    ig: '/icons/instagram.png',
    outlook: '/icons/outlook.png',
    mscal: '/icons/ms_calendar.png',
    onedrive: '/icons/onedrive.png',
    mscontacts: '/icons/outlook.png',
};

export function getToolIcon(type, parameters) {
    // 1. Handle Stimulus (Trigger) subtypes
    if (type === 'stimulus' && parameters?.type) {
        const triggerType = parameters.type.toLowerCase();
        if (triggerType === 'gmail') return TOOL_ICONS.gmail;
        if (triggerType === 'telegram') return TOOL_ICONS.telegram;
        if (triggerType === 'whatsapp') return TOOL_ICONS.whatsapp;
    }

    // 2a. Service-specific MCP nodes (mcp_<service>) — resolve straight from the
    // node_type so the icon paints immediately, before /mcp/tools returns.
    if (type && type.startsWith('mcp_')) {
        const service = type.slice(4);
        if (MCP_SERVICE_ICONS[service]) return MCP_SERVICE_ICONS[service];
    }

    // 2b. Legacy monolithic MCP node — resolve from its selected tool name.
    if (type === 'mcp' && parameters?.tool_name) {
        const toolName = parameters.tool_name.toLowerCase();

        // Google ecosystem
        if (toolName.includes('instagram') || toolName.startsWith('ig_')) return TOOL_ICONS.instagram;
        if (toolName.includes('facebook') || toolName.startsWith('fb_')) return TOOL_ICONS.facebook;
        if (toolName.includes('youtube') || toolName.startsWith('gyoutube_')) return TOOL_ICONS.youtube;
        if (toolName.includes('places') || toolName.startsWith('gplaces_')) return TOOL_ICONS.google_places;
        if (toolName.includes('calendar') && (toolName.includes('google') || toolName.startsWith('gcal_'))) return TOOL_ICONS.google_calendar;
        if (toolName.includes('sheet')) return TOOL_ICONS.google_sheets;
        if (toolName.includes('doc')) return TOOL_ICONS.google_docs;
        if (toolName.includes('gmail')) return TOOL_ICONS.gmail;
        if (toolName.includes('drive')) return TOOL_ICONS.google_drive;
        if (toolName.includes('meet')) return TOOL_ICONS.google_meet;
        if (toolName.includes('contact')) return TOOL_ICONS.google_contacts;
        if (toolName.includes('google')) return TOOL_ICONS.google;

        // Microsoft / Others
        if (toolName.includes('slack')) return TOOL_ICONS.slack;
        if (toolName.includes('discord')) return TOOL_ICONS.discord;
        if (toolName.includes('github')) return TOOL_ICONS.github;
        if (toolName.includes('search')) return TOOL_ICONS.search;
        if (toolName.includes('onedrive')) return TOOL_ICONS.onedrive;
        if (toolName.includes('outlook')) return TOOL_ICONS.outlook;
        if (toolName.includes('calendar') && toolName.includes('ms')) return TOOL_ICONS.ms_calendar;
        if (toolName.includes('crm') || toolName.includes('hubspot') || toolName.includes('salesforce')) return TOOL_ICONS.crm;
    }

    return null;
}
