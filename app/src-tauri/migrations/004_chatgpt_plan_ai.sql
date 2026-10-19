INSERT OR IGNORE INTO preferences(key, json_value, updated_at)
SELECT 'ai_client_rollback_v4', json_value, updated_at
FROM preferences
WHERE key = 'ai_client';

INSERT INTO preferences(key, json_value, updated_at)
VALUES (
  'ai_client',
  '{"enabled":false,"selectedModelSlug":null,"allowAiQa":false,"allowAiDailyQuote":false,"privacyNoticeVersion":0,"privacyNoticeAcceptedAt":null}',
  datetime('now')
)
ON CONFLICT(key) DO UPDATE SET
  json_value = excluded.json_value,
  updated_at = excluded.updated_at;
