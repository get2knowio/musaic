import { useEffect } from "react";
import { useLocation, useNavigate } from "react-router-dom";

import { openActivityPanel, readActivityLink } from "@/lib/activityPanel";

/**
 * `?activity=open|jobs|history[&job=<id>]` on any route opens the Activity
 * panel over that page (at that section, the job marked) and is then
 * dropped from the address, so Back and a reload do not reopen it.
 * Mounted once, in App.
 */
export function useActivityLinks(): void {
  const { pathname, search, hash } = useLocation();
  const navigate = useNavigate();
  useEffect(() => {
    const link = readActivityLink(search);
    if (!link) return;
    openActivityPanel({ section: link.section, job: link.job });
    navigate(`${pathname}${link.rest ? `?${link.rest}` : ""}${hash}`, { replace: true });
  }, [pathname, search, hash, navigate]);
}
