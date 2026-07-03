-- Routing fix: the generic '\bserver\s*(\d+)?\b' and '\bbash\b' patterns sent
-- every server/bash mention — including "local server" — to ssh_tool
-- (remote-only), while shell_tool (local host) had no patterns at all, so the
-- router could never surface it. Drop the two hijacking rows here; seed.sql
-- (which runs right after migrations on the same boot) now carries the
-- shell_tool patterns and the narrower '\bremote\s+server\b' replacement.
DELETE FROM tool_patterns
WHERE tool_name = 'ssh_tool'
  AND pattern IN ('\bserver\s*(\d+)?\b', '\bbash\b');
