CREATE TABLE books (
  id TEXT PRIMARY KEY NOT NULL,
  sha256 TEXT NOT NULL UNIQUE,
  normalized_sha256 TEXT NOT NULL,
  title TEXT NOT NULL,
  author TEXT NOT NULL DEFAULT '未知作者',
  import_name TEXT NOT NULL,
  file_path TEXT NOT NULL,
  total_utf16 INTEGER NOT NULL CHECK (total_utf16 >= 0),
  chapter_count INTEGER NOT NULL CHECK (chapter_count >= 0),
  created_at TEXT NOT NULL,
  last_opened_at TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0 CHECK (is_deleted IN (0, 1)),
  parser_version INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX books_recent_idx ON books(is_deleted, last_opened_at DESC, created_at DESC);

CREATE TABLE chapters (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  title TEXT NOT NULL,
  start_utf16 INTEGER NOT NULL CHECK (start_utf16 >= 0),
  end_utf16 INTEGER NOT NULL CHECK (end_utf16 >= start_utf16),
  UNIQUE(book_id, ordinal)
);

CREATE TABLE paragraphs (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  text TEXT NOT NULL,
  start_utf16 INTEGER NOT NULL CHECK (start_utf16 >= 0),
  end_utf16 INTEGER NOT NULL CHECK (end_utf16 >= start_utf16),
  UNIQUE(book_id, ordinal)
);

CREATE INDEX paragraphs_chapter_idx ON paragraphs(book_id, chapter_id, ordinal);

CREATE TABLE reading_positions (
  book_id TEXT PRIMARY KEY NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
  paragraph_id TEXT NOT NULL REFERENCES paragraphs(id) ON DELETE CASCADE,
  intra_utf16 INTEGER NOT NULL CHECK (intra_utf16 >= 0),
  absolute_utf16 INTEGER NOT NULL CHECK (absolute_utf16 >= 0),
  nearby_text TEXT NOT NULL DEFAULT '',
  progress_ratio REAL NOT NULL DEFAULT 0 CHECK (progress_ratio BETWEEN 0 AND 1),
  updated_at TEXT NOT NULL
);

CREATE TABLE bookmarks (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  paragraph_id TEXT NOT NULL REFERENCES paragraphs(id) ON DELETE CASCADE,
  intra_utf16 INTEGER NOT NULL CHECK (intra_utf16 >= 0),
  created_at TEXT NOT NULL,
  note TEXT,
  UNIQUE(book_id, paragraph_id, intra_utf16)
);

CREATE TABLE highlights (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  start_paragraph_id TEXT NOT NULL REFERENCES paragraphs(id) ON DELETE CASCADE,
  start_intra_utf16 INTEGER NOT NULL CHECK (start_intra_utf16 >= 0),
  end_paragraph_id TEXT NOT NULL REFERENCES paragraphs(id) ON DELETE CASCADE,
  end_intra_utf16 INTEGER NOT NULL CHECK (end_intra_utf16 >= 0),
  text_snapshot TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE paragraph_views (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  paragraph_id TEXT NOT NULL REFERENCES paragraphs(id) ON DELETE CASCADE,
  viewed_at TEXT NOT NULL,
  dwell_ms INTEGER NOT NULL CHECK (dwell_ms >= 0)
);

CREATE INDEX paragraph_views_recent_idx ON paragraph_views(viewed_at DESC);

CREATE TABLE daily_quotes (
  local_date TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  chapter_id TEXT NOT NULL REFERENCES chapters(id) ON DELETE CASCADE,
  paragraph_id TEXT NOT NULL REFERENCES paragraphs(id) ON DELETE CASCADE,
  start_intra_utf16 INTEGER NOT NULL CHECK (start_intra_utf16 >= 0),
  end_intra_utf16 INTEGER NOT NULL CHECK (end_intra_utf16 > start_intra_utf16),
  text_snapshot TEXT NOT NULL,
  source_hash TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE reading_sessions (
  id TEXT PRIMARY KEY NOT NULL,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  started_at TEXT NOT NULL,
  ended_at TEXT,
  first_anchor TEXT,
  last_anchor TEXT
);

CREATE TABLE recommendation_cache (
  id TEXT PRIMARY KEY NOT NULL,
  date_key TEXT NOT NULL,
  provider TEXT NOT NULL,
  work_id TEXT NOT NULL,
  title TEXT NOT NULL,
  author TEXT NOT NULL,
  cover_id TEXT,
  source_url TEXT NOT NULL,
  reason TEXT NOT NULL,
  fetched_at TEXT NOT NULL,
  UNIQUE(provider, work_id, date_key)
);

CREATE TABLE preferences (
  key TEXT PRIMARY KEY NOT NULL,
  json_value TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
