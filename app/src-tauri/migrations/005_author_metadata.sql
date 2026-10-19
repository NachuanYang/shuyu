ALTER TABLE books
ADD COLUMN author_metadata_checked INTEGER NOT NULL DEFAULT 0
CHECK (author_metadata_checked IN (0, 1));
