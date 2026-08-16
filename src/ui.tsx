export type IconName =
  | "archive"
  | "book"
  | "chat"
  | "check"
  | "copy"
  | "download"
  | "eye"
  | "eyeOff"
  | "folder"
  | "gear"
  | "history"
  | "mic"
  | "refresh"
  | "shield"
  | "text"
  | "trash"
  | "wave";

export const isTauri = "__TAURI_INTERNALS__" in window;

export function Icon({ name }: { name: IconName }) {
  const paths: Record<IconName, preact.JSX.Element> = {
    archive: <><path d="M4 7h16v13H4z"/><path d="M3 3h18v4H3zm6 8h6"/></>,
    book: <><path d="M4 4h11a3 3 0 0 1 3 3v13H7a3 3 0 0 0-3 3z"/><path d="M7 8h8m-8 4h6"/></>,
    chat: <><path d="M4 5h16v11H9l-5 4z"/><path d="M8 9h8m-8 3h5"/></>,
    check: <path d="m5 12 4 4L19 6"/>,
    copy: <><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15H4V4h11v1"/></>,
    download: <><path d="M12 3v12m-5-5 5 5 5-5"/><path d="M5 20h14"/></>,
    eye: <><path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6-10-6-10-6z"/><circle cx="12" cy="12" r="3"/></>,
    eyeOff: <><path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6-10-6-10-6z"/><circle cx="12" cy="12" r="3"/><path d="m4 20 16-16"/></>,
    folder: <path d="M3 6h7l2 2h9v11H3z"/>,
    gear: <><circle cx="12" cy="12" r="3"/><path d="M19 13.5v-3l-2-.7-.7-1.7.9-1.9-2.1-2.1-1.9.9-1.7-.7L10.5 2h-3l-.7 2-1.7.7-1.9-.9-2.1 2.1.9 1.9-.7 1.7-2 .7v3l2 .7.7 1.7-.9 1.9 2.1 2.1 1.9-.9 1.7.7.7 2h3l.7-2 1.7-.7 1.9.9 2.1-2.1-.9-1.9.7-1.7z"/></>,
    history: <><path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5m4-2v6l4 2"/></>,
    mic: <><rect x="8" y="3" width="8" height="12" rx="4"/><path d="M5 11a7 7 0 0 0 14 0m-7 7v3m-4 0h8"/></>,
    refresh: <><path d="M20 7v5h-5"/><path d="M19 12a7 7 0 1 0-2 5"/></>,
    shield: <path d="M12 2 4 5v6c0 5 3.4 8.6 8 11 4.6-2.4 8-6 8-11V5z"/>,
    text: <path d="M4 6h16M4 11h16M4 16h10"/>,
    trash: <><path d="M4 7h16m-10 4v6m4-6v6M9 7l1-3h4l1 3m3 0-1 14H7L6 7"/></>,
    wave: <path d="M3 12h2l2-7 3 14 3-11 2 8 2-4h4"/>,
  };
  return <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">{paths[name]}</svg>;
}

export function SettingsSection({ icon, title, action, children }: { icon: IconName; title: string; action?: preact.ComponentChildren; children: preact.ComponentChildren }) { return <section class="panel-card settings-section"><div class={`section-heading ${action ? "has-action" : ""}`}><div><span class="section-icon"><Icon name={icon} /></span><h2>{title}</h2></div>{action}</div>{children}</section>; }
export function SelectField({ label, value, options, disabled = false, onChange }: { label: string; value: string; options: { value: string; label: string }[]; disabled?: boolean; onChange: (value: string) => void }) { return <label class="field"><span>{label}</span><select value={value} disabled={disabled} onChange={(event) => onChange(event.currentTarget.value)}>{options.map((option) => <option value={option.value} key={option.value}>{option.label}</option>)}</select></label>; }
export function TextField({ label, value, onChange, type = "text", disabled = false, className = "", placeholder = "" }: { label: string; value: string; onChange: (value: string) => void; type?: "text" | "url"; disabled?: boolean; className?: string; placeholder?: string }) { return <label class={`field ${className}`}><span>{label}</span><input disabled={disabled} placeholder={placeholder} type={type} value={value} onInput={(event) => onChange(event.currentTarget.value)} /></label>; }
export function NumberField({ label, value, min, max, onChange }: { label: string; value: number; min: number; max: number; onChange: (value: number) => void }) { return <label class="field"><span>{label}</span><input type="number" value={value} min={min} max={max} onChange={(event) => onChange(Number(event.currentTarget.value))} /></label>; }
export function RangeField({ label, value, min, max, step, suffix, onChange }: { label: string; value: number; min: number; max: number; step: number; suffix: string; onChange: (value: number) => void }) { return <label class="range-field"><span><b>{label}</b><em>{value.toFixed(step < 0.1 ? 2 : 1)}{suffix}</em></span><input type="range" value={value} min={min} max={max} step={step} onInput={(event) => onChange(Number(event.currentTarget.value))} /></label>; }
export function Toggle({ label, checked, onChange }: { label: string; checked: boolean; onChange: (checked: boolean) => void }) { return <label class="toggle-row"><span>{label}</span><input type="checkbox" checked={checked} onChange={(event) => onChange(event.currentTarget.checked)} /><i /></label>; }

export function formatDuration(milliseconds: number) { const total = Math.floor(milliseconds / 1000); const hours = Math.floor(total / 3600); const minutes = Math.floor(total % 3600 / 60); const seconds = total % 60; return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`; }
export function formatClock(milliseconds: number) { const total = Math.max(0, Math.floor(milliseconds / 1000)); return `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`; }
export function formatBytes(bytes: number) { if (bytes <= 0) return "0 B"; const units = ["B", "KB", "MB", "GB"]; const index = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024))); return `${(bytes / 1024 ** index).toFixed(index > 1 ? 1 : 0)} ${units[index]}`; }
