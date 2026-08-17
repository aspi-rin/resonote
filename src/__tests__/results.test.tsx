import { fireEvent, render, screen, waitFor, within } from "@testing-library/preact";
import { describe, expect, it, vi } from "vitest";
import { translator } from "../i18n";
import { hasSpokenContent, MeetingNotesChips, MeetingNotesDetail, SummaryPanel } from "../meeting_notes_ui";
import { clipboardWrite, confirmDialog, scrollIntoView } from "../test/dom";
import { CLEANED_SEGMENTS, analysisResult, completeAnalysisView, errorPayload, globalContent, meetingContent, runView, transcriptDocument, transcriptSegment } from "../test/fixtures";
import { detailProps, selectDetailTab, type DetailProps } from "../test/render";
import { script } from "../test/tauri";
import type { RunError, TranscriptDocument } from "../types";

const t = translator("en-US");

function runError(overrides: Partial<RunError> = {}): RunError {
  return { causeCode: null, code: "PROVIDER_TIMEOUT", httpStatus: null, messageKey: "meetingNotesErrorProviderTimeout", retryable: true, stage: "cleaning", ...overrides };
}

function renderDetail(overrides: Partial<DetailProps> = {}, transcript: TranscriptDocument = transcriptDocument()) {
  script("get_session_transcript", () => transcript);
  const props = detailProps({ analysis: completeAnalysisView(), globalContext: globalContent({ knowledgeBackground: "当前全局背景" }), ...overrides });
  return { ...render(<MeetingNotesDetail {...props} />), props };
}

describe("result rendering", () => {
  it("shows a fresh summary without a stale banner", () => {
    renderDetail();

    expect(screen.getByRole("heading", { name: "Q3 路线图评审" })).toBeTruthy();
    expect(screen.getByText("会议确认了下季度优先级与预算安排。")).toBeTruthy();
    expect(screen.getByText("决定冻结本季度预算")).toBeTruthy();
    expect(screen.getByText(t("resultFresh"))).toBeTruthy();
    expect(document.querySelector(".run-stale")).toBeNull();
    expect(screen.getByText(t("resultInputComplete"))).toBeTruthy();
  });

  it("lists localized stale reasons next to a regenerate button", () => {
    renderDetail({ analysis: completeAnalysisView({ freshness: "stale", staleReasons: ["globalContextChanged", "transcriptChanged"] }) });

    const stale = document.querySelector(".run-stale");
    expect(stale?.textContent).toBe(`${t("resultStale")}: ${t("staleGlobalContextChanged")}; ${t("staleTranscriptChanged")}`);
    expect((screen.getByRole("button", { name: t("regenerate") }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("confirms before regenerating over an existing result", async () => {
    const { props } = renderDetail();
    await waitFor(() => expect(screen.getByRole("button", { name: t("regenerate") })).toBeTruthy());

    confirmDialog.mockReturnValueOnce(false);
    fireEvent.click(screen.getByRole("button", { name: t("regenerate") }));
    expect(props.generate).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: t("regenerate") }));
    expect(confirmDialog).toHaveBeenCalledTimes(2);
    expect(vi.mocked(props.generate).mock.calls[0][1]).toEqual({ acceptPartial: false, mode: "regenerate" });
  });

  it("shows a retry button for a retryable failure", () => {
    const { props } = renderDetail({ analysis: completeAnalysisView({ currentRun: runView({ error: runError(), state: "failed" }), lastSuccessfulResult: null }) });

    expect(screen.getByText(t("meetingNotesErrorProviderTimeout"))).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: t("retryRun") }));
    expect(props.retry).toHaveBeenCalledTimes(1);
  });

  it("hides the retry button for a failure that cannot be retried", () => {
    renderDetail({ analysis: completeAnalysisView({ currentRun: runView({ error: runError({ code: "TRANSCRIPT_INVALID", messageKey: "meetingNotesErrorTranscriptInvalid", retryable: false }), state: "failed" }), lastSuccessfulResult: null }) });

    expect(screen.getByText(t("meetingNotesErrorTranscriptInvalid"))).toBeTruthy();
    expect(screen.queryByRole("button", { name: t("retryRun") })).toBeNull();
  });

  it("offers resume and regenerate after a cancelled run", () => {
    const { props } = renderDetail({ analysis: completeAnalysisView({ currentRun: runView({ state: "cancelled" }) }) });

    expect(screen.getByRole("button", { name: t("regenerate") })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: t("resumeRun") }));
    expect(props.retry).toHaveBeenCalledTimes(1);
  });

  it("offers a jump to settings when the provider is not configured", () => {
    const { props } = renderDetail({
      analysis: completeAnalysisView({ currentRun: runView({ error: runError({ code: "MEETING_NOTES_NOT_CONFIGURED", messageKey: "meetingNotesErrorMeetingNotesNotConfigured", retryable: false }), state: "failed" }), lastSuccessfulResult: null }),
      error: errorPayload({ code: "MEETING_NOTES_NOT_CONFIGURED", messageKey: "meetingNotesErrorMeetingNotesNotConfigured", retryable: false }),
    });

    expect(screen.getAllByText(t("meetingNotesErrorMeetingNotesNotConfigured"))).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: t("openMeetingNotesSettings") }));
    expect(props.openSettings).toHaveBeenCalledTimes(1);
  });

  it("marks a partial result in the chips and in the result meta", () => {
    const analysis = completeAnalysisView({
      freshness: "stale",
      lastSuccessfulResult: analysisResult({ inputQuality: { completedSegmentCount: 2, failedSegmentCount: 1, kind: "partial", skippedSegmentCount: 0 } }),
    });
    const view = render(<MeetingNotesChips analysis={analysis} state="complete" t={t} />);
    expect([...view.container.querySelectorAll(".mn-chip")].map((chip) => chip.textContent)).toEqual([
      `${t("meetingNotes")}: ${t("meetingNotesComplete")}`,
      t("meetingNotesPartialBadge"),
      t("meetingNotesStaleBadge"),
    ]);
    view.unmount();

    renderDetail({ analysis });
    expect(screen.getByText(t("resultInputPartial"))).toBeTruthy();
  });

  it("renders no chips before the first status snapshot", () => {
    const view = render(<MeetingNotesChips analysis={null} state={null} t={t} />);
    expect(view.container.querySelector(".mn-chip")).toBeNull();
  });
});

describe("source timestamps", () => {
  it("labels sources with the cleaned segment start time", () => {
    const onSelect = vi.fn();
    const result = analysisResult();
    render(<SummaryPanel copied={false} copy={vi.fn()} result={result} t={t} onSelect={onSelect} />);

    const decision = screen.getByText("决定冻结本季度预算").closest("li") as HTMLElement;
    fireEvent.click(within(decision).getByRole("button", { name: "01:05" }));
    expect(onSelect).toHaveBeenCalledWith(2);

    const overview = screen.getByText(result.summary.overview.text).closest("p") as HTMLElement;
    expect(within(overview).getByRole("button", { name: "00:05" })).toBeTruthy();
  });

  it("opens and highlights the cleaned segment behind a timestamp", () => {
    renderDetail();

    fireEvent.click(within(screen.getByText("决定冻结本季度预算").closest("li") as HTMLElement).getByRole("button", { name: "01:05" }));

    const highlighted = document.querySelector(".transcript-row.highlighted");
    expect(highlighted?.textContent).toContain("预算保持不变。");
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect((within(screen.getByRole("tablist")).getByRole("tab", { name: t("tabCleaned") }) as HTMLElement).getAttribute("aria-selected")).toBe("true");
  });
});

describe("raw transcript visibility", () => {
  it("hides a contentless complete segment while keeping normal, processing and failed rows", async () => {
    renderDetail({}, transcriptDocument([
      transcriptSegment({ id: 1, text: "我们确认下季度优先级。" }),
      transcriptSegment({ id: 2, text: "..." }),
      transcriptSegment({ id: 3, status: "processing", text: "" }),
      transcriptSegment({ id: 4, status: "failed", text: "" }),
    ]));
    selectDetailTab(t, "tabTranscript");

    expect(await screen.findByText("我们确认下季度优先级。")).toBeTruthy();
    expect(document.querySelectorAll(".transcript-row")).toHaveLength(3);
    expect(screen.getByText(`${t("transcribing")}…`)).toBeTruthy();
    expect(screen.getByText(t("transcriptFailed"))).toBeTruthy();
    expect(screen.queryByText("...")).toBeNull();
  });
});

describe("hasSpokenContent", () => {
  it("treats empty or punctuation-only text as contentless", () => {
    expect(hasSpokenContent("...")).toBe(false);
    expect(hasSpokenContent("。。。")).toBe(false);
    expect(hasSpokenContent("")).toBe(false);
    expect(hasSpokenContent(" . , ! ")).toBe(false);
  });

  it("treats any letter or digit as content", () => {
    expect(hasSpokenContent("こんにちは")).toBe(true);
    expect(hasSpokenContent("OK.")).toBe(true);
    expect(hasSpokenContent("3")).toBe(true);
  });
});

describe("current and snapshot context", () => {
  it("separates the editable meeting context from the frozen snapshot", () => {
    renderDetail();
    selectDetailTab(t, "tabContext");

    expect((screen.getByLabelText(t("contextTitle")) as HTMLInputElement).value).toBe("当前会议标题");
    const current = screen.getByText(t("contextCurrentGlobal")).closest(".context-block") as HTMLElement;
    expect(within(current).getByText("当前全局背景")).toBeTruthy();
    expect(within(current).getByText(t("contextKnowledge"))).toBeTruthy();

    const snapshot = screen.getByText(t("contextSnapshot")).closest(".context-block") as HTMLElement;
    expect(snapshot.classList.contains("snapshot")).toBe(true);
    expect(within(snapshot).getByText("快照全局背景")).toBeTruthy();
    expect(within(snapshot).getByText("快照会议标题")).toBeTruthy();
    expect(within(snapshot).queryByText("当前全局背景")).toBeNull();
  });

  it("hides the snapshot block until a result exists", () => {
    renderDetail({ analysis: completeAnalysisView({ lastSuccessfulResult: null }) });
    selectDetailTab(t, "tabContext");

    expect(screen.queryByText(t("contextSnapshot"))).toBeNull();
    expect(screen.getByText(t("contextCurrentGlobal"))).toBeTruthy();
  });
});

describe("copy actions", () => {
  it("copies the cleaned transcript with timestamps", () => {
    renderDetail();
    selectDetailTab(t, "tabCleaned");

    fireEvent.click(screen.getByRole("button", { name: t("copyText") }));
    expect(clipboardWrite).toHaveBeenCalledWith("[00:05] 我们确认下季度优先级。\n[01:05] 预算保持不变。");
  });

  it("skips contentless segments when copying the cleaned transcript", () => {
    renderDetail({ analysis: completeAnalysisView({ lastSuccessfulResult: analysisResult({ cleaned: { segments: [...CLEANED_SEGMENTS, { endMs: 75_000, segmentId: 3, startMs: 71_000, text: "..." }] } }) }) });
    selectDetailTab(t, "tabCleaned");

    fireEvent.click(screen.getByRole("button", { name: t("copyText") }));
    expect(clipboardWrite).toHaveBeenCalledWith("[00:05] 我们确认下季度优先级。\n[01:05] 预算保持不变。");
  });

  it("copies the summary as plain text", () => {
    renderDetail();

    fireEvent.click(screen.getByRole("button", { name: t("copyText") }));
    const text = clipboardWrite.mock.calls[0][0];
    expect(text.startsWith("Q3 路线图评审")).toBe(true);
    expect(text).toContain(`${t("summaryOverview")}: 会议确认了下季度优先级与预算安排。`);
    expect(text).toContain(`${t("summaryDecisions")}\n- 决定冻结本季度预算`);
    expect(text).toContain(`- 更新路线图文档 (${t("summaryOwner")}: 张三, ${t("summaryDueDate")}: 2026-08-20)`);
    expect(text).toContain(`${t("summaryOpenQuestions")}\n- ${t("summaryEmptySection")}`);
  });

  it("shows the copied state on the button", async () => {
    renderDetail({ analysis: completeAnalysisView({ meetingContext: { content: meetingContent(), revision: 2, updatedAt: "" } }) });

    fireEvent.click(screen.getByRole("button", { name: t("copyText") }));
    expect(await screen.findByRole("button", { name: t("copied") })).toBeTruthy();
  });
});
