/* ── Axon Dashboard app.js ─────────────────────────────────────────────────── */

// ── WebSocket ─────────────────────────────────────────────────────────────────
let ws, wsReconnectTimer;
const wsDot = document.getElementById('wsDot');
const wsStatus = document.getElementById('wsStatus');

function connectWs() {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    ws = new WebSocket(`${proto}://${location.host}/ws`);

    ws.onopen = () => {
        wsDot.className = 'ws-dot connected';
        wsStatus.textContent = 'Connected';
        clearTimeout(wsReconnectTimer);
    };
    ws.onclose = () => {
        wsDot.className = 'ws-dot disconnected';
        wsStatus.textContent = 'Reconnecting…';
        wsReconnectTimer = setTimeout(connectWs, 3000);

        // Safety: enable chat if it was disabled during a run
        setChatEnabled(true);
        currentRunId = null;
        if (currentAgentBubble) {
            const bubble = currentAgentBubble.querySelector('.chat-bubble');
            const indicator = bubble.querySelector('.thinking-indicator');
            if (indicator) indicator.remove();
        }
    };
    ws.onerror = () => ws.close();

    ws.onmessage = e => {
        try { handleWsEvent(JSON.parse(e.data)); } catch { }
    };
}

// ── WS event handler ─────────────────────────────────────────────────────────
let currentAgentBubble = null;
let currentTraceBubble = null;
let currentRunId = null;

function handleWsEvent(ev) {
    if (!currentRunId && ev.run_id) {
        currentRunId = ev.run_id;
        if (currentTraceBubble) currentTraceBubble.dataset.runId = ev.run_id;
    }
    switch (ev.type) {
        case 'thinking':
            if (ev.run_id !== currentRunId) break;
            updateThinkingIndicator(ev.text);
            ensureTraceBubble();
            appendTrace(`⋯ ${ev.text}`, 'muted');
            break;

        case 'model':
            if (ev.run_id !== currentRunId) break;
            ensureTraceBubble();
            const dur = ev.duration_ms ? ` (${ev.duration_ms}ms)` : '';
            appendTrace(`🤖 ${ev.model}  iter ${ev.iteration}${dur}`, 'model');
            break;

        case 'tools':
            if (ev.run_id !== currentRunId) break;
            ensureTraceBubble();
            const toolStr = ev.tools.join(', ') || 'none';
            const par = ev.parallel ? '⚡parallel' : 'sequential';
            appendTrace(`🔀 ${ev.tier} → [${toolStr}] ${par}`, 'info');
            break;

        case 'tool_start':
            if (ev.run_id !== currentRunId) break;
            updateThinkingIndicator(`Using ${ev.tool}...`);
            ensureTraceBubble();
            appendTrace(`▶ ${ev.tool}…`, 'warn', ev.tool_call_id);
            break;

        case 'tool_end':
            if (ev.run_id !== currentRunId) break;
            updateTrace(ev.tool_call_id,
                `${ev.ok ? '✓' : '✗'} ${ev.tool}  ${ev.duration_ms}ms`,
                ev.ok ? 'ok' : 'err');
            break;

        case 'token':
            if (ev.run_id !== currentRunId) break;
            appendToAgentBubble(ev.text);
            break;

        case 'memory_hit':
            if (ev.run_id !== currentRunId) break;
            ensureTraceBubble();
            appendTrace(`🧠 ${ev.count} memories retrieved`, 'info');
            break;

        case 'done':
            if (ev.run_id !== currentRunId) break;
            finalizeAgentBubble(ev);
            currentRunId = null;
            break;

        case 'error':
            if (ev.run_id !== currentRunId) break;
            if (currentAgentBubble) {
                const bubble = currentAgentBubble.querySelector('.chat-bubble');
                const indicator = bubble.querySelector('.thinking-indicator');
                if (indicator) indicator.remove();
                if (!bubble.textContent.trim()) bubble.textContent = 'Failed to complete task.';
            }
            appendChatMsg('agent', `⚠️ ${ev.message}`);
            currentAgentBubble = null;
            currentTraceBubble = null;
            currentRunId = null;
            setChatEnabled(true);
            break;
    }
}

// ── Chat helpers ──────────────────────────────────────────────────────────────
function appendChatMsg(role, text) {
    const msgs = document.getElementById('chatMessages');
    // Remove welcome if present
    const welcome = msgs.querySelector('.chat-welcome');
    if (welcome) welcome.remove();

    const div = document.createElement('div');
    div.className = `chat-msg ${role}`;
    div.innerHTML = `<div class="chat-bubble"></div>`;
    div.querySelector('.chat-bubble').textContent = text;
    msgs.appendChild(div);
    msgs.scrollTop = msgs.scrollHeight;
    return div;
}

function startAgentResponse(runId) {
    currentRunId = runId;

    // Trace bubble
    const msgs = document.getElementById('chatMessages');
    const traceDiv = document.createElement('div');
    traceDiv.className = 'tool-trace';
    traceDiv.dataset.runId = runId;
    msgs.appendChild(traceDiv);
    currentTraceBubble = traceDiv;

    // Agent bubble
    const agentDiv = document.createElement('div');
    agentDiv.className = 'chat-msg agent';
    agentDiv.innerHTML = `
    <div class="chat-bubble"><span class="thinking-indicator">Thinking…</span></div>
    <div class="chat-meta"></div>`;
    msgs.appendChild(agentDiv);
    currentAgentBubble = agentDiv;
    msgs.scrollTop = msgs.scrollHeight;
}

function ensureTraceBubble() {
    if (!currentTraceBubble) return;
}

function appendTrace(text, type, id) {
    if (!currentTraceBubble) return;
    const row = document.createElement('div');
    row.className = 'tool-trace-item';
    if (id) row.dataset.callId = id;
    const colors = { ok: '#48bb78', err: '#f56565', warn: '#f6ad55', info: '#4299e1', model: '#81e6d9', muted: '#718096' };
    row.innerHTML = `<span style="color:${colors[type] || '#718096'}">${escHtml(text)}</span>`;
    currentTraceBubble.appendChild(row);
    document.getElementById('chatMessages').scrollTop = 9999;
}

function updateTrace(callId, text, type) {
    if (!currentTraceBubble) return;
    const row = currentTraceBubble.querySelector(`[data-call-id="${callId}"]`);
    const colors = { ok: '#48bb78', err: '#f56565' };
    if (row) {
        row.innerHTML = `<span style="color:${colors[type] || '#718096'}">${escHtml(text)}</span>`;
    } else {
        appendTrace(text, type);
    }
}

function appendToAgentBubble(text) {
    if (!currentAgentBubble) return;
    const bubble = currentAgentBubble.querySelector('.chat-bubble');
    const indicator = bubble.querySelector('.thinking-indicator');
    if (indicator) indicator.remove();
    bubble.textContent += text;
    document.getElementById('chatMessages').scrollTop = 9999;
}

function updateThinkingIndicator(text) {
    if (!currentAgentBubble) return;
    const indicator = currentAgentBubble.querySelector('.thinking-indicator');
    if (indicator) {
        indicator.textContent = text;
    }
}

function finalizeAgentBubble(ev) {
    if (currentAgentBubble) {
        const bubble = currentAgentBubble.querySelector('.chat-bubble');
        const indicator = bubble.querySelector('.thinking-indicator');
        if (indicator) indicator.remove();
        if (!bubble.textContent.trim() && ev.full_text) {
            bubble.textContent = ev.full_text;
        }
        const dur = ev.total_duration_ms ? ` · ${ev.total_duration_ms}ms` : '';
        const meta = currentAgentBubble.querySelector('.chat-meta');
        meta.textContent = `${ev.iterations} iter · ${ev.total_tokens} tokens${dur}`;
    }
    currentAgentBubble = null;
    currentTraceBubble = null;
    setChatEnabled(true);
}

// ── Chat send ─────────────────────────────────────────────────────────────────
const chatInput = document.getElementById('chatInput');
const chatSend = document.getElementById('chatSend');

chatSend.addEventListener('click', sendChat);
chatInput.addEventListener('keydown', e => {
    if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); sendChat(); }
});
chatInput.addEventListener('input', () => {
    chatInput.style.height = 'auto';
    chatInput.style.height = Math.min(chatInput.scrollHeight, 160) + 'px';
});

// ── Session ───────────────────────────────────────────────────────────────────
const sessionId = 'dash_' + Math.random().toString(36).substring(2, 9);

function sendChat() {
    const msg = chatInput.value.trim();
    if (!msg || !ws || ws.readyState !== 1) return;

    appendChatMsg('user', msg);
    chatInput.value = '';
    chatInput.style.height = 'auto';
    setChatEnabled(false);
    startAgentResponse(null); // placeholder; run_id comes from first event

    ws.send(JSON.stringify({ task: msg, session_id: sessionId }));
}

function setChatEnabled(enabled) {
    chatInput.disabled = !enabled;
    chatSend.disabled = !enabled;
    if (enabled) {
        chatInput.focus();
    }
}

// Patch startAgentResponse to set currentRunId from first event
const _orig = startAgentResponse;
// run_id is set in handleWsEvent from each event

// ── Page navigation ───────────────────────────────────────────────────────────
const loaders = {
    overview: loadOverview,
    models: loadModels,
    tools: loadTools,
    patterns: loadPatterns,
    memory: loadMemoryRecent,
    scheduler: loadJobs,
    watchers: loadWatchers,
    mcp: loadMcp,
    files: loadFiles,
    runs: loadRuns,
    messaging: loadMessaging,
    settings: loadSettings,
};

document.querySelectorAll('.nav-item').forEach(btn => {
    btn.addEventListener('click', () => {
        document.querySelectorAll('.nav-item').forEach(b => b.classList.remove('active'));
        document.querySelectorAll('.page').forEach(p => p.classList.remove('active'));
        btn.classList.add('active');
        const page = btn.dataset.page;
        document.getElementById(`page-${page}`).classList.add('active');
        loaders[page]?.();
        if (page === 'chat') {
            setTimeout(() => chatInput.focus(), 100);
        }
    });
});

// ── API helpers ───────────────────────────────────────────────────────────────
async function api(method, path, body) {
    const opts = { method, headers: { 'Content-Type': 'application/json' } };
    if (body) opts.body = JSON.stringify(body);
    const r = await fetch('/api' + path, opts);
    return r.json();
}
function get(path) { return api('GET', path); }
function post(path, body) { return api('POST', path, body); }
function put(path, body) { return api('PUT', path, body); }
function del(path) { return api('DELETE', path); }

function toast(msg, ok = true) {
    const t = document.createElement('div');
    t.className = `toast ${ok ? 'ok' : 'err'}`;
    t.textContent = msg;
    document.body.appendChild(t);
    setTimeout(() => t.remove(), 3000);
}

function escHtml(s) {
    return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function fmtTokens(n) {
    return n >= 1000 ? (n / 1000).toFixed(1) + 'k' : String(n);
}

function pill(text, type) {
    return `<span class="pill pill-${type}">${escHtml(text)}</span>`;
}

function timeAgo(iso) {
    if (!iso) return '—';
    const diff = Date.now() - new Date(iso).getTime();
    const s = Math.floor(diff / 1000);
    if (s < 60) return `${s}s ago`;
    if (s < 3600) return `${Math.floor(s / 60)}m ago`;
    if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
    return `${Math.floor(s / 86400)}d ago`;
}

function fmtBytes(n) {
    if (!n) return '—';
    if (n < 1024) return n + ' B';
    if (n < 1048576) return (n / 1024).toFixed(1) + ' KB';
    return (n / 1048576).toFixed(1) + ' MB';
}

// ── Overview ──────────────────────────────────────────────────────────────────
async function loadOverview() {
    const [runsData, modelsData, jobsData] = await Promise.all([
        get('/runs'), get('/models'), get('/jobs')
    ]);
    const runs = runsData.runs || [];
    const models = modelsData.models || [];
    const jobs = jobsData.jobs || [];

    const completed = runs.filter(r => r.status === 'completed').length;
    const totalTok = runs.reduce((a, r) => a + (r.total_tokens || 0), 0);
    const avail = models.filter(m => m.status === 'available').length;
    const activeJ = jobs.filter(j => j.status === 'active').length;

    document.getElementById('overviewStats').innerHTML = `
    <div class="stat"><div class="stat-value">${runs.length}</div><div class="stat-label">Total Runs</div></div>
    <div class="stat"><div class="stat-value">${completed}</div><div class="stat-label">Completed</div></div>
    <div class="stat"><div class="stat-value">${avail}/${models.length}</div><div class="stat-label">Models Available</div></div>
    <div class="stat"><div class="stat-value">${activeJ}</div><div class="stat-label">Active Jobs</div></div>
  `;

    const recent = runs.slice(0, 10).map(r => `
    <div class="item">
      <div class="item-left">
        <div class="item-name">${escHtml(r.task.slice(0, 80))}</div>
        <div class="item-meta">${timeAgo(r.created_at)} · ${fmtTokens(r.total_tokens || 0)} tokens · ${r.iterations || 0} iter</div>
      </div>
      <div class="item-right">
        ${pill(r.status, r.status === 'completed' ? 'ok' : r.status === 'running' ? 'info' : 'err')}
        ${pill(r.platform || 'dashboard', 'muted')}
      </div>
    </div>`).join('');
    document.getElementById('recentRuns').innerHTML = recent || '<div class="empty">No runs yet</div>';
}

// ── Models ────────────────────────────────────────────────────────────────────
let allModels = [];

async function loadModels() {
    const data = await get('/models');
    allModels = data.models || [];
    const list = document.getElementById('modelsList');
    if (!allModels.length) { list.innerHTML = '<div class="empty">No models configured</div>'; return; }

    list.innerHTML = allModels.map(m => {
        const rl = m.rl_snapshot || {};
        const tokRem = rl.tokens_remaining_per_min;
        const tokLim = rl.tokens_limit_per_min;
        const pct = (tokRem && tokLim) ? Math.round((tokRem / tokLim) * 100) : null;

        return `
    <div class="model-card status-${m.status} ${m.enabled === false ? 'disabled' : ''}">
      <div class="model-info">
        <div class="model-name-row">
            <div class="model-name">${escHtml(m.name)} ${m.enabled === false ? pill('disabled', 'muted') : ''}</div>
            <div class="model-actions-inline">
                <button class="btn btn-xs btn-ghost" onclick="showEditModel('${escHtml(m.name)}')">Edit</button>
                <button class="btn btn-xs btn-ghost" onclick="toggleModel('${escHtml(m.name)}', ${m.enabled !== false})">
                    ${m.enabled !== false ? 'Disable' : 'Enable'}
                </button>
                <button class="btn btn-xs btn-danger" onclick="deleteModel('${escHtml(m.name)}')">✕</button>
            </div>
        </div>
        <div class="model-provider">${escHtml(m.provider)} / ${escHtml(m.model_id)}${m.role ? ` · ${pill(m.role, 'info')}` : ''}</div>
        <div class="model-stats-row">
          <span>calls <span>${m.total_calls}</span></span>
          <span>in <span>${fmtTokens(m.total_input_tokens)}</span></span>
          <span>out <span>${fmtTokens(m.total_output_tokens)}</span></span>
          <span>errors <span>${m.consecutive_errors}</span></span>
          ${m.rate_limit_reset_at ? `<span>resets <span>${timeAgo(m.rate_limit_reset_at)}</span></span>` : ''}
        </div>
        ${pct !== null ? `
        <div class="progress-bar" title="${tokRem} / ${tokLim} tokens remaining">
          <div class="progress-fill" style="width:${pct}%"></div>
        </div>` : ''}
      </div>
      <div class="item-right">
        ${pill(m.status, m.status === 'available' ? 'ok' : m.status === 'rate_limited' ? 'warn' : 'err')}
        ${pill('p' + m.priority, 'muted')}
        ${m.status !== 'available' ? `<button class="btn btn-sm btn-ghost" onclick="resetModel('${escHtml(m.name)}')">Reset</button>` : ''}
      </div>
    </div>`;
    }).join('');
}

let isEditingModel = false;

function showAddModel() {
    isEditingModel = false;
    document.getElementById('modelModalTitle').textContent = 'Add AI Model';
    document.getElementById('modelName').value = '';
    document.getElementById('modelName').disabled = false;
    document.getElementById('modelProvider').value = 'openai';
    document.getElementById('modelId').value = '';
    document.getElementById('modelKey').value = '';
    document.getElementById('modelBaseUrl').value = '';
    document.getElementById('modelPriority').value = '1';
    document.getElementById('modelRole').value = '';
    document.getElementById('modelMaxTokens').value = '4096';
    document.getElementById('modelModal').classList.add('open');
}

function showEditModel(name) {
    const m = allModels.find(x => x.name === name);
    if (!m) return;
    isEditingModel = true;
    document.getElementById('modelModalTitle').textContent = 'Edit Model: ' + name;
    document.getElementById('modelName').value = m.name;
    document.getElementById('modelName').disabled = true;
    document.getElementById('modelProvider').value = m.provider;
    document.getElementById('modelId').value = m.model_id;
    document.getElementById('modelKey').value = m.api_key;
    document.getElementById('modelBaseUrl').value = m.base_url || '';
    document.getElementById('modelPriority').value = m.priority;
    document.getElementById('modelRole').value = m.role || '';
    document.getElementById('modelMaxTokens').value = m.max_tokens || 4096;
    document.getElementById('modelModal').classList.add('open');
}

async function saveModel() {
    const body = {
        name: document.getElementById('modelName').value.trim(),
        provider: document.getElementById('modelProvider').value,
        model_id: document.getElementById('modelId').value.trim() || null,
        api_key: document.getElementById('modelKey').value.trim(),
        base_url: document.getElementById('modelBaseUrl').value.trim() || null,
        priority: parseInt(document.getElementById('modelPriority').value) || 1,
        role: document.getElementById('modelRole').value,
        max_tokens: parseInt(document.getElementById('modelMaxTokens').value) || 4096
    };
    if (!body.name || !body.api_key) return toast('Name and API key required', false);

    let r;
    if (isEditingModel) {
        r = await put(`/models/${encodeURIComponent(body.name)}`, body);
    } else {
        r = await post('/models', body);
    }

    toast(r.ok ? `Model saved` : r.error, r.ok);
    if (r.ok) { closeModal('modelModal'); loadModels(); }
}

async function toggleModel(name, currentlyEnabled) {
    const r = await put(`/models/${encodeURIComponent(name)}`, { enabled: !currentlyEnabled });
    toast(r.ok ? `Model ${currentlyEnabled ? 'disabled' : 'enabled'}` : r.error, r.ok);
    loadModels();
}

async function deleteModel(name) {
    if (!confirm(`Delete model "${name}"?`)) return;
    const r = await del(`/models/${encodeURIComponent(name)}`);
    toast(r.ok ? 'Model deleted' : r.error, r.ok);
    loadModels();
}

async function resetModel(name) {
    const r = await post(`/models/${encodeURIComponent(name)}/reset`, {});
    toast(r.ok ? `${name} reset` : r.error, r.ok);
    loadModels();
}

// ── Tools ─────────────────────────────────────────────────────────────────────
async function loadTools() {
    const data = await get('/tools');
    const list = document.getElementById('toolsList');
    if (!data.tools?.length) { list.innerHTML = '<div class="empty">No tools loaded</div>'; return; }

    const bySource = { python: [], mcp: [], internal: [], temp: [] };
    data.tools.forEach(t => {
        const type = t.source?.source_type || 'internal';
        (bySource[type] = bySource[type] || []).push(t);
    });

    let html = '';
    for (const [src, tools] of Object.entries(bySource)) {
        if (!tools.length) continue;
        html += `<div class="card"><h2>${src.toUpperCase()} TOOLS (${tools.length})</h2>`;
        html += tools.map(t => `
      <div class="item">
        <div class="item-left">
          <div class="item-name">${escHtml(t.name)}</div>
          <div class="item-meta">${escHtml(t.description)}</div>
          ${t.required?.length ? `<div class="tags">${t.required.map(r => `<span class="tag">${escHtml(r)}</span>`).join('')}</div>` : ''}
        </div>
        <div class="item-right">
          ${pill(t.enabled ? 'enabled' : 'disabled', t.enabled ? 'ok' : 'muted')}
          <button class="btn btn-sm btn-ghost" onclick="toggleTool('${escHtml(t.name)}', ${!t.enabled})">
            ${t.enabled ? 'Disable' : 'Enable'}
          </button>
        </div>
      </div>`).join('');
        html += '</div>';
    }
    list.innerHTML = html;
}

async function reloadTools() {
    const r = await post('/tools/reload', { dir: 'tools' });
    toast(r.ok ? `${r.count} tools loaded` : r.error, r.ok);
    loadTools();
}

async function toggleTool(name, enabled) {
    const r = await put(`/tools/${encodeURIComponent(name)}`, { enabled });
    toast(r.ok ? `Tool ${enabled ? 'enabled' : 'disabled'}` : r.error, r.ok);
    loadTools();
}

// ── Patterns ──────────────────────────────────────────────────────────────────
async function loadPatterns() {
    const data = await get('/patterns');
    const list = document.getElementById('patternsList');
    if (!data.patterns?.length) { list.innerHTML = '<div class="empty">No patterns configured</div>'; return; }

    const grouped = {};
    data.patterns.forEach(p => {
        (grouped[p.tool_name] = grouped[p.tool_name] || []).push(p);
    });

    list.innerHTML = Object.entries(grouped).map(([tool, pats]) => `
    <div class="card pattern-group">
      <div class="pattern-group-title">${escHtml(tool)}</div>
      ${pats.map(p => `
        <div class="item">
          <div class="item-left">
            <div class="item-name" style="font-family:monospace;font-size:13px">${escHtml(p.pattern)}</div>
            ${p.description ? `<div class="item-meta">${escHtml(p.description)}</div>` : ''}
          </div>
          <div class="item-right">
            ${pill(p.enabled ? 'on' : 'off', p.enabled ? 'ok' : 'muted')}
            <button class="btn btn-sm btn-ghost" onclick="togglePattern(${p.id}, ${!p.enabled})">
              ${p.enabled ? 'Disable' : 'Enable'}
            </button>
            <button class="btn btn-sm btn-danger" onclick="deletePattern(${p.id})">✕</button>
          </div>
        </div>`).join('')}
    </div>`).join('');
}

function showAddPattern() { document.getElementById('patternModal').classList.add('open'); }

async function addPattern() {
    const tool = document.getElementById('newPatternTool').value.trim();
    const pat = document.getElementById('newPatternRegex').value.trim();
    const desc = document.getElementById('newPatternDesc').value.trim();
    if (!tool || !pat) return toast('Tool name and pattern required', false);
    const r = await post('/patterns', { tool_name: tool, pattern: pat, description: desc || null });
    toast(r.ok ? 'Pattern added' : r.error, r.ok);
    if (r.ok) { closeModal('patternModal'); loadPatterns(); }
}

async function togglePattern(id, enabled) {
    await put(`/patterns/${id}`, { enabled });
    loadPatterns();
}

async function deletePattern(id) {
    if (!confirm('Delete this pattern?')) return;
    const r = await del(`/patterns/${id}`);
    toast(r.ok ? 'Deleted' : r.error, r.ok);
    loadPatterns();
}

async function testRouting() {
    const msg = document.getElementById('patternTestInput').value.trim();
    if (!msg) return;
    const r = await post('/patterns/test', { message: msg });
    const res = document.getElementById('testResult');
    const tools = r.matched_tools || [];
    const tier = r.routing_info?.tier || '?';
    res.innerHTML = `
    <div style="font-size:13px">
      <span style="color:var(--muted)">Tier: </span>${pill(tier, 'info')} &nbsp;
      <span style="color:var(--muted)">Matched: </span>
      ${tools.length ? tools.map(t => pill(t, 'ok')).join(' ') : pill('none', 'muted')}
    </div>`;
}

// ── Memory ────────────────────────────────────────────────────────────────────
let memoryPoll = null;
async function loadMemoryRecent() {
    const data = await get('/memory/recent');
    renderMemoryList(data.entries || []);
    // Auto-refresh while on memory page
    if (!memoryPoll) {
        memoryPoll = setInterval(() => {
            if (document.getElementById('page-memory')?.classList.contains('active')) {
                get('/memory/recent').then(d => renderMemoryList(d.entries || []));
            } else { clearInterval(memoryPoll); memoryPoll = null; }
        }, 10000);
    }
}

async function searchMemory() {
    const q = document.getElementById('memorySearch').value.trim();
    if (!q) return loadMemoryRecent();
    const data = await post('/memory/search', { query: q, top_k: 20 });
    renderMemoryList(data.results || []);
}

function renderMemoryList(entries) {
    const list = document.getElementById('memoryList');
    if (!entries.length) { list.innerHTML = '<div class="empty">No memories found</div>'; return; }
    list.innerHTML = entries.map(e => `
    <div class="memory-item">
      <div class="item-left">
        <div class="memory-content">${escHtml(e.content)}</div>
        <div class="memory-meta">${e.source || ''} · ${timeAgo(e.created_at)}
          ${e.score != null ? ` · <span class="memory-score">${(e.score * 100).toFixed(0)}% match</span>` : ''}</div>
      </div>
      <button class="btn btn-sm btn-danger" onclick="deleteMemory(${e.id})">✕</button>
    </div>`).join('');
}

async function deleteMemory(id) {
    const r = await del(`/memory/${id}`);
    toast(r.ok ? 'Deleted' : r.error, r.ok);
    loadMemoryRecent();
}

// ── Scheduler ─────────────────────────────────────────────────────────────────
let allJobs = [];
let editingJobId = null;

async function loadJobs() {
    const data = await get('/jobs');
    const list = document.getElementById('jobsList');
    allJobs = data.jobs || [];
    if (!allJobs.length) { list.innerHTML = '<div class="empty">No scheduled jobs</div>'; return; }

    list.innerHTML = allJobs.map(j => `
    <div class="job-card status-${j.status}">
      <div>
        <div class="job-name">${escHtml(j.name)} ${j.created_by === 'agent' ? pill('agent-created', 'info') : ''}</div>
        <div class="job-schedule">${escHtml(j.cron_expr)} · ${escHtml(j.schedule_nl)}</div>
        <div class="job-task">${escHtml(j.task)}</div>
        <div class="job-meta">
          runs: ${j.run_count} · last: ${timeAgo(j.last_run_at)}
          ${pill(j.status, j.status === 'active' ? 'ok' : j.status === 'paused' ? 'warn' : 'muted')}
          ${j.platform !== 'dashboard' ? pill(j.platform, 'info') : ''}
        </div>
      </div>
        <div class="job-actions">
          <button class="btn btn-sm btn-success" onclick="runJobNow('${j.id}')">Run Now</button>
          <button class="btn btn-sm" onclick="showEditJob('${j.id}')">Edit</button>
          <button class="btn btn-sm btn-info" onclick="viewJobRuns('${j.id}', '${escHtml(j.name)}')">View Runs</button>
          ${j.status === 'active' ? `<button class="btn btn-sm btn-warn"  onclick="pauseJob('${j.id}')">Pause</button>` : ''}
          ${j.status === 'paused' ? `<button class="btn btn-sm btn-success" onclick="resumeJob('${j.id}')">Resume</button>` : ''}
          <button class="btn btn-sm btn-danger" onclick="deleteJob('${j.id}')">Delete</button>
        </div>
      </div>`).join('');
}

function viewJobRuns(jobId, jobName) {
    document.querySelectorAll('.nav-item').forEach(b => b.classList.remove('active'));
    document.querySelectorAll('.page').forEach(p => p.classList.remove('active'));
    const btn = document.querySelector('.nav-item[data-page="runs"]');
    btn.classList.add('active');
    document.getElementById('page-runs').classList.add('active');
    loadRuns(jobId, jobName);
}

function showCreateJob() {
    editingJobId = null;
    document.getElementById('jobModalTitle').textContent = 'Create Scheduled Job';
    document.getElementById('jobSaveBtn').textContent = 'Schedule';
    document.getElementById('jobName').value = '';
    document.getElementById('jobTask').value = '';
    document.getElementById('jobSchedule').value = 'every day';
    document.getElementById('jobStopCond').value = '';
    document.getElementById('jobModal').classList.add('open');
}

function showEditJob(id) {
    const j = allJobs.find(x => x.id === id);
    if (!j) return;
    editingJobId = id;
    document.getElementById('jobModalTitle').textContent = 'Edit Job: ' + j.name;
    document.getElementById('jobSaveBtn').textContent = 'Save Changes';
    document.getElementById('jobName').value = j.name;
    document.getElementById('jobTask').value = j.task;
    document.getElementById('jobSchedule').value = j.schedule_nl;
    document.getElementById('jobStopCond').value = j.stop_condition?.value || '';
    document.getElementById('jobModal').classList.add('open');
}

async function saveJob() {
    const name = document.getElementById('jobName').value.trim();
    const task = document.getElementById('jobTask').value.trim();
    const sched = document.getElementById('jobSchedule').value.trim();
    const stop = document.getElementById('jobStopCond').value.trim();
    if (!name || !task || !sched) return toast('Name, task and schedule required', false);

    const body = { name, task, schedule_nl: sched };
    if (stop) body.stop_condition = { condition_type: 'result_contains', value: stop };

    let r;
    if (editingJobId) {
        r = await put(`/jobs/${editingJobId}`, body);
    } else {
        r = await post('/jobs', body);
    }

    toast(r.ok ? (editingJobId ? 'Job updated' : 'Job scheduled') : r.error, r.ok);
    if (r.ok) { closeModal('jobModal'); loadJobs(); }
}

async function runJobNow(id) {
    toast('Running job...', true);
    const r = await post(`/jobs/${id}/run`, {});
    toast(r.ok ? 'Job completed' : r.error, r.ok);
    loadJobs();
}

async function pauseJob(id) { const r = await post(`/jobs/${id}/pause`, ''); toast(r.ok ? 'Paused' : r.error, r.ok); loadJobs(); }
async function resumeJob(id) { const r = await post(`/jobs/${id}/resume`, ''); toast(r.ok ? 'Resumed' : r.error, r.ok); loadJobs(); }
async function deleteJob(id) {
    if (!confirm('Delete this job?')) return;
    const r = await del(`/jobs/${id}/delete`);
    toast(r.ok ? 'Deleted' : r.error, r.ok);
    loadJobs();
}

// ── Watchers (Smart Notifications) ────────────────────────────────────────────
async function loadWatchers() {
    const data = await get('/watchers');
    const logsData = await get('/watchers/log');
    const list = document.getElementById('watchersList');
    const watchers = data.watchers || [];

    if (!watchers.length) {
        list.innerHTML = '<div class="empty">No watchers arranged</div>';
    } else {
        list.innerHTML = watchers.map(w => {
            const isCustom = w.service === 'custom';
            const displayService = isCustom ? `Custom Tool: ${w.tool_name}` : w.service.toUpperCase();
            const label = w.label || displayService;

            return `
            <div class="job-card status-${w.enabled ? 'active' : 'paused'}">
                <div>
                    <div class="job-name" style="font-weight:600; font-size:16px;">${escHtml(label)}</div>
                    <div class="job-schedule">Polls every ${w.poll_mins}m · Service: ${escHtml(displayService)}</div>
                    ${w.tool_args && w.tool_args !== '{}' ? `<div class="job-task" style="font-family:monospace">args: ${escHtml(w.tool_args)}</div>` : ''}
                    <div class="job-meta">
                        last check: ${timeAgo(w.last_check)} · items seen: ${w.last_seen_count || 0}
                        ${pill(w.enabled ? 'active' : 'paused', w.enabled ? 'ok' : 'muted')}
                    </div>
                </div>
                <div class="job-actions">
                    <button class="btn btn-sm ${w.enabled ? 'btn-warn' : 'btn-success'}" onclick="toggleWatcherStatus('${w.id}', ${!w.enabled})">
                        ${w.enabled ? 'Pause' : 'Resume'}
                    </button>
                    <button class="btn btn-sm btn-danger" onclick="deleteWatcher('${w.id}')">Delete</button>
                </div>
            </div>`;
        }).join('');
    }

    const logsList = document.getElementById('watcherLogList');
    const logs = logsData.log || [];
    if (!logs.length) {
        logsList.innerHTML = '<div style="color:var(--muted);font-size:13px">No recent polling activity.</div>';
    } else {
        logsList.innerHTML = logs.map(l => {
            const time = new Date(l.created_at).toLocaleTimeString();
            const count = l.new_count || 0;
            const color = count > 0 ? "var(--teal)" : "var(--muted)";
            return `<div style="padding:4px 0; border-bottom:1px solid rgba(255,255,255,0.05); font-size:13px; font-family:monospace; color:${color}">
                [${time}] ${escHtml(l.label)} checked: found ${count} new items.
            </div>`;
        }).join('');
    }
}

function showAddWatcher() {
    document.getElementById('watcherId').value = '';
    document.getElementById('watcherService').value = 'gmail';
    document.getElementById('watcherToolName').value = '';
    document.getElementById('watcherToolArgs').value = '{}';
    document.getElementById('watcherLabel').value = '';
    document.getElementById('watcherPollMins').value = '5';
    document.getElementById('watcherEnabled').checked = true;
    toggleWatcherCustomFields();
    document.getElementById('watcherModal').classList.add('open');
}

function toggleWatcherCustomFields() {
    const s = document.getElementById('watcherService').value;
    document.getElementById('watcherCustomFields').style.display = (s === 'custom') ? 'block' : 'none';
}

async function saveWatcher() {
    const body = {
        id: document.getElementById('watcherId').value || undefined,
        service: document.getElementById('watcherService').value,
        poll_mins: parseInt(document.getElementById('watcherPollMins').value) || 5,
        enabled: document.getElementById('watcherEnabled').checked
    };

    if (body.service === 'custom') {
        body.tool_name = document.getElementById('watcherToolName').value.trim();
        body.tool_args = document.getElementById('watcherToolArgs').value.trim() || '{}';
        body.label = document.getElementById('watcherLabel').value.trim();
        if (!body.tool_name) return toast('Tool Name is required for custom watchers', false);
    }

    const r = await post('/watchers', body);
    toast(r.ok ? 'Watcher saved' : r.error, r.ok);
    if (r.ok) { closeModal('watcherModal'); loadWatchers(); }
}

async function toggleWatcherStatus(id, enabled) {
    const r = await put(`/watchers/${id}`, { enabled });
    loadWatchers();
}

async function deleteWatcher(id) {
    if (!confirm('Delete this watcher?')) return;
    const r = await del(`/watchers/${id}`);
    toast(r.ok ? 'Deleted' : r.error, r.ok);
    loadWatchers();
}

// ── MCP ───────────────────────────────────────────────────────────────────────
async function loadMcp() {
    const data = await get('/mcp');
    const list = document.getElementById('mcpList');
    const servers = data.servers || [];
    if (!servers.length) { list.innerHTML = '<div class="empty">No MCP servers connected. Click + Connect to add one.</div>'; return; }
    list.innerHTML = servers.map(name => `
    <div class="item">
      <div class="item-left">
        <div class="item-name">${escHtml(name)}</div>
        <div class="item-meta">${(data.tools || []).filter(t => t.source?.server_name === name).length} tools</div>
      </div>
      <div class="item-right">
        ${pill('connected', 'ok')}
        <button class="btn btn-sm btn-danger" onclick="disconnectMcp('${escHtml(name)}')">Disconnect</button>
      </div>
    </div>`).join('');
}

function showConnectMcp() { document.getElementById('mcpModal').classList.add('open'); }

async function connectMcp() {
    const name = document.getElementById('mcpName').value.trim();
    const url = document.getElementById('mcpUrl').value.trim();
    const key = document.getElementById('mcpKey').value.trim();
    if (!name || !url) return toast('Name and URL required', false);
    const r = await post('/mcp', { name, url, api_key: key || null });
    toast(r.ok ? `Connected (${r.tool_count} tools)` : r.error, r.ok);
    if (r.ok) { closeModal('mcpModal'); loadMcp(); }
}

async function disconnectMcp(name) {
    const r = await del(`/mcp/${encodeURIComponent(name)}`);
    toast(r.ok ? 'Disconnected' : r.error, r.ok);
    loadMcp();
}

// ── Files ─────────────────────────────────────────────────────────────────────
async function loadFiles() {
    const [inc, out] = await Promise.all([get('/files/incoming'), get('/files/outgoing')]);
    renderFiles('filesIncoming', inc.files || []);
    renderFiles('filesOutgoing', out.files || []);
}

function renderFiles(containerId, files) {
    const el = document.getElementById(containerId);
    if (!files.length) { el.innerHTML = '<div class="empty">No files</div>'; return; }
    el.innerHTML = files.map(f => `
    <div class="file-item">
      <div class="item-left">
        <div class="file-name">${escHtml(f.filename)}</div>
        <div class="file-meta">${f.mime_type || 'unknown type'} · ${fmtBytes(f.size_bytes)} · ${timeAgo(f.created_at)}</div>
      </div>
      <a class="btn btn-sm btn-ghost" href="/api/files/${f.id}" download="${escHtml(f.filename)}">Download</a>
    </div>`).join('');
}

async function uploadFile(input) {
    const file = input.files[0];
    if (!file) return;
    const form = new FormData();
    form.append('file', file);
    const r = await fetch('/api/files/upload', { method: 'POST', body: form }).then(r => r.json());
    toast(r.ok ? `Uploaded: ${file.name}` : r.error, r.ok);
    loadFiles();
}

// ── Run history ───────────────────────────────────────────────────────────────
let runsPoll = null;
async function loadRuns(jobId = null, jobName = null) {
    const path = jobId ? `/runs?job_id=${encodeURIComponent(jobId)}` : '/runs';
    const data = await get(path);
    const list = document.getElementById('runsList');

    // Add filter header if necessary
    let html = '';
    if (jobId) {
        html += `<div class="filter-bar">
            <span>Filtering by Job: <b>${escHtml(jobName || jobId)}</b></span>
            <button class="btn btn-sm btn-ghost" onclick="loadRuns()">Clear Filter</button>
        </div>`;
    }

    // Auto-refresh while on runs page
    if (!runsPoll) {
        runsPoll = setInterval(() => {
            if (document.getElementById('page-runs')?.classList.contains('active')) {
                loadRuns(jobId, jobName);
            } else { clearInterval(runsPoll); runsPoll = null; }
        }, 10000);
    }

    const runs = data.runs || [];
    if (!runs.length) {
        list.innerHTML = html + '<div class="empty">No runs found for this view</div>';
        return;
    }

    list.innerHTML = html + runs.map(r => {
        const models = safeJsonParse(r.models_used, []).join(', ');
        const tools = safeJsonParse(r.tools_used, []).join(', ');
        return `
    <div class="run-item">
      <div class="run-header" onclick="toggleRunDetail('${r.id}')">
        <div class="run-task">${escHtml(r.task.slice(0, 100))}</div>
        ${pill(r.status, r.status === 'completed' ? 'ok' : r.status === 'running' ? 'info' : 'err')}
        ${pill(r.platform, 'muted')}
        <span style="font-size:11px;color:var(--muted)">${timeAgo(r.created_at)}</span>
      </div>
      <div class="run-detail" id="run-detail-${r.id}">
        <div style="font-size:11px;color:var(--muted);margin-bottom:10px">
          ${r.iterations} iterations · ${fmtTokens(r.total_tokens)} tokens ·
          models: ${escHtml(models || '—')} · tools: ${escHtml(tools || '—')}
        </div>
        ${r.result ? `<div class="run-result">${escHtml(r.result.slice(0, 1000))}${r.result.length > 1000 ? '…' : ''}</div>` : ''}
        <button class="btn btn-sm btn-ghost" onclick="loadRunDetail('${r.id}')">Load Tool Trace</button>
        <div id="run-trace-${r.id}"></div>
      </div>
    </div>`;
    }).join('');
}

function toggleRunDetail(id) {
    document.getElementById(`run-detail-${id}`)?.classList.toggle('open');
}

async function loadRunDetail(id) {
    const data = await get(`/runs/${id}`);
    const el = document.getElementById(`run-trace-${id}`);
    const calls = data.tool_calls || [];
    const iters = data.iterations || [];

    if (!calls.length && !iters.length) {
        el.innerHTML = '<div style="color:var(--muted);font-size:12px;margin-top:8px">No trace data</div>';
        return;
    }

    let iterHtml = '';
    if (iters.length) {
        iterHtml = `<div style="margin-top:12px;margin-bottom:12px;border-left:2px solid var(--border);padding-left:10px;">
          <strong style="font-size:11px;color:var(--muted);">ITERATION HISTORY</strong>` + iters.map(it => `
        <div style="background:var(--surface);border:1px solid var(--border);border-radius:6px;padding:10px;margin-top:8px">
          <div style="display:flex;justify-content:space-between;font-size:12px;margin-bottom:6px;border-bottom:1px solid var(--border);padding-bottom:6px">
            <span style="font-weight:600;color:var(--brand)">Iteration ${it.iteration}</span>
            <span style="color:var(--muted)">${it.duration_ms != null ? (it.duration_ms / 1000).toFixed(1) + 's' : '?s'}</span>
          </div>
          <div style="font-size:11px;color:var(--muted)">
            Model: <code>${escHtml(it.model_name)}</code> · Tokens: ${it.tokens}
          </div>
        </div>`).join('') + `</div>`;
    }

    let callHtml = '';
    if (calls.length) {
        callHtml = `<div style="margin-top:12px;border-left:2px solid var(--border);padding-left:10px;">
          <strong style="font-size:11px;color:var(--muted);">TOOL CALLS</strong>` + calls.map(tc => `
        <div class="tool-call-card" style="background:var(--surface);border:1px solid var(--border);border-radius:6px;padding:10px;margin-top:8px">
          <div style="display:flex;justify-content:space-between;font-size:12px;margin-bottom:6px;border-bottom:1px solid var(--border);padding-bottom:6px">
            <span style="font-weight:600">${escHtml(tc.tool_name)}</span>
            <span style="color:var(--muted)">${tc.duration_ms || 0}ms ${tc.parallel ? '⚡' : ''}</span>
            ${pill(tc.error ? 'error' : 'ok', tc.error ? 'err' : 'ok')}
          </div>
          ${tc.args ? `<div style="font-size:11px;color:var(--muted);margin-bottom:4px">args: <code>${escHtml(tc.args.slice(0, 200))}</code></div>` : ''}
          ${tc.result ? `<div style="font-size:11px;color:var(--teal)">result: <code>${escHtml(tc.result.slice(0, 300))}</code></div>` : ''}
          ${tc.error ? `<div style="font-size:11px;color:var(--red)">error: ${escHtml(tc.error)}</div>` : ''}
        </div>`).join('') + '</div>';
    }

    el.innerHTML = iterHtml + callHtml;
}

// ── Settings ──────────────────────────────────────────────────────────────────
async function loadSettings() {
    const data = await get('/settings');
    const list = document.getElementById('settingsList');
    const settings = data.settings || [];
    if (!settings.length) { list.innerHTML = '<div class="empty">No settings</div>'; return; }

    const byCategory = {};
    settings.forEach(s => {
        const cat = s.category || 'other';
        // Hide legacy "providers" section — models page handles API keys now
        if (cat === 'providers') return;
        (byCategory[cat] = byCategory[cat] || []).push(s);
    });

    let html = '';
    for (const [cat, rows] of Object.entries(byCategory)) {
        html += `<div class="card"><h2>${cat.toUpperCase()}</h2><table class="settings-table">
      <thead><tr><th>Key</th><th>Value</th><th>Type</th><th>Description</th><th></th></tr></thead><tbody>`;
        html += rows.map(s => {
            const isKey = s.key.toLowerCase().includes('key') || s.key.toLowerCase().includes('token');
            return `
      <tr>
        <td class="key-col">${escHtml(s.key)}</td>
        <td><input type="${isKey ? 'password' : 'text'}" value="${escHtml(s.value)}" id="setting-${escHtml(s.key.replace(/\./g, '-'))}" style="width:160px;margin:0"></td>
        <td><span class="cat-badge">${escHtml(s.value_type)}</span></td>
        <td class="desc-col">${escHtml(s.description || '')}</td>
        <td><button class="btn btn-sm btn-success" onclick="saveSetting('${escHtml(s.key)}')">Save</button></td>
      </tr>`;
        }).join('');
        html += '</tbody></table></div>';
    }
    list.innerHTML = html;
}

async function saveSetting(key) {
    const inputId = `setting-${key.replace(/\./g, '-')}`;
    const val = document.getElementById(inputId)?.value ?? '';
    const r = await put(`/settings/${encodeURIComponent(key)}`, { value: val });
    toast(r.ok ? `Saved: ${key}` : r.error, r.ok);
}

// ── Messaging ────────────────────────────────────────────────────────────────
let messagingPoll = null;
async function loadMessaging() {
    const [settingsData, statusData] = await Promise.all([
        get('/settings'), get('/messaging/status')
    ]);
    const settings = settingsData.settings || [];
    const status = statusData || {};
    const list = document.getElementById('platformsList');

    const platforms = [
        { id: 'telegram', name: 'Telegram', icon: '📤', key: 'messaging.telegram_token', desc: 'Create a bot via @BotFather.' },
        { id: 'discord', name: 'Discord', icon: '👾', key: 'messaging.discord_token', desc: 'Discord Developer Portal.' },
        { id: 'slack', name: 'Slack', icon: '💬', key: 'messaging.slack_token', desc: 'Bot User OAuth Token.' }
    ];

    list.innerHTML = platforms.map(p => {
        const s = settings.find(x => x.key === p.key) || { value: '' };
        const isConfigured = !!s.value;
        const isConnected = status[p.id]?.connected;
        return `
    <div class="card platform-card">
      <div class="platform-header">
        <div class="platform-icon">${p.icon}</div>
        <div class="platform-info">
          <h3>${p.name}</h3>
          <div class="platform-desc">${p.desc}</div>
        </div>
        <div class="platform-status">
           ${isConnected ? pill('Active', 'ok') : isConfigured ? pill('Ready to Start', 'info') : pill('Not Configured', 'muted')}
        </div>
      </div>
      <div class="platform-actions" style="margin-top:16px; display:flex; gap:10px; align-items:center;">
        <input type="password" placeholder="Paste Token..." value="${escHtml(s.value)}" id="msg-token-${p.id}" style="margin:0; flex:1">
        <button class="btn btn-success" onclick="saveMsgToken('${p.id}', '${p.key}')">Save</button>
        <button class="btn btn-primary" onclick="reconnectPlatform('${p.id}')" ${!isConfigured ? 'disabled' : ''}>${isConnected ? 'Restart' : 'Connect'}</button>
      </div>
    </div>`;
    }).join('');

    if (!messagingPoll) {
        messagingPoll = setInterval(() => {
            if (document.getElementById('page-messaging').classList.contains('active')) {
                loadMessaging();
            } else {
                clearInterval(messagingPoll);
                messagingPoll = null;
            }
        }, 5000);
    }
}

async function saveMsgToken(id, key) {
    const val = document.getElementById(`msg-token-${id}`).value.trim();
    const r = await put(`/settings/${encodeURIComponent(key)}`, { value: val });
    toast(r.ok ? `Saved ${key}` : r.error, r.ok);
    loadMessaging();
}

async function reconnectPlatform(id) {
    toast(`Starting ${id}...`, true);
    const r = await post(`/messaging/reconnect/${id}`);
    if (r.ok) toast(`${id} started!`, true);
    else toast(r.error || `Failed to start ${id}`, false);
    loadMessaging();
}

// ── Modal helpers ─────────────────────────────────────────────────────────────
function closeModal(id) { document.getElementById(id).classList.remove('open'); }
document.querySelectorAll('.modal-overlay').forEach(m => {
    m.addEventListener('click', e => { if (e.target === m) m.classList.remove('open'); });
});

// ── Utilities ─────────────────────────────────────────────────────────────────
function safeJsonParse(s, def) {
    try { return JSON.parse(s); } catch { return def; }
}

// ── Init ──────────────────────────────────────────────────────────────────────
wsDot.className = 'ws-dot connecting';
wsStatus.textContent = 'Connecting…';
connectWs();

// Keep WS alive with pings
setInterval(() => {
    if (ws?.readyState === 1) ws.send(JSON.stringify({ type: 'ping' }));
}, 25000);

// Fix startAgentResponse: run_id comes from first WS event
// We patch handleWsEvent to set it on first event
const _handleWsEvent = handleWsEvent;
window.handleWsEvent = function (ev) {
    if (ev.run_id && !currentRunId && currentAgentBubble) {
        currentRunId = ev.run_id;
    }
    _handleWsEvent(ev);
};
ws && (ws.onmessage = e => {
    try { window.handleWsEvent(JSON.parse(e.data)); } catch { }
});

// Initial focus
setTimeout(() => chatInput.focus(), 500);