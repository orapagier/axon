---
name: verify
description: Build, launch, and drive the Axon dashboard (axon-ui + axon-agent) on Windows to verify UI/agent changes end-to-end.
---

# Verifying Axon locally (Windows)

## Build + launch

1. Frontend: `cd axon-ui && npm run build`
   - If `vite` is not recognized, run `npm install` first (a WSL session may
     have left Linux-flavored node_modules on the shared checkout).
   - If rollup fails on Windows: `npm i @rollup/rollup-win32-x64-msvc --no-save`.
2. Sync static: copy `axon-ui/dist/*` over `crates/axon-agent/static/`
   (gitignored; run.bat does the same). No need to clean first — index.html
   only references the new hashed assets.
3. Backend: run `target\debug\axon.exe` with working directory
   `crates\axon-agent` (picks up `.env` there). Dashboard: http://localhost:3000.
   Rust rebuild only needed if crates/ changed.
   - **Stop the process when done** (`Stop-Process -Name axon`): the local
     instance starts Telegram polling and can 409 against the live server bot.

## Drive (headless browser)

No e2e deps in the repo. Use `puppeteer-core` in the scratchpad pointed at
installed Chrome (`C:/Program Files/Google/Chrome/Application/chrome.exe`),
viewport 1440x900.

- Login: type `AXON_MASTER_KEY` (from `crates/axon-agent/.env`) into
  `input[type=password]`, click `button[type=submit]`, wait for `.shell-topbar`.
- Navigate: click `button.nav-item` matched by `.nav-label` text **or**
  `title` attr (labels are v-if-removed when the sidebar is collapsed).
- Pages render in `#page-<id>` sections (chat, models, tools, memories,
  tasks, workflows, crm, files, settings); pages mount lazily on first visit.
- Topbar search input: `.shell-topbar-search input`.
- Useful counts: tools rows `#page-tools .tool-row`; models — assert
  visually, `.models-list` children don't map 1:1 to rows.

## Gotchas

- Pre-existing 404s for `/icons/slack.png`, `discord.png`, `github.png`,
  `search.png` (`toolIcons.js` references icons that aren't in
  `axon-ui/public/icons/`) — not a regression.
- CRM: header search hides on Dashboard/Archived tabs by design; record
  tabs are server-searched via Enter (`/api/crm/<tab>?q=`).
