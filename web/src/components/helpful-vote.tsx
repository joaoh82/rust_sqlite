"use client";

import { useState } from "react";
import { usePathname } from "next/navigation";
import { useTrack } from "@/lib/analytics";

// "Was this page helpful?" thumbs widget. One PostHog event per vote
// (`docs-helpful-vote`, properties: { path, helpful }) — no PII, no
// storage; weekly review happens in PostHog itself.
export function HelpfulVote() {
  const [voted, setVoted] = useState<null | boolean>(null);
  const pathname = usePathname();
  const track = useTrack();

  function vote(helpful: boolean) {
    if (voted !== null) return;
    setVoted(helpful);
    track("docs-helpful-vote", { path: pathname, helpful });
  }

  return (
    <div className="helpful-vote" aria-live="polite">
      {voted === null ? (
        <>
          <span className="helpful-vote-label">Was this page helpful?</span>
          <button type="button" className="btn" onClick={() => vote(true)}>
            👍 Yes
          </button>
          <button type="button" className="btn" onClick={() => vote(false)}>
            👎 No
          </button>
        </>
      ) : (
        <span className="helpful-vote-label">
          {voted
            ? "Thanks! Glad it helped."
            : "Thanks — we'll use that to improve this page."}
        </span>
      )}
    </div>
  );
}
