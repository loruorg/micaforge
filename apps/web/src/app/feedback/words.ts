/**
 * The words for feedback's keys, in one place.
 *
 * The dialog that files a report and the admin panel that reads it must call
 * the same thing by the same name: a reporter who chose "Feature request"
 * should see an administrator answer about a "Feature request", not a
 * "feature". The order of each list is the order the controls offer it in.
 */
import type { FeedbackKind, FeedbackSeverity, FeedbackSource, FeedbackStatus } from "../../lib/api";

export const KIND_WORDS: Record<FeedbackKind, string> = {
  bug: "Bug",
  problem: "Problem",
  feature: "Feature request",
  other: "Other",
};

export const SEVERITY_WORDS: Record<FeedbackSeverity, string> = {
  low: "Low",
  medium: "Medium",
  high: "High",
  critical: "Critical",
};

export const STATUS_WORDS: Record<FeedbackStatus, string> = {
  new: "New",
  triaged: "Triaged",
  planned: "Planned",
  done: "Done",
  wont_fix: "Won't fix",
};

export const SOURCE_WORDS: Record<FeedbackSource, string> = {
  dashboard: "Dashboard",
  mcp: "MCP",
  api: "API",
  chat: "Chat app",
};
