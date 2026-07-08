-- known_hosts-style pinning: the SHA-256 fingerprint of the host key seen on
-- first connect is stored here and compared on every subsequent connect
-- (see tools/ssh.rs Client::check_server_key). NULL means "not yet trusted".
ALTER TABLE ssh_servers ADD COLUMN host_key_fingerprint TEXT;
