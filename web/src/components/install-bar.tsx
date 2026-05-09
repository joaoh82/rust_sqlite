"use client";

import { useEffect, useRef, useState } from "react";

type InstallBarProps = {
  /** Plain command, will be split on the first space; second half gets accent. */
  cmd: string;
  prompt?: string;
};

export function InstallBar({ cmd, prompt = "$" }: InstallBarProps) {
  const [copied, setCopied] = useState(false);
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const firstSpace = cmd.indexOf(" ");
  const lead = firstSpace === -1 ? cmd : cmd.slice(0, firstSpace);
  const tail = firstSpace === -1 ? "" : cmd.slice(firstSpace + 1);

  useEffect(() => {
    return () => {
      if (timeoutRef.current) clearTimeout(timeoutRef.current);
    };
  }, []);

  const copy = () => {
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      void navigator.clipboard.writeText(cmd);
    }
    setCopied(true);
    if (timeoutRef.current) clearTimeout(timeoutRef.current);
    timeoutRef.current = setTimeout(() => setCopied(false), 1400);
  };

  return (
    <div className="install">
      <span className="install-prompt">{prompt}</span>
      <span className="install-cmd">
        <span className="dim">{lead}</span>
        {tail ? (
          <>
            {" "}
            <span className="accent">{tail}</span>
          </>
        ) : null}
      </span>
      <button
        className="install-copy"
        onClick={copy}
        aria-label="Copy install command"
      >
        {copied ? "copied ✓" : "copy"}
      </button>
    </div>
  );
}
