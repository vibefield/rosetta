// ②'s head with a host's rename (H4): the project's name is the control.
// Pressed, it becomes a field holding the name; Enter commits, Escape — or
// leaving the field — cancels, and a host that refuses says why right there,
// in its own words, with the field still open. Until the host's catalog
// brings the new name, the head names the project as the host last said.
import { useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { RoomRename } from "../host-inputs";

export interface RenameHeadingProps {
  readonly name: string;
  readonly tint: string | null;
  readonly count: number | null;
  readonly countTitle?: string | undefined;
  readonly rename: RoomRename;
}

type Edit =
  | { readonly kind: "idle" }
  | { readonly kind: "editing"; readonly draft: string; readonly failure: string | null }
  | { readonly kind: "saving"; readonly draft: string };

function failureWords(cause: unknown): string {
  const words = cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "";
  return words === "" ? "Could not rename" : `Could not rename — ${words}`;
}

export function RenameHeading({ name, tint, count, countTitle, rename }: RenameHeadingProps) {
  const [edit, setEdit] = useState<Edit>({ kind: "idle" });
  const fieldRef = useRef<HTMLInputElement | null>(null);
  const nameRef = useRef<HTMLButtonElement | null>(null);
  const label = rename.label ?? "Rename";
  const alive = useRef(true);
  useEffect(() => () => {
    alive.current = false;
  }, []);
  const editing = edit.kind !== "idle";
  useEffect(() => {
    if (edit.kind !== "editing") return;
    fieldRef.current?.focus();
  }, [edit.kind]);

  const close = (refocus: boolean) => {
    setEdit({ kind: "idle" });
    if (refocus) queueMicrotask(() => nameRef.current?.focus());
  };
  const commit = (draft: string) => {
    const next = draft.trim();
    if (next === "" || next === name) {
      close(true);
      return;
    }
    setEdit({ kind: "saving", draft });
    let answer: void | PromiseLike<void>;
    try {
      answer = rename.onRename(next);
    } catch (cause) {
      setEdit({ kind: "editing", draft, failure: failureWords(cause) });
      return;
    }
    Promise.resolve(answer).then(
      () => {
        if (alive.current) close(true);
      },
      (cause: unknown) => {
        if (alive.current) setEdit({ kind: "editing", draft, failure: failureWords(cause) });
      },
    );
  };

  return (
    <div className="ah-colhead" data-rename={editing ? edit.kind : ""}>
      <i style={tint !== null ? ({ "--tint": tint } as CSSProperties) : undefined} />
      {!editing ? (
        <button
          ref={nameRef}
          type="button"
          className="ah-rename"
          title={`${label} · ${name}`}
          aria-label={`${label}: ${name}`}
          onClick={() => setEdit({ kind: "editing", draft: name, failure: null })}
        >
          {name}
        </button>
      ) : (
        <input
          ref={fieldRef}
          className="ah-rename__field"
          value={edit.draft}
          aria-label={label}
          aria-invalid={edit.kind === "editing" && edit.failure !== null ? true : undefined}
          aria-busy={edit.kind === "saving" ? true : undefined}
          readOnly={edit.kind === "saving"}
          onFocus={(event) => event.currentTarget.select()}
          onChange={(event) => {
            const draft = event.currentTarget.value;
            setEdit((current) => (current.kind === "editing" ? { ...current, draft, failure: null } : current));
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              event.stopPropagation();
              if (edit.kind === "editing") commit(edit.draft);
            } else if (event.key === "Escape") {
              event.preventDefault();
              event.stopPropagation();
              if (edit.kind === "editing") close(true);
            }
          }}
          onBlur={() => {
            if (edit.kind === "editing") close(false);
          }}
        />
      )}
      {edit.kind === "editing" && edit.failure !== null ? (
        <span className="ah-rename__failure" role="alert">{edit.failure}</span>
      ) : (
        count !== null && (
          <span className="ah-bubble" title={countTitle}>
            {count}
          </span>
        )
      )}
    </div>
  );
}
