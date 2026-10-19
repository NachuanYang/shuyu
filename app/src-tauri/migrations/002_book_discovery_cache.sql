CREATE TABLE discovery_search_cache (
  query_hash TEXT PRIMARY KEY NOT NULL,
  query TEXT NOT NULL,
  result_json TEXT NOT NULL,
  fetched_at TEXT NOT NULL
);
