import { fireEvent, screen, waitFor, within } from "@testing-library/preact";
import { describe, expect, it } from "vitest";
import { translator } from "../i18n";
import { mergeContextDraft } from "../meeting_notes_ui";
import { analysisView, errorPayload, globalContent, globalContextDocument, historyEntry, meetingContent, scriptAppDefaults } from "../test/fixtures";
import { contextField, expandSession, openTab, renderApp, selectDetailTab, typeInto } from "../test/render";
import { callsOf, lastCall, script, scriptFailure } from "../test/tauri";
import type { GlobalContextContent } from "../types";

const t = translator("en-US");

function openPanel() {
  fireEvent.click(screen.getByRole("button", { name: t("extractFromText") }));
}

function pasteReferenceText(text: string) {
  openPanel();
  typeInto(screen.getByLabelText(t("extractContextTitle")), text);
}

function extractButton() {
  return screen.getByRole("button", { name: t("extractContextRun") }) as HTMLButtonElement;
}

function runExtraction() {
  fireEvent.click(extractButton());
}

function addFile(name: string, text: string) {
  fireEvent.change(screen.getByLabelText(t("chooseTextFiles")), { target: { files: [new File([text], name, { type: "text/plain" })] } });
}

function rowInputs(field: HTMLElement) {
  return (within(field).getAllByRole("textbox") as HTMLInputElement[]).map((input) => input.value);
}

function fieldValue(label: string) {
  return (screen.getByLabelText(label) as HTMLInputElement).value;
}

describe("mergeContextDraft", () => {
  it("fills empty scalars, keeps written ones and appends the free text", () => {
    const merged = mergeContextDraft(
      globalContent({ freeText: "现有补充", knowledgeBackground: "已有的背景" }),
      globalContent({ freeText: "草稿补充", knowledgeBackground: "模型写的背景", timezone: "Asia/Shanghai" }),
    );

    expect(merged.knowledgeBackground).toBe("已有的背景");
    expect(merged.timezone).toBe("Asia/Shanghai");
    expect(merged.freeText).toBe("现有补充\n\n草稿补充");
  });

  it("keeps a blank free text from growing a leading blank line", () => {
    expect(mergeContextDraft(globalContent(), globalContent({ freeText: "草稿补充" })).freeText).toBe("草稿补充");
    expect(mergeContextDraft(globalContent({ freeText: "现有补充" }), globalContent()).freeText).toBe("现有补充");
  });

  it("merges nested identity fields under the same rules", () => {
    const merged = mergeContextDraft(
      globalContent({ identity: { aliases: ["小张"], canonicalName: "张三", commonAsrErrors: [] } }),
      globalContent({ identity: { aliases: ["ZHANG SAN", " 小张 "], canonicalName: "Alex Chen", commonAsrErrors: ["章三"] } }),
    );

    expect(merged.identity.canonicalName).toBe("张三");
    expect(merged.identity.aliases).toEqual(["小张", "ZHANG SAN"]);
    expect(merged.identity.commonAsrErrors).toEqual(["章三"]);
  });

  it("appends list entries and skips names that already exist, ignoring case and padding", () => {
    const merged = mergeContextDraft(
      globalContent({
        glossary: [{ aliases: [], commonAsrErrors: [], id: "term-1", meaning: "自动语音识别", term: "ASR" }],
        recurringPeople: [{ aliases: [], canonicalName: "张三", commonAsrErrors: [], description: "", id: "person-1" }],
        rolesAndAffiliations: ["Product lead"],
      }),
      globalContent({
        glossary: [{ aliases: [], commonAsrErrors: [], id: "", meaning: "别的解释", term: " asr " }, { aliases: [], commonAsrErrors: [], id: "", meaning: "语音活动检测", term: "VAD" }],
        recurringPeople: [{ aliases: [], canonicalName: " 张三 ", commonAsrErrors: [], description: "重复", id: "" }, { aliases: [], canonicalName: "Alex Chen", commonAsrErrors: [], description: "移动端同事", id: "" }],
        rolesAndAffiliations: ["product lead", "Advisor"],
      }),
    );

    expect(merged.rolesAndAffiliations).toEqual(["Product lead", "Advisor"]);
    expect(merged.glossary.map((entry) => entry.term)).toEqual(["ASR", "VAD"]);
    expect(merged.glossary[0].meaning).toBe("自动语音识别");
    expect(merged.recurringPeople.map((entry) => entry.canonicalName)).toEqual(["张三", "Alex Chen"]);
  });

  it("merges the meeting template the same way", () => {
    const merged = mergeContextDraft(
      meetingContent({ participants: [{ aliases: [], canonicalName: "张三", commonAsrErrors: [], description: "", id: "p-1", role: "主持人", speakerLabel: null }], title: "当前会议标题" }),
      meetingContent({ agenda: ["回顾上季度结论"], date: "2026-08-15", participants: [{ aliases: [], canonicalName: "Alex Chen", commonAsrErrors: [], description: "", id: "", role: "项目经理", speakerLabel: null }], title: "Q3 路线图评审" }),
    );

    expect(merged.title).toBe("当前会议标题");
    expect(merged.date).toBe("2026-08-15");
    expect(merged.agenda).toEqual(["回顾上季度结论"]);
    expect(merged.participants.map((entry) => entry.canonicalName)).toEqual(["张三", "Alex Chen"]);
  });
});

describe("global context extraction", () => {
  it("sends the pasted text to the global command and merges the draft without saving", async () => {
    scriptAppDefaults({ globalContext: globalContextDocument({ content: globalContent({ freeText: "现有补充", knowledgeBackground: "已有的背景", rolesAndAffiliations: ["Product lead"] }) }) });
    script("extract_global_context_draft", () => globalContent({
      freeText: "草稿补充",
      identity: { aliases: [], canonicalName: "张三", commonAsrErrors: [] },
      knowledgeBackground: "模型写的背景",
      rolesAndAffiliations: ["product lead", "Advisor"],
      timezone: "Asia/Shanghai",
    }));
    await renderApp(t);
    openTab(t, "settings");

    pasteReferenceText("张三是 Resonote 的产品负责人。");
    runExtraction();

    await waitFor(() => expect(callsOf("extract_global_context_draft")).toHaveLength(1));
    expect(lastCall("extract_global_context_draft")?.args.request).toEqual({ outputLanguage: "en-US", text: "张三是 Resonote 的产品负责人。" });
    expect(await screen.findByText(t("contextDraftApplied"))).toBeTruthy();
    expect(fieldValue(t("contextCanonicalName"))).toBe("张三");
    expect(fieldValue(t("contextTimezone"))).toBe("Asia/Shanghai");
    expect(fieldValue(t("contextKnowledge"))).toBe("已有的背景");
    expect(fieldValue(t("contextFreeText"))).toBe("现有补充\n\n草稿补充");
    expect(rowInputs(contextField(t("contextRoles")))).toEqual(["Product lead", "Advisor"]);
    expect(callsOf("save_global_context")).toHaveLength(0);
    expect(screen.queryByLabelText(t("extractContextTitle"))).toBeNull();
  });

  it("lists a picked file with its character count and joins it after the pasted text", async () => {
    scriptAppDefaults();
    script("extract_global_context_draft", () => globalContent());
    await renderApp(t);
    openTab(t, "settings");

    pasteReferenceText("会议邀请正文");
    addFile("brief.md", "# 项目简介");

    expect(await screen.findByText("brief.md")).toBeTruthy();
    expect(screen.getByText("6 characters")).toBeTruthy();
    runExtraction();

    await waitFor(() => expect(callsOf("extract_global_context_draft")).toHaveLength(1));
    expect(lastCall("extract_global_context_draft")?.args.request).toEqual({ outputLanguage: "en-US", text: "会议邀请正文\n\n--- brief.md ---\n\n# 项目简介" });
  });

  it("refuses a file over the size limit and keeps the other sources", async () => {
    scriptAppDefaults();
    script("extract_global_context_draft", () => globalContent());
    await renderApp(t);
    openTab(t, "settings");

    pasteReferenceText("会议邀请正文");
    fireEvent.change(screen.getByLabelText(t("chooseTextFiles")), { target: { files: [new File(["x".repeat(2 * 1024 * 1024 + 1)], "huge.txt", { type: "text/plain" })] } });

    expect(await screen.findByText("File too large (2 MB max): huge.txt")).toBeTruthy();
    expect(screen.queryByText("huge.txt")).toBeNull();
    runExtraction();

    await waitFor(() => expect(callsOf("extract_global_context_draft")).toHaveLength(1));
    expect(lastCall("extract_global_context_draft")?.args.request).toMatchObject({ text: "会议邀请正文" });
  });

  it("shows a localized failure, keeps the typed text and offers the extraction again", async () => {
    scriptAppDefaults();
    scriptFailure("extract_global_context_draft", errorPayload({ code: "CONTEXT_DRAFT_INVALID", messageKey: "meetingNotesErrorContextDraftInvalid", retryable: true }));
    await renderApp(t);
    openTab(t, "settings");

    pasteReferenceText("参考文本");
    runExtraction();

    expect(await screen.findByText(t("meetingNotesErrorContextDraftInvalid"))).toBeTruthy();
    expect((screen.getByLabelText(t("extractContextTitle")) as HTMLTextAreaElement).value).toBe("参考文本");
    expect(screen.queryByText(t("contextDraftApplied"))).toBeNull();
    expect(extractButton().disabled).toBe(false);
  });

  it("disables the extract button and reports progress while a draft is in flight", async () => {
    scriptAppDefaults();
    script("extract_global_context_draft", () => new Promise(() => {}));
    await renderApp(t);
    openTab(t, "settings");

    openPanel();
    expect(extractButton().disabled).toBe(true);

    typeInto(screen.getByLabelText(t("extractContextTitle")), "参考文本");
    expect(extractButton().disabled).toBe(false);

    runExtraction();
    expect(await screen.findByText(t("extracting"))).toBeTruthy();
    expect(extractButton().disabled).toBe(true);
  });
});

describe("meeting context extraction", () => {
  it("sends the pasted text to the meeting command and merges the draft into the editor", async () => {
    scriptAppDefaults({ analysis: analysisView({ meetingContext: { content: meetingContent({ title: "当前会议标题" }), revision: 2, updatedAt: "2026-08-15T02:00:00.000Z" } }), history: [historyEntry()] });
    script("extract_meeting_context_draft", () => meetingContent({ date: "2026-08-15", purpose: "确认下季度优先级", title: "Q3 路线图评审" }));
    await renderApp(t);
    openTab(t, "history");
    await expandSession(t);
    selectDetailTab(t, "tabContext");

    pasteReferenceText("会议邀请：Q3 路线图评审，2026-08-15");
    runExtraction();

    await waitFor(() => expect(callsOf("extract_meeting_context_draft")).toHaveLength(1));
    expect(lastCall("extract_meeting_context_draft")?.args.request).toEqual({ outputLanguage: "en-US", text: "会议邀请：Q3 路线图评审，2026-08-15" });
    expect(await screen.findByText(t("contextDraftApplied"))).toBeTruthy();
    expect(fieldValue(t("contextTitle"))).toBe("当前会议标题");
    expect(fieldValue(t("contextDate"))).toBe("2026-08-15");
    expect(fieldValue(t("contextPurpose"))).toBe("确认下季度优先级");
    expect(callsOf("save_session_context")).toHaveLength(0);
  });

  it("offers the settings jump when the chat model is not configured", async () => {
    scriptAppDefaults({ history: [historyEntry()] });
    scriptFailure("extract_meeting_context_draft", errorPayload({ code: "MEETING_NOTES_NOT_CONFIGURED", messageKey: "meetingNotesErrorMeetingNotesNotConfigured", retryable: false }));
    await renderApp(t);
    openTab(t, "history");
    await expandSession(t);
    selectDetailTab(t, "tabContext");

    pasteReferenceText("参考文本");
    runExtraction();

    expect(await screen.findByText(t("meetingNotesErrorMeetingNotesNotConfigured"))).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: t("openMeetingNotesSettings") }));
    expect(screen.getByRole("heading", { level: 2, name: t("globalContext") })).toBeTruthy();
  });
});

describe("global context draft typing", () => {
  it("never overwrites a scalar the user is still editing", async () => {
    scriptAppDefaults({ globalContext: globalContextDocument({ content: globalContent({ timezone: "Europe/Berlin" }) }) });
    script("extract_global_context_draft", () => globalContent({ timezone: "Asia/Shanghai" }) satisfies GlobalContextContent);
    await renderApp(t);
    openTab(t, "settings");

    pasteReferenceText("参考文本");
    runExtraction();

    await waitFor(() => expect(callsOf("extract_global_context_draft")).toHaveLength(1));
    await screen.findByText(t("contextDraftApplied"));
    expect(fieldValue(t("contextTimezone"))).toBe("Europe/Berlin");
  });
});
