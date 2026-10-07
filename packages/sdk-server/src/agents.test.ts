import { describe, expect, it } from "vitest";

import { classify, isAgent, mirrorTokens } from "./agents.js";

describe("the agent mirror", () => {
  it("names an agent inside a full user agent sentence", () => {
    const ua =
      "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2; +https://openai.com/gptbot)";
    expect(classify(ua)).toEqual({ slug: "gptbot", operator: "openai" });
  });

  it("ignores case, because operators do not agree on it", () => {
    expect(classify("claudebot")?.slug).toBe("claudebot");
    expect(classify("CLAUDEBOT/1.0")?.slug).toBe("claudebot");
  });

  it("prefers the longest token when one is a substring of another", () => {
    // The whole reason the index is sorted by length. `semrushbot` is inside
    // `semrushbot-ocob`, and only one of the two is the AI opt-out crawler.
    expect(classify("Mozilla/5.0 (compatible; SemrushBot-OCOB/7~bl)")?.slug).toBe("semrushbot-ocob");
    expect(classify("Mozilla/5.0 (compatible; SemrushBot/7~bl)")?.slug).toBe("semrushbot");
  });

  it("keeps OpenAI's three tokens apart", () => {
    expect(classify("GPTBot/1.2")?.slug).toBe("gptbot");
    expect(classify("OAI-SearchBot/1.0")?.slug).toBe("oai-searchbot");
    expect(classify("ChatGPT-User/1.0")?.slug).toBe("chatgpt-user");
  });

  it("says nothing about a browser", () => {
    const chrome =
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Safari/537.36";
    expect(classify(chrome)).toBeNull();
    expect(isAgent(chrome)).toBe(false);
  });

  it("says nothing about an empty or missing user agent", () => {
    expect(classify("")).toBeNull();
    expect(classify(undefined)).toBeNull();
    expect(classify(null)).toBeNull();
  });

  it("holds no token that is a substring of a shorter one it should lose to", () => {
    const tokens = mirrorTokens();
    // Sorted longest first, so any pair that overlaps resolves the specific way.
    for (let i = 1; i < tokens.length; i++) {
      expect(tokens[i - 1].length).toBeGreaterThanOrEqual(tokens[i].length);
    }
  });
});
