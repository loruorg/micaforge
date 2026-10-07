-- Micaforge: seed the agent catalog.
--
-- `agent_catalog` is read by the verification path and by the policy report, and
-- the `refresh_agent_ranges` job reseeds it from the compiled-in catalog in
-- `micaforge_agents::catalog::CATALOG` every twelve hours. That job first runs
-- ten minutes after boot, which would leave a fresh install with an empty table
-- for its first ten minutes and an empty *agent catalog screen* to go with it.
-- This migration removes that gap: the rows are there before the first request.
--
-- What is seeded, and what is not:
--
--   * `slug`, `display_name`, `operator`, `purpose`, `ua_patterns`,
--     `rdns_suffixes`, `docs_url` and `respects_robots` are facts about the
--     agent. They come straight from the compiled catalog and change only when
--     this product ships a new build.
--   * `ip_ranges` is deliberately left empty. Those come from each operator's
--     own published document and are fetched at runtime; writing a snapshot into
--     a migration would bake a set of prefixes into the schema and then serve
--     them forever on an install where the refresh job never ran. Until the job
--     has run once, verification answers `unknown`, which is an honest answer and
--     is reported as one.
--
-- ON CONFLICT DO NOTHING, not DO UPDATE. A migration must never overwrite the
-- ranges an operator's install has already fetched, and it must never undo an
-- edit an operator made to a row on purpose.
--
-- `respects_robots` records what the operator SAYS. Two entries below say `false`
-- because their operators state plainly that the agent does not consult
-- robots.txt; that is a claim on the record, and the policy report measures
-- behaviour against it rather than assuming it.

INSERT INTO agent_catalog
    (slug, display_name, operator, purpose, ua_patterns, ip_ranges, rdns_suffixes, docs_url, respects_robots)
VALUES
    -- openai
    ('gptbot', 'GPTBot', 'openai', 'training', '["GPTBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://platform.openai.com/docs/bots', true),
    ('oai-searchbot', 'OAI-SearchBot', 'openai', 'search', '["OAI-SearchBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://platform.openai.com/docs/bots', true),
    ('chatgpt-user', 'ChatGPT-User', 'openai', 'rag', '["ChatGPT-User"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://platform.openai.com/docs/bots', true),
    -- anthropic
    ('claudebot', 'ClaudeBot', 'anthropic', 'training', '["ClaudeBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://support.anthropic.com/en/articles/8896518-does-anthropic-crawl-data-from-the-web-and-how-can-site-owners-block-the-crawler', true),
    ('claude-user', 'Claude-User', 'anthropic', 'rag', '["Claude-User"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://support.anthropic.com/en/articles/8896518-does-anthropic-crawl-data-from-the-web-and-how-can-site-owners-block-the-crawler', true),
    ('claude-searchbot', 'Claude-SearchBot', 'anthropic', 'search', '["Claude-SearchBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://support.anthropic.com/en/articles/8896518-does-anthropic-crawl-data-from-the-web-and-how-can-site-owners-block-the-crawler', true),
    ('anthropic-ai', 'anthropic-ai (retired)', 'anthropic', 'training', '["anthropic-ai"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    ('claude-web', 'Claude-Web (retired)', 'anthropic', 'rag', '["Claude-Web"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- perplexity
    ('perplexitybot', 'PerplexityBot', 'perplexity', 'search', '["PerplexityBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://docs.perplexity.ai/guides/bots', true),
    ('perplexity-user', 'Perplexity-User', 'perplexity', 'rag', '["Perplexity-User"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://docs.perplexity.ai/guides/bots', false),
    -- google
    ('googlebot', 'Googlebot', 'google', 'search', '["Googlebot"]'::jsonb, '[]'::jsonb, '["googlebot.com","google.com"]'::jsonb, 'https://developers.google.com/search/docs/crawling-indexing/overview-google-crawlers', true),
    ('google-extended', 'Google-Extended', 'google', 'training', '[]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://developers.google.com/search/docs/crawling-indexing/overview-google-crawlers', true),
    ('googleother', 'GoogleOther', 'google', 'eval', '["GoogleOther"]'::jsonb, '[]'::jsonb, '["googlebot.com","google.com"]'::jsonb, 'https://developers.google.com/search/docs/crawling-indexing/google-special-case-crawlers', true),
    ('google-cloudvertexbot', 'Google-CloudVertexBot', 'google', 'rag', '["Google-CloudVertexBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://developers.google.com/search/docs/crawling-indexing/google-special-case-crawlers', true),
    ('google-inspectiontool', 'Google-InspectionTool', 'google', 'eval', '["Google-InspectionTool"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://developers.google.com/search/docs/crawling-indexing/google-user-triggered-fetchers', true),
    -- microsoft
    ('bingbot', 'bingbot', 'microsoft', 'search', '["bingbot"]'::jsonb, '[]'::jsonb, '["search.msn.com"]'::jsonb, 'https://www.bing.com/webmasters/help/which-crawlers-does-bing-use-8c184ec0', true),
    ('bingpreview', 'BingPreview', 'microsoft', 'preview', '["BingPreview"]'::jsonb, '[]'::jsonb, '["search.msn.com"]'::jsonb, 'https://www.bing.com/webmasters/help/which-crawlers-does-bing-use-8c184ec0', true),
    ('msnbot', 'msnbot', 'microsoft', 'search', '["msnbot"]'::jsonb, '[]'::jsonb, '["search.msn.com"]'::jsonb, 'https://www.bing.com/webmasters/help/which-crawlers-does-bing-use-8c184ec0', true),
    -- meta
    ('meta-externalagent', 'meta-externalagent', 'meta', 'training', '["meta-externalagent"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://developers.facebook.com/docs/sharing/webmasters/web-crawlers', true),
    ('meta-externalfetcher', 'meta-externalfetcher', 'meta', 'rag', '["meta-externalfetcher"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://developers.facebook.com/docs/sharing/webmasters/web-crawlers', true),
    ('facebookbot', 'FacebookBot', 'meta', 'training', '["FacebookBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://developers.facebook.com/docs/sharing/webmasters/web-crawlers', true),
    ('facebookexternalhit', 'facebookexternalhit', 'meta', 'preview', '["facebookexternalhit"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://developers.facebook.com/docs/sharing/webmasters/web-crawlers', true),
    -- apple
    ('applebot', 'Applebot', 'apple', 'search', '["Applebot"]'::jsonb, '[]'::jsonb, '["applebot.apple.com"]'::jsonb, 'https://support.apple.com/en-us/119829', true),
    ('applebot-extended', 'Applebot-Extended', 'apple', 'training', '[]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://support.apple.com/en-us/119829', true),
    -- amazon
    ('amazonbot', 'Amazonbot', 'amazon', 'search', '["Amazonbot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://developer.amazon.com/amazonbot', true),
    -- bytedance
    ('bytespider', 'Bytespider', 'bytedance', 'training', '["Bytespider"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- common-crawl
    ('ccbot', 'CCBot', 'common-crawl', 'training', '["CCBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://commoncrawl.org/ccbot', true),
    -- cohere
    ('cohere-training-data-crawler', 'cohere-training-data-crawler', 'cohere', 'training', '["cohere-training-data-crawler"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    ('cohere-ai', 'cohere-ai', 'cohere', 'rag', '["cohere-ai"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- mistral
    ('mistralai-user', 'MistralAI-User', 'mistral', 'rag', '["MistralAI-User"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://docs.mistral.ai/robots', true),
    -- you
    ('youbot', 'YouBot', 'you', 'search', '["YouBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://about.you.com/youbot/', true),
    -- duckduckgo
    ('duckassistbot', 'DuckAssistBot', 'duckduckgo', 'rag', '["DuckAssistBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://duckduckgo.com/duckduckgo-help-pages/results/duckassistbot/', true),
    ('duckduckbot', 'DuckDuckBot', 'duckduckgo', 'search', '["DuckDuckBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://duckduckgo.com/duckduckgo-help-pages/results/duckduckbot/', true),
    -- allen-institute
    ('ai2bot', 'AI2Bot', 'allen-institute', 'training', '["AI2Bot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://allenai.org/crawler', true),
    -- diffbot
    ('diffbot', 'Diffbot', 'diffbot', 'training', '["Diffbot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- webz-io
    ('omgilibot', 'omgilibot', 'webz-io', 'training', '["omgilibot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    ('omgili', 'omgili', 'webz-io', 'training', '["omgili"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- timpi
    ('timpibot', 'Timpibot', 'timpi', 'search', '["Timpibot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- huawei
    ('pangubot', 'PanguBot', 'huawei', 'training', '["PanguBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    ('petalbot', 'PetalBot', 'huawei', 'search', '["PetalBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://webmaster.petalsearch.com/site/petalbot', true),
    -- kangaroo-llm
    ('kangaroo-bot', 'Kangaroo Bot', 'kangaroo-llm', 'training', '["Kangaroo Bot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- imagesift
    ('imagesiftbot', 'ImagesiftBot', 'imagesift', 'training', '["ImagesiftBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://imagesift.com/about', true),
    -- open-source
    ('img2dataset', 'img2dataset', 'open-source', 'training', '["img2dataset"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, false),
    -- firecrawl
    ('firecrawl', 'FirecrawlAgent', 'firecrawl', 'rag', '["FirecrawlAgent"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://docs.firecrawl.dev/', true),
    -- semrush
    ('semrushbot-ocob', 'SemrushBot-OCOB', 'semrush', 'training', '["SemrushBot-OCOB"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://www.semrush.com/bot/', true),
    ('semrushbot', 'SemrushBot', 'semrush', 'unknown', '["SemrushBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://www.semrush.com/bot/', true),
    -- ahrefs
    ('ahrefsbot', 'AhrefsBot', 'ahrefs', 'unknown', '["AhrefsBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://ahrefs.com/robot', true),
    -- yandex
    ('yandexbot', 'YandexBot', 'yandex', 'search', '["YandexBot"]'::jsonb, '[]'::jsonb, '["yandex.ru","yandex.net","yandex.com"]'::jsonb, 'https://yandex.com/support/webmaster/robot-workings/check-yandex-robots.html', true),
    -- baidu
    ('baiduspider', 'Baiduspider', 'baidu', 'search', '["Baiduspider"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- x
    ('twitterbot', 'Twitterbot', 'x', 'preview', '["Twitterbot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- linkedin
    ('linkedinbot', 'LinkedInBot', 'linkedin', 'preview', '["LinkedInBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- slack
    ('slackbot', 'Slackbot', 'slack', 'preview', '["Slackbot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, 'https://api.slack.com/robots', true),
    -- discord
    ('discordbot', 'Discordbot', 'discord', 'preview', '["Discordbot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true),
    -- telegram
    ('telegrambot', 'TelegramBot', 'telegram', 'preview', '["TelegramBot"]'::jsonb, '[]'::jsonb, '[]'::jsonb, NULL, true)
ON CONFLICT (slug) DO NOTHING;
