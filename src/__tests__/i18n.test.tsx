import { render, screen, within } from "@testing-library/preact";
import { describe, expect, it } from "vitest";
import { dictionaries, format, translator, type TranslationKey } from "../i18n";
import { MeetingNotesDetail, analysisChipKey, describeError, staleReasonKey } from "../meeting_notes_ui";
import { DESKTOP_VIEWPORT, MOBILE_VIEWPORT, setViewport } from "../test/dom";
import { analysisResult, appSettings, completeAnalysisView, errorPayload, globalContent, inputSnapshot, transcriptDocument } from "../test/fixtures";
import { detailProps, type DetailProps, type Translate } from "../test/render";
import { script } from "../test/tauri";
import type { AnalysisState, MeetingNotesErrorCode, StaleReason } from "../types";

const zh = translator("zh-CN");
const en = translator("en-US");
const TRANSLATION_KEYS = new Set(Object.keys(dictionaries["zh-CN"]));

const ERROR_CODES = [
  "MEETING_NOTES_NOT_CONFIGURED",
  "INVALID_ENDPOINT",
  "INSECURE_ENDPOINT",
  "RESPONSE_TOO_LARGE",
  "SESSION_NOT_FOUND",
  "SESSION_ID_CONFLICT",
  "SESSION_DELETED",
  "SESSION_STILL_RECORDING",
  "TRANSCRIPT_NOT_READY",
  "TRANSCRIPT_DOCUMENT_CORRUPT",
  "TRANSCRIPT_INVALID",
  "NO_TRANSCRIPT_CONTENT",
  "PARTIAL_CONFIRMATION_REQUIRED",
  "CONTEXT_DRAFT_INVALID",
  "CONTEXT_REVISION_CONFLICT",
  "CONTEXT_TOO_LARGE",
  "PROVIDER_CHANGED",
  "PROVIDER_UNAUTHORIZED",
  "PROVIDER_FORBIDDEN",
  "PROVIDER_RATE_LIMITED",
  "PROVIDER_TIMEOUT",
  "PROVIDER_UNAVAILABLE",
  "PROVIDER_RESPONSE_INVALID",
  "ANALYSIS_BUSY",
  "CLEAN_OUTPUT_INVALID",
  "SUMMARY_OUTPUT_INVALID",
  "SUMMARY_REDUCE_DID_NOT_CONVERGE",
  "SUMMARY_CANDIDATE_TOO_LARGE",
  "RETRY_EXHAUSTED",
  "ANALYSIS_CHECKPOINT_CORRUPT",
  "ANALYSIS_DOCUMENT_CORRUPT",
  "IO_ERROR",
] as const satisfies readonly MeetingNotesErrorCode[];

// Fails to compile when types.ts gains an error code this file does not list.
type AssertNever<T extends never> = T;
type EveryCodeListed = AssertNever<Exclude<MeetingNotesErrorCode, (typeof ERROR_CODES)[number]>>;
const ALL_STATES: AnalysisState[] = ["draft", "queued", "cleaning", "summarizing", "complete", "cancelled", "failed"];
const ALL_STALE_REASONS: StaleReason[] = ["globalContextChanged", "meetingContextChanged", "transcriptChanged", "providerChanged", "outputLanguageChanged", "pipelineChanged"];

function messageKeyFor(code: MeetingNotesErrorCode) {
  const pascal = code.toLowerCase().split("_").map((part) => part[0].toUpperCase() + part.slice(1)).join("");
  return `meetingNotesError${pascal}` as TranslationKey;
}

function renderDetail(t: Translate, overrides: Partial<DetailProps> = {}) {
  script("get_session_transcript", () => transcriptDocument());
  return render(<MeetingNotesDetail {...detailProps({
    analysis: completeAnalysisView({ freshness: "stale", staleReasons: ["globalContextChanged"] }),
    globalContext: globalContent({ knowledgeBackground: "当前全局背景" }),
    settings: appSettings((next) => { next.meetingNotes.endpoint = "https://api.example.com/v1"; }),
    t,
    ...overrides,
  })} />);
}

function visibleText(root: Element) {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const texts: string[] = [];
  for (let node = walker.nextNode(); node; node = walker.nextNode()) texts.push((node.textContent ?? "").trim());
  return texts;
}

describe("localized detail view", () => {
  it("renders a full result in Chinese", () => {
    const { container } = renderDetail(zh);

    expect(within(screen.getByRole("tablist")).getByRole("tab", { name: zh("tabSummary") })).toBeTruthy();
    expect(screen.getByRole("button", { name: zh("regenerate") })).toBeTruthy();
    expect(screen.getByText(zh("resultGeneratedAt"), { exact: false })).toBeTruthy();
    expect(document.querySelector(".run-stale")?.textContent).toContain(zh("staleGlobalContextChanged"));
    expect(visibleText(container).length).toBeGreaterThan(20);
    expect(visibleText(container).filter((text) => TRANSLATION_KEYS.has(text))).toEqual([]);
    expect(container.textContent).not.toMatch(/meetingNotes[A-Z]/);
  });

  it("renders the same result in English", () => {
    const { container } = renderDetail(en);

    expect(within(screen.getByRole("tablist")).getByRole("tab", { name: en("tabSummary") })).toBeTruthy();
    expect(screen.getByRole("button", { name: en("regenerate") })).toBeTruthy();
    expect(screen.getByText(en("resultGeneratedAt"), { exact: false })).toBeTruthy();
    expect(document.querySelector(".run-stale")?.textContent).toContain(en("staleGlobalContextChanged"));
    expect(visibleText(container).length).toBeGreaterThan(20);
    expect(visibleText(container).filter((text) => TRANSLATION_KEYS.has(text))).toEqual([]);
    expect(container.textContent).not.toMatch(/meetingNotes[A-Z]/);
  });

  it("discloses the endpoint host and that audio is never sent", () => {
    renderDetail(zh);
    const disclosure = document.querySelector(".run-disclosure")?.textContent ?? "";
    expect(disclosure).toContain("api.example.com");
    expect(disclosure).toContain("音频不会发送");
    expect(disclosure).not.toContain("{host}");
  });

  it("names an unconfigured endpoint instead of leaving the sentence empty", () => {
    renderDetail(en, { settings: appSettings((next) => { next.meetingNotes.endpoint = ""; }) });
    expect(document.querySelector(".run-disclosure")?.textContent).toContain(en("meetingNotesEndpointUnset"));
  });

  // jsdom has no layout engine, so this only pins structure: every control stays
  // rendered and nothing hard-codes nowrap or a fixed pixel width inline.
  it("keeps every detail control rendered at 1280x800 and 375x667", () => {
    for (const viewport of [DESKTOP_VIEWPORT, MOBILE_VIEWPORT]) {
      setViewport(viewport);
      const longModel = "meeting-notes-model-with-a-very-long-unbreakable-identifier-0123456789";
      const view = renderDetail(zh, { analysis: completeAnalysisView({ lastSuccessfulResult: analysisResult({ inputSnapshot: inputSnapshot({ provider: { authMode: "bearer", endpoint: "https://api.example.com/v1", maxInputCharacters: 48_000, model: longModel, requestTimeoutSeconds: 180 } }) }) }) });

      expect(within(screen.getByRole("tablist")).getAllByRole("tab").map((tab) => tab.textContent)).toEqual([zh("tabTranscript"), zh("tabCleaned"), zh("tabSummary"), zh("tabContext")]);
      expect(screen.getByRole("button", { name: zh("regenerate") })).toBeTruthy();
      expect(screen.getByText(longModel, { exact: false })).toBeTruthy();
      const inlineStyles = [...view.container.querySelectorAll("[style]")].map((element) => element.getAttribute("style") ?? "");
      expect(inlineStyles.filter((style) => /nowrap|width:\s*\d/.test(style))).toEqual([]);
      view.unmount();
    }
  });
});

describe("translation dictionaries", () => {
  it("keeps the Chinese and English key sets identical", () => {
    expect(Object.keys(dictionaries["en-US"]).sort()).toEqual(Object.keys(dictionaries["zh-CN"]).sort());
    for (const [key, value] of Object.entries(dictionaries["en-US"])) expect(value.length, key).toBeGreaterThan(0);
    for (const [key, value] of Object.entries(dictionaries["zh-CN"])) expect(value.length, key).toBeGreaterThan(0);
  });

  it("localizes every meeting-notes error code in both languages", () => {
    expect(ERROR_CODES).toHaveLength(32);
    for (const code of ERROR_CODES) {
      const key = messageKeyFor(code);
      expect(dictionaries["zh-CN"][key], code).toBeTruthy();
      expect(dictionaries["en-US"][key], code).toBeTruthy();
      expect(describeError(zh, errorPayload({ code, messageKey: key }))).toBe(dictionaries["zh-CN"][key]);
    }
    const declared = new Set<string>(ERROR_CODES.map(messageKeyFor));
    expect(Object.keys(dictionaries["zh-CN"]).filter((key) => key.startsWith("meetingNotesError") && !declared.has(key))).toEqual([]);
  });

  it("localizes every run state and stale reason", () => {
    for (const state of ALL_STATES) expect(zh(analysisChipKey(state)), state).toBeTruthy();
    for (const reason of ALL_STALE_REASONS) expect(en(staleReasonKey(reason)), reason).toBeTruthy();
  });

  it("fills message parameters and falls back to the raw reason", () => {
    const payload = errorPayload({ code: "CONTEXT_TOO_LARGE", messageKey: "meetingNotesErrorContextTooLarge", params: { characters: 21_000, limit: 20_000 }, retryable: false });
    const expected = format(dictionaries["zh-CN"].meetingNotesErrorContextTooLarge, { characters: 21_000, limit: 20_000 });
    expect(describeError(zh, payload)).toBe(expected);
    expect(describeError(zh, JSON.stringify(payload))).toBe(expected);
    expect(describeError(zh, payload)).not.toContain("{characters}");
    expect(describeError(zh, "backend exploded")).toBe("backend exploded");
    expect(describeError(zh, errorPayload({ code: "IO_ERROR", messageKey: "notATranslationKey" }))).toBe("IO_ERROR");
  });
});
