import { useEffect, useState } from "react";
import { Icon } from "./Icon";

/** Copies `text` (or the result of `getText`) and confirms for a moment. */
export function CopyButton({
  text,
  getText,
  label = "Copy",
  className = "btn btn-ghost btn-sm",
}: {
  text?: string;
  getText?: () => Promise<string>;
  label?: string;
  className?: string;
}) {
  const [state, setState] = useState<"idle" | "copied" | "error">("idle");

  useEffect(() => {
    if (state === "idle") return;
    const timer = window.setTimeout(() => setState("idle"), 1600);
    return () => window.clearTimeout(timer);
  }, [state]);

  const copy = async () => {
    try {
      const value = getText ? await getText() : (text ?? "");
      await navigator.clipboard.writeText(value);
      setState("copied");
    } catch {
      setState("error");
    }
  };

  return (
    <button type="button" className={className} onClick={() => void copy()}>
      <Icon name={state === "copied" ? "check" : "copy"} size={15} />
      <span aria-live="polite">
        {state === "copied"
          ? "Copied"
          : state === "error"
            ? "Couldn't copy"
            : label}
      </span>
    </button>
  );
}
