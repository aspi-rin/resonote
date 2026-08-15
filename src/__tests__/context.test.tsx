import { fireEvent, screen, waitFor, within } from "@testing-library/preact";
import { describe, expect, it } from "vitest";
import { translator } from "../i18n";
import { CONTEXT_CHARACTER_LIMIT } from "../meeting_notes_ui";
import {
  SESSION_ID,
  analysisView,
  errorPayload,
  globalContent,
  globalContextDocument,
  historyEntry,
  meetingContent,
  scriptAppDefaults,
} from "../test/fixtures";
import { contextField, expandSession, openTab, renderApp, selectDetailTab, settingsSection, typeInto } from "../test/render";
import { callsOf, lastCall, script, scriptFailure } from "../test/tauri";
import type { GlobalContextContent, MeetingContextContent } from "../types";

const t = translator("en-US");
const ADD = `+ ${t("listAdd")}`;

function rowInputs(field: HTMLElement) {
  return (within(field).getAllByRole("textbox") as HTMLInputElement[]).map((input) => input.value);
}

function characterCount(scope: HTMLElement) {
  const text = scope.querySelector(".context-count")?.textContent ?? "";
  const match = text.match(/(\d+)\s*\/\s*(\d+)/);
  if (!match) throw new Error(`no character count in: ${text}`);
  return { characters: Number(match[1]), limit: Number(match[2]) };
}

describe("global context editor", () => {
  it("adds, edits, reorders and removes list entries", async () => {
    scriptAppDefaults({ globalContext: globalContextDocument({ content: globalContent({ rolesAndAffiliations: ["Product lead"] }) }) });
    await renderApp(t);
    openTab(t, "settings");
    const roles = () => contextField(t("contextRoles"));

    fireEvent.click(within(roles()).getByRole("button", { name: ADD }));
    typeInto(within(roles()).getAllByRole("textbox")[1], "Advisor");
    expect(rowInputs(roles())).toEqual(["Product lead", "Advisor"]);

    fireEvent.click(within(within(roles()).getAllByRole("listitem")[0]).getByTitle(t("listMoveDown")));
    expect(rowInputs(roles())).toEqual(["Advisor", "Product lead"]);

    fireEvent.click(within(roles()).getByRole("button", { name: ADD }));
    expect(rowInputs(roles())).toHaveLength(3);
    fireEvent.click(within(within(roles()).getAllByRole("listitem")[2]).getByTitle(t("listRemove")));
    expect(rowInputs(roles())).toEqual(["Advisor", "Product lead"]);
  });

  it("saves with the expected revision, keeping existing ids and leaving new ones empty", async () => {
    scriptAppDefaults({
      globalContext: globalContextDocument({
        content: globalContent({
          recurringPeople: [{ aliases: ["小刘"], canonicalName: "刘业新", commonAsrErrors: [], description: "", id: "person-1" }],
          rolesAndAffiliations: ["Product lead"],
        }),
        revision: 4,
      }),
    });
    await renderApp(t);
    openTab(t, "settings");

    const people = () => contextField(t("contextRecurringPeople"));
    fireEvent.click(within(people()).getByRole("button", { name: ADD }));
    const added = within(people()).getAllByRole("listitem")[1];
    typeInto(within(added).getByLabelText(t("contextCanonicalName")), "Alex Chen");
    typeInto(within(added).getByLabelText(t("contextAliases")), "Alex, AC");
    fireEvent.click(screen.getByRole("button", { name: t("saveGlobalContext") }));

    await waitFor(() => expect(callsOf("save_global_context")).toHaveLength(1));
    const call = lastCall("save_global_context");
    expect(call?.args.expectedGlobalContextRevision).toBe(4);
    const content = call?.args.content as GlobalContextContent;
    expect(content.recurringPeople.map((entry) => entry.id)).toEqual(["person-1", ""]);
    expect(content.recurringPeople[0].aliases).toEqual(["小刘"]);
    expect(content.recurringPeople[1]).toMatchObject({ aliases: ["Alex", "AC"], canonicalName: "Alex Chen" });
    expect(content.rolesAndAffiliations).toEqual(["Product lead"]);
  });

  it("reorders and removes entity rows without touching the remaining ids", async () => {
    scriptAppDefaults({
      globalContext: globalContextDocument({
        content: globalContent({
          recurringPeople: [
            { aliases: [], canonicalName: "刘业新", commonAsrErrors: [], description: "", id: "person-1" },
            { aliases: [], canonicalName: "Alex Chen", commonAsrErrors: [], description: "", id: "person-2" },
          ],
        }),
        revision: 2,
      }),
    });
    await renderApp(t);
    openTab(t, "settings");
    const people = () => contextField(t("contextRecurringPeople"));
    const names = () => (within(people()).getAllByLabelText(t("contextCanonicalName")) as HTMLInputElement[]).map((input) => input.value);

    fireEvent.click(within(within(people()).getAllByRole("listitem")[0]).getByTitle(t("listMoveDown")));
    expect(names()).toEqual(["Alex Chen", "刘业新"]);

    fireEvent.click(within(within(people()).getAllByRole("listitem")[0]).getByTitle(t("listRemove")));
    expect(names()).toEqual(["刘业新"]);

    fireEvent.click(screen.getByRole("button", { name: t("saveGlobalContext") }));
    await waitFor(() => expect(callsOf("save_global_context")).toHaveLength(1));
    expect((lastCall("save_global_context")?.args.content as GlobalContextContent).recurringPeople).toEqual([
      { aliases: [], canonicalName: "刘业新", commonAsrErrors: [], description: "", id: "person-1" },
    ]);
  });

  it("reports a revision conflict and reloads the document", async () => {
    const stored = globalContextDocument({ content: globalContent({ rolesAndAffiliations: ["Product lead"] }), revision: 4 });
    const reloaded = globalContextDocument({ content: globalContent({ rolesAndAffiliations: ["Reloaded role"] }), revision: 9 });
    scriptAppDefaults({ globalContext: stored });
    let reads = 0;
    script("get_global_context", () => (reads++ === 0 ? stored : reloaded));
    scriptFailure("save_global_context", errorPayload({ code: "CONTEXT_REVISION_CONFLICT", messageKey: "meetingNotesErrorContextRevisionConflict", retryable: false }));

    await renderApp(t);
    openTab(t, "settings");
    fireEvent.click(screen.getByRole("button", { name: t("saveGlobalContext") }));

    expect(await screen.findByText(t("contextConflictReloaded"))).toBeTruthy();
    expect(callsOf("get_global_context")).toHaveLength(2);
    expect(rowInputs(contextField(t("contextRoles")))).toEqual(["Reloaded role"]);
  });

  it("renders the character count against the limit and updates it while typing", async () => {
    scriptAppDefaults({ globalContext: globalContextDocument({ content: globalContent({ knowledgeBackground: "x".repeat(5_000) }) }) });
    await renderApp(t);
    openTab(t, "settings");

    const before = characterCount(settingsSection(t("globalContext")));
    expect(before.limit).toBe(CONTEXT_CHARACTER_LIMIT);
    expect(before.characters).toBeGreaterThan(5_000);

    typeInto(screen.getByLabelText(t("contextFreeText")), "y".repeat(300));
    expect(characterCount(settingsSection(t("globalContext"))).characters).toBeGreaterThan(before.characters + 290);
  });
});

describe("meeting context editor", () => {
  it("saves the meeting background with the expected revision and reloads the view", async () => {
    scriptAppDefaults({
      analysis: analysisView({ meetingContext: { content: meetingContent({ title: "当前会议标题" }), revision: 2, updatedAt: "2026-08-15T02:00:00.000Z" } }),
      history: [historyEntry()],
    });
    await renderApp(t);
    openTab(t, "history");
    await expandSession(t);
    selectDetailTab(t, "tabContext");

    const title = screen.getByLabelText(t("contextTitle")) as HTMLInputElement;
    expect(title.value).toBe("当前会议标题");
    const block = screen.getAllByText(t("meetingContext"))[0].closest(".context-block") as HTMLElement;
    expect(characterCount(block).limit).toBe(CONTEXT_CHARACTER_LIMIT);

    typeInto(title, "Q3 roadmap review");
    fireEvent.click(screen.getByRole("button", { name: t("saveMeetingContext") }));

    await waitFor(() => expect(callsOf("save_session_context")).toHaveLength(1));
    expect(lastCall("save_session_context")?.args).toMatchObject({ expectedMeetingContextRevision: 2, sessionId: SESSION_ID });
    expect((lastCall("save_session_context")?.args.content as MeetingContextContent).title).toBe("Q3 roadmap review");
    await waitFor(() => expect(callsOf("get_session_analysis")).toHaveLength(2));
  });

  it("shows a localized conflict error and reloads the analysis", async () => {
    scriptAppDefaults({ history: [historyEntry()] });
    scriptFailure("save_session_context", JSON.stringify(errorPayload({ code: "CONTEXT_REVISION_CONFLICT", messageKey: "meetingNotesErrorContextRevisionConflict", retryable: false })));
    await renderApp(t);
    openTab(t, "history");
    await expandSession(t);
    selectDetailTab(t, "tabContext");

    fireEvent.click(screen.getByRole("button", { name: t("saveMeetingContext") }));

    expect(await screen.findByText(t("meetingNotesErrorContextRevisionConflict"))).toBeTruthy();
    await waitFor(() => expect(callsOf("get_session_analysis")).toHaveLength(2));
  });
});
