import { vi } from "vitest";

/** Stand-in for `@tauri-apps/api/core` and `@tauri-apps/api/event`: every command
 *  must be scripted by the test, and every call is recorded for assertions. */
export type InvokeArgs = Record<string, unknown>;
export type Responder = (args: InvokeArgs) => unknown;
export interface InvokeCall { args: InvokeArgs; command: string }

const responders = new Map<string, Responder>();
const listeners = new Map<string, Set<(event: { payload: unknown }) => void>>();
export const invokeCalls: InvokeCall[] = [];

export const invoke = vi.fn(async (command: string, args: InvokeArgs = {}) => {
  invokeCalls.push({ args, command });
  const responder = responders.get(command);
  if (!responder) throw new Error(`unscripted command: ${command}`);
  return responder(args);
});

export const listen = vi.fn(async (event: string, handler: (event: { payload: unknown }) => void) => {
  const handlers = listeners.get(event) ?? new Set<(event: { payload: unknown }) => void>();
  handlers.add(handler);
  listeners.set(event, handlers);
  return () => { handlers.delete(handler); };
});

export function script(command: string, responder: Responder) { responders.set(command, responder); }

/** Scripts a command that rejects the way a Tauri command error reaches the UI. */
export function scriptFailure(command: string, reason: unknown) {
  responders.set(command, () => { throw reason; });
}

export function callsOf(command: string) { return invokeCalls.filter((call) => call.command === command); }
export function lastCall(command: string) { return callsOf(command).at(-1); }
export function emit(event: string, payload: unknown) { listeners.get(event)?.forEach((handler) => handler({ payload })); }
export function listenerCount(event: string) { return listeners.get(event)?.size ?? 0; }

export function resetTauri() {
  responders.clear();
  listeners.clear();
  invokeCalls.length = 0;
  invoke.mockClear();
  listen.mockClear();
}
