BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS material_sets(id TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS material_set_assets(
    asset_id TEXT PRIMARY KEY REFERENCES assets(id),
    material_set_id TEXT NOT NULL REFERENCES material_sets(id)
);
INSERT OR IGNORE INTO schema_migrations(version) VALUES(4);
COMMIT;
