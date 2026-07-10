CREATE TABLE IF NOT EXISTS tokens (
	token TEXT PRIMARY KEY NOT NULL,
	provider TEXT NOT NULL,
	provisioned_cost REAL NOT NULL,
	current_cost REAL NOT NULL DEFAULT 0.0,
	creation_time REAL NOT NULL,
	expiration_time REAL NOT NULL,
	allowed_ips TEXT NOT NULL);