ALTER TABLE daily_quotes ADD COLUMN source_local_date TEXT NOT NULL DEFAULT '';
ALTER TABLE daily_quotes ADD COLUMN selection_method TEXT NOT NULL DEFAULT 'local';
