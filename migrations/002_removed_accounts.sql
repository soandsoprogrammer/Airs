BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS removed_accounts(account_id TEXT PRIMARY KEY REFERENCES accounts(id));
INSERT OR IGNORE INTO schema_migrations(version) VALUES(2);
COMMIT;
