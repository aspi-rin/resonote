import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/preact";
import { describe, expect, it, vi } from "vitest";
import { format, translator } from "../i18n";
import { MeetingNotesDetail, analysisStatusFromEvent, analysisStatusFromView, generateReadiness, isNewerAnalysisStatus, needsPartialConfirmation } from "../meeting_notes_ui";
import {
  analysisResult,
  analysisView,
  completeAnalysisView,
  errorPayload,
  historyEntry,
  runView,
  scriptAppDefaults,
  statusEvent,
  transcriptDocument,
  transcriptSegment,
} from "../test/fixtures";
import { detailProps, expandSession, openTab, renderApp, type DetailProps } from "../test/render";
import { callsOf, emit, lastCall, script, scriptFailure } from "../test/tauri";
import type { GenerateSessionAnalysisRequest } from "../types";

const t = translator("en-US");

function renderDetail(overrides: Partial<DetailProps> = {}) {
  const props = detailProps(overrides);
  return { ...render(<MeetingNotesDetail {...props} />), props };
}

function generateButton() {
  return screen.getByRole("button", { name: t("generateMeetingNotes") }) as HTMLButtonElement;
}

function chipText() {
  return document.querySelector(".mn-chip")?.textContent ?? "";
}

describe("generate readiness", () => {
  it("blocks generation while the session is still recording", () => {
    script("get_session_transcript", () => null);
    renderDetail({ entry: historyEntry({ status: "recording", transcriptStatus: "processing" }) });

    expect(generateButton().disabled).toBe(true);
    expect(screen.getByText(t("notReadyRecording"))).toBeTruthy();
  });

  it("blocks generation while transcription is still running", () => {
    script("get_session_transcript", () => null);
    renderDetail({ entry: historyEntry({ transcriptStatus: "processing" }) });

    expect(generateButton().disabled).toBe(true);
    expect(screen.getByText(t("notReadyTranscript"))).toBeTruthy();
  });

  it("blocks generation when no transcript segment succeeded", async () => {
    script("get_session_transcript", () => transcriptDocument([transcriptSegment({ status: "failed", text: "" })]));
    renderDetail({ entry: historyEntry({ transcriptStatus: "partial" }) });

    expect(await screen.findByText(t("notReadyEmpty"))).toBeTruthy();
    expect(generateButton().disabled).toBe(true);
  });

  it("starts a run for a finished session", async () => {
    script("get_session_transcript", () => transcriptDocument());
    const { props } = renderDetail();

    await waitFor(() => expect(callsOf("get_session_transcript")).toHaveLength(1));
    expect(generateButton().disabled).toBe(false);
    fireEvent.click(generateButton());
    expect(props.generate).toHaveBeenCalledTimes(1);
    expect(vi.mocked(props.generate).mock.calls[0][1]).toEqual({ acceptPartial: false, mode: "ensure" });
  });

  it("maps every readiness case to its localized reason", () => {
    expect(generateReadiness(historyEntry({ status: "recording" }), null).reason).toBe("notReadyRecording");
    expect(generateReadiness(historyEntry({ transcriptStatus: "pending" }), null).reason).toBe("notReadyTranscript");
    expect(generateReadiness(historyEntry({ transcriptStatus: null }), null).reason).toBe("notReadyEmpty");
    expect(generateReadiness(historyEntry(), transcriptDocument([transcriptSegment({ status: "failed" })])).reason).toBe("notReadyEmpty");
    expect(generateReadiness(historyEntry(), transcriptDocument())).toEqual({ ready: true, reason: null });
    expect(needsPartialConfirmation(historyEntry({ transcriptStatus: "partial" }))).toBe(true);
    expect(needsPartialConfirmation(historyEntry({ status: "interrupted" }))).toBe(true);
    expect(needsPartialConfirmation(historyEntry())).toBe(false);
  });
});

describe("partial confirmation", () => {
  it("confirms the segment counts before generating from a partial transcript", async () => {
    scriptAppDefaults({
      history: [historyEntry({ transcriptStatus: "partial" })],
      transcript: transcriptDocument([
        transcriptSegment({ id: 1 }),
        transcriptSegment({ id: 2, startMs: 65_000, text: "预算保持不变。" }),
        transcriptSegment({ id: 3, status: "failed", text: "" }),
      ]),
    });
    await renderApp(t);
    openTab(t, "history");
    await expandSession(t);

    fireEvent.click(generateButton());
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText(t("partialConfirmTitle"))).toBeTruthy();
    expect(await within(dialog).findByText(format(t("partialConfirmBody"), { complete: 2, failed: 1 }))).toBeTruthy();
    expect(callsOf("generate_session_analysis")).toHaveLength(0);

    fireEvent.click(within(dialog).getByRole("button", { name: t("partialConfirmAccept") }));
    await waitFor(() => expect(callsOf("generate_session_analysis")).toHaveLength(1));
    const request = lastCall("generate_session_analysis")?.args.request as GenerateSessionAnalysisRequest;
    expect(request).toMatchObject({ acceptPartial: true, mode: "ensure" });
  });

  it("reopens the confirmation when the backend asks for it", async () => {
    scriptAppDefaults({ history: [historyEntry()], transcript: transcriptDocument() });
    scriptFailure("generate_session_analysis", errorPayload({ code: "PARTIAL_CONFIRMATION_REQUIRED", messageKey: "meetingNotesErrorPartialConfirmationRequired", retryable: false }));
    await renderApp(t);
    openTab(t, "history");
    await expandSession(t);

    fireEvent.click(generateButton());
    expect(await screen.findByRole("alertdialog")).toBeTruthy();
    expect(screen.getByText(t("meetingNotesErrorPartialConfirmationRequired"))).toBeTruthy();
  });

  it("issues one command for a double click on generate", async () => {
    scriptAppDefaults({ history: [historyEntry()], transcript: transcriptDocument() });
    await renderApp(t);
    openTab(t, "history");
    await expandSession(t);

    const button = generateButton();
    fireEvent.click(button);
    fireEvent.click(button);

    await waitFor(() => expect(callsOf("generate_session_analysis")).toHaveLength(1));
    expect(callsOf("save_session_context")).toHaveLength(1);
  });
});

describe("out-of-order status events", () => {
  it("keeps the newest snapshot when an older event arrives", async () => {
    scriptAppDefaults({ history: [historyEntry()] });
    await renderApp(t);
    openTab(t, "history");

    await act(async () => { emit("meeting-notes-status", statusEvent({ documentRevision: 3, generation: 2, state: "complete", updatedAt: "2026-08-15T03:00:10.000Z" })); });
    expect(chipText()).toBe(`${t("meetingNotes")}: ${t("meetingNotesComplete")}`);

    await act(async () => { emit("meeting-notes-status", statusEvent({ documentRevision: 2, generation: 9, state: "cleaning", updatedAt: "2026-08-15T03:00:20.000Z" })); });
    expect(chipText()).toBe(`${t("meetingNotes")}: ${t("meetingNotesComplete")}`);

    await act(async () => { emit("meeting-notes-status", statusEvent({ documentRevision: 3, generation: 1, state: "failed", updatedAt: "2026-08-15T03:00:30.000Z" })); });
    expect(chipText()).toBe(`${t("meetingNotes")}: ${t("meetingNotesComplete")}`);

    await act(async () => { emit("meeting-notes-status", statusEvent({ documentRevision: 3, generation: 3, state: "failed", updatedAt: "2026-08-15T03:00:40.000Z" })); });
    expect(chipText()).toBe(`${t("meetingNotes")}: ${t("meetingNotesFailed")}`);
  });

  it("orders snapshots by document revision, generation and update time", () => {
    const base = analysisStatusFromEvent(statusEvent({ documentRevision: 3, generation: 2, updatedAt: "2026-08-15T03:00:10.000Z" }));
    expect(isNewerAnalysisStatus({ ...base, documentRevision: 4, generation: 1 }, base)).toBe(true);
    expect(isNewerAnalysisStatus({ ...base, documentRevision: 2, generation: 9 }, base)).toBe(false);
    expect(isNewerAnalysisStatus({ ...base, generation: 3 }, base)).toBe(true);
    expect(isNewerAnalysisStatus({ ...base, generation: 1 }, base)).toBe(false);
    expect(isNewerAnalysisStatus({ ...base, updatedAt: "2026-08-15T03:00:09.000Z" }, base)).toBe(false);
    expect(isNewerAnalysisStatus({ ...base }, base)).toBe(true);
  });

  it("derives the same ordering keys from the read view", () => {
    const view = completeAnalysisView({ currentRun: runView({ generation: 4, progress: { completedChunks: 2, totalChunks: 5 }, updatedAt: "2026-08-15T03:00:10.000Z" }), documentRevision: 7 });
    expect(analysisStatusFromView(view)).toEqual({
      completedChunks: 2,
      documentRevision: 7,
      generation: 4,
      state: "complete",
      totalChunks: 5,
      updatedAt: "2026-08-15T03:00:10.000Z",
    });
    expect(analysisStatusFromView(analysisView()).state).toBe("draft");
    expect(analysisStatusFromView(analysisView({ lastSuccessfulResult: analysisResult() })).state).toBe("complete");
  });
});

describe("history card actions", () => {
  it("stacks delete, open folder and the detail toggle in that order", async () => {
    scriptAppDefaults({ history: [historyEntry()], transcript: transcriptDocument() });
    await renderApp(t);
    openTab(t, "history");

    const buttons = [...document.querySelectorAll<HTMLButtonElement>(".history-actions button")];
    expect(buttons.map((button) => button.title)).toEqual([t("deleteRecording"), t("openFolder"), t("meetingNotes")]);
    expect(buttons.map((button) => button.getAttribute("aria-expanded"))).toEqual([null, null, "false"]);

    fireEvent.click(buttons[2]);

    await screen.findByRole("tablist");
    expect(document.querySelector(".history-actions button[aria-expanded]")?.getAttribute("aria-expanded")).toBe("true");
  });
});
