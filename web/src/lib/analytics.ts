"use client";

import { useCallback } from "react";
import { usePostHog } from "@posthog/next";

// Same gate as the provider in layout.tsx: when the key is absent the
// provider isn't mounted, so capture calls must no-op instead of warning.
const ENABLED = Boolean(process.env.NEXT_PUBLIC_POSTHOG_KEY);

/** Returns a stable `track(event, properties)` that no-ops when PostHog
 *  isn't configured. Event names are kebab-case per the SQLR-36 spec
 *  (`docs-search-query`, `docs-helpful-vote`). */
export function useTrack() {
  const posthog = usePostHog();
  return useCallback(
    (event: string, properties?: Record<string, unknown>) => {
      if (!ENABLED) return;
      posthog?.capture(event, properties);
    },
    [posthog],
  );
}
