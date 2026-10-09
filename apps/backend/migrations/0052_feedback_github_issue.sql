-- Micaforge: the GitHub issue a feedback report became.
--
-- A report about a site whose repository is linked to the GitHub App can be
-- opened as an issue in that repository. The issue is remembered on the
-- report, so the triage screen links to it and a second request answers with
-- the same issue instead of opening another.

ALTER TABLE feedback ADD COLUMN IF NOT EXISTS github_issue_url text;
ALTER TABLE feedback ADD COLUMN IF NOT EXISTS github_issue_number integer;
-- `owner/name` at the time, so the link still says where it went after the
-- repository is unlinked or renamed.
ALTER TABLE feedback ADD COLUMN IF NOT EXISTS github_issue_repo text;
