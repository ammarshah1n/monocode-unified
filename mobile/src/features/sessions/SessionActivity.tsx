import { useEffect, useMemo } from "react";

import { useConnection } from "@/features/connection/ConnectionProvider";

import { resetSessionActivity, trackSessions } from "./activityStore";

/**
 * Follows the desktop's session list as the phone polls it, wherever the
 * app is, so a session that finishes while another project is open is still
 * on its deck when that project comes back. Mounted once, per pairing.
 */
export function SessionActivity() {
  const { connected, hosts, hostCatalogs } = useConnection();
  const sessions = useMemo(() => hosts.flatMap((host) =>
    (hostCatalogs[host.fingerprint]?.sessions ?? []).map((session) => ({
      ...session,
      hostFingerprint: host.fingerprint,
      hostName: host.name,
    })),
  ), [hosts, hostCatalogs]);
  useEffect(() => {
    if (connected) trackSessions(sessions);
  }, [connected, sessions]);
  useEffect(() => resetSessionActivity, []);
  return null;
}
