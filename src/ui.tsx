import type { ReactNode } from "react";

export function Card({
  title,
  subtitle,
  children,
  accent,
}: {
  title: string;
  subtitle?: string;
  children: ReactNode;
  accent?: boolean;
}) {
  return (
    <section className={`card${accent ? " card-accent" : ""}`}>
      <header className="card-head">
        <h2>{title}</h2>
        {subtitle ? <p className="card-sub">{subtitle}</p> : null}
      </header>
      <div className="card-body">{children}</div>
    </section>
  );
}

export function Field({
  label,
  children,
  hint,
}: {
  label: string;
  children: ReactNode;
  hint?: string;
}) {
  return (
    <label className="field">
      <span className="field-label">{label}</span>
      {children}
      {hint ? <span className="field-hint">{hint}</span> : null}
    </label>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <label className={`toggle${disabled ? " toggle-disabled" : ""}`}>
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <span className="toggle-track" aria-hidden />
      <span className="toggle-label">{label}</span>
    </label>
  );
}

export function Pill({ tone, children }: { tone: string; children: ReactNode }) {
  return <span className={`pill pill-${tone}`}>{children}</span>;
}

export function Button({
  children,
  onClick,
  variant = "default",
  disabled,
}: {
  children: ReactNode;
  onClick: () => void;
  variant?: "default" | "primary" | "danger" | "ghost";
  disabled?: boolean;
}) {
  return (
    <button
      className={`btn btn-${variant}`}
      onClick={onClick}
      disabled={disabled}
    >
      {children}
    </button>
  );
}
