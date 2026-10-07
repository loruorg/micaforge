-- Micaforge: at most one queued occurrence of each recurring job.
--
-- A recurring job is a `jobs` row whose payload carries `repeat_secs`. The
-- schedule assumes there is exactly one such row per kind in `pending` or
-- `running` at any moment. Until now that was a `WHERE NOT EXISTS` check, which
-- two replicas booting at the same instant could both pass, leaving two copies
-- of every sweep running side by side from then on. This index makes it a
-- guarantee: `jobs::enqueue_recurring` inserts with `ON CONFLICT DO NOTHING`
-- against it.

-- Collapse any duplicates the old race already left behind, or the index
-- cannot be built. The row kept per kind is the running one if there is one,
-- else the earliest due. Extra `pending` copies are deleted; an extra `running`
-- copy belongs to a lane that may still be working on it, so it is marked
-- `done` instead, which lets that lane finish and record nothing new.
WITH ranked AS (
    SELECT id, status,
           row_number() OVER (
               PARTITION BY kind
               ORDER BY (status = 'running') DESC, run_at ASC, id ASC
           ) AS rank
    FROM jobs
    WHERE status IN ('pending', 'running')
      AND (payload -> 'repeat_secs') IS NOT NULL
)
DELETE FROM jobs
USING ranked
WHERE jobs.id = ranked.id
  AND ranked.rank > 1
  AND ranked.status = 'pending';

WITH ranked AS (
    SELECT id,
           row_number() OVER (PARTITION BY kind ORDER BY run_at ASC, id ASC) AS rank
    FROM jobs
    WHERE status = 'running'
      AND (payload -> 'repeat_secs') IS NOT NULL
)
UPDATE jobs SET status = 'done', locked_by = NULL, locked_at = NULL
FROM ranked
WHERE jobs.id = ranked.id
  AND ranked.rank > 1;

-- The expression must match the `ON CONFLICT ... WHERE` in
-- `jobs::enqueue_recurring` exactly, or Postgres cannot infer this index.
CREATE UNIQUE INDEX IF NOT EXISTS jobs_one_recurring_per_kind
    ON jobs (kind)
    WHERE status IN ('pending', 'running') AND (payload -> 'repeat_secs') IS NOT NULL;
